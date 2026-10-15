//! Second VT parser run over the same bytes as alacritty, for things
//! alacritty_terminal does not expose in order: the title (OSC 0/2, title
//! stack, RIS), notifications (OSC 9 / 777), shell-integration marks
//! (OSC 133), the working directory (OSC 7) and XTVERSION.

use alacritty_terminal::vte::{Params, Parser, Perform};

use crate::types::{OscNotification, PromptMark};

const TITLE_STACK_MAX: usize = 4096;
const TITLE_MAX_BYTES: usize = 4096;

/// Parser plus the state it maintains.
pub(crate) struct SideParser {
    parser: Parser,
    pub(crate) st: SideState,
}

impl std::fmt::Debug for SideParser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SideParser")
            .field("st", &self.st)
            .finish_non_exhaustive()
    }
}

/// What the side parser tracks / collects.
#[derive(Debug, Default)]
pub(crate) struct SideState {
    pub(crate) title: String,
    title_stack: Vec<String>,
    pub(crate) notifications: Vec<OscNotification>,
    pub(crate) prompt_marks: Vec<PromptMark>,
    pub(crate) cwd: Option<String>,
    pub(crate) replies: Vec<u8>,
}

impl SideParser {
    pub(crate) fn new() -> Self {
        Self {
            parser: Parser::new(),
            st: SideState::default(),
        }
    }

    pub(crate) fn advance(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.st, bytes);
    }
}

fn sanitize(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let mut out: String = s
        .chars()
        .filter(|c| !c.is_control())
        .take(TITLE_MAX_BYTES)
        .collect();
    while out.len() > TITLE_MAX_BYTES {
        out.pop();
    }
    out
}

fn join(params: &[&[u8]]) -> Vec<u8> {
    let mut v = Vec::new();
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            v.push(b';');
        }
        v.extend_from_slice(p);
    }
    v
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// `file://host/path` → `/path` (percent-decoded). Plain absolute paths are
/// accepted as-is.
fn parse_cwd(raw: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(raw);
    if let Some(rest) = s.strip_prefix("file://") {
        let path = &rest[rest.find('/')?..];
        Some(percent_decode(path))
    } else if s.starts_with('/') {
        Some(percent_decode(&s))
    } else {
        None
    }
}

impl SideState {
    fn set_title(&mut self, t: String) {
        self.title = t;
    }
}

impl Perform for SideState {
    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        let Some(&kind) = params.first() else {
            return;
        };
        match kind {
            b"0" | b"2" => {
                let t = sanitize(&join(&params[1..]));
                self.set_title(t);
            }
            b"9" => {
                if params.len() < 2 {
                    return;
                }
                // ConEmu extensions (`OSC 9 ; 4 ; state ; progress` etc.) are
                // not notifications.
                let sub = params[1];
                if params.len() >= 3 && !sub.is_empty() && sub.iter().all(u8::is_ascii_digit) {
                    return;
                }
                self.notifications.push(OscNotification {
                    title: None,
                    body: sanitize(&join(&params[1..])),
                });
            }
            b"777" => {
                if params.get(1) == Some(&&b"notify"[..]) {
                    let title = params.get(2).map(|t| sanitize(t));
                    let body = if params.len() > 3 {
                        sanitize(&join(&params[3..]))
                    } else {
                        String::new()
                    };
                    self.notifications.push(OscNotification { title, body });
                }
            }
            b"133" => {
                let Some(sub) = params.get(1) else {
                    return;
                };
                let mark = match sub.first() {
                    Some(b'A') => PromptMark::PromptStart,
                    Some(b'B') => PromptMark::CommandStart,
                    Some(b'C') => PromptMark::CommandExecuted,
                    Some(b'D') => PromptMark::CommandFinished {
                        exit_code: params
                            .get(2)
                            .and_then(|p| std::str::from_utf8(p).ok())
                            .and_then(|p| p.trim().parse().ok()),
                    },
                    _ => return,
                };
                self.prompt_marks.push(mark);
            }
            b"7" => {
                if let Some(p) = parse_cwd(&join(&params[1..])) {
                    self.cwd = Some(p);
                }
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        match (action, intermediates) {
            ('t', []) => {
                let mut it = params.iter().map(|p| p.first().copied().unwrap_or(0));
                let op = it.next().unwrap_or(0);
                let which = it.next().unwrap_or(0);
                if which != 0 && which != 2 {
                    return;
                }
                match op {
                    22 => {
                        if self.title_stack.len() >= TITLE_STACK_MAX {
                            self.title_stack.remove(0);
                        }
                        self.title_stack.push(self.title.clone());
                    }
                    23 => {
                        if let Some(t) = self.title_stack.pop() {
                            self.set_title(t);
                        }
                    }
                    _ => {}
                }
            }
            ('q', [b'>']) => {
                // XTVERSION.
                self.replies.extend_from_slice(
                    format!("\x1bP>|civet {}\x1b\\", env!("CARGO_PKG_VERSION")).as_bytes(),
                );
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        if intermediates.is_empty() && byte == b'c' {
            // RIS.
            self.title.clear();
            self.title_stack.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cwd_parsing() {
        assert_eq!(
            parse_cwd(b"file://host/home/a%20b").as_deref(),
            Some("/home/a b")
        );
        assert_eq!(parse_cwd(b"/tmp").as_deref(), Some("/tmp"));
        assert_eq!(parse_cwd(b"nope"), None);
    }
}
