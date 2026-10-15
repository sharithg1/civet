//! Shared helpers for the integration tests.
#![allow(dead_code)]

use std::sync::Arc;

use civet_proto::TermSize;
use civet_term::{EncodedUpdate, PaneTerminal, Snapshot, TermConfig};

pub fn term(cols: u16, rows: u16) -> PaneTerminal {
    PaneTerminal::new(TermConfig {
        size: TermSize::new(cols, rows),
        scrollback_limit: 10_000,
    })
}

pub fn term_with_limit(cols: u16, rows: u16, limit: usize) -> PaneTerminal {
    PaneTerminal::new(TermConfig {
        size: TermSize::new(cols, rows),
        scrollback_limit: limit,
    })
}

/// Apply an update the way a client does: resize first, then write.
pub fn apply(b: &mut PaneTerminal, upd: &EncodedUpdate) {
    if b.size().cols != upd.size.cols || b.size().rows != upd.size.rows {
        b.resize(upd.size).expect("resize");
    }
    b.feed(&upd.payload);
}

/// Fresh client terminal brought to `snap` by a Full update.
pub fn client_from_full(a: &PaneTerminal, snap: &Snapshot) -> PaneTerminal {
    let upd = civet_term::encode_full(snap, a.scrollback());
    let mut b = term(snap.size().cols, snap.size().rows);
    apply(&mut b, &upd);
    b
}

#[track_caller]
pub fn assert_same(expected: &Snapshot, actual: &Snapshot, ctx: &str) {
    if let Some(d) = expected.first_difference(actual) {
        panic!(
            "{ctx}: screens differ: {d}\nexpected:\n{}\nactual:\n{}",
            expected.screen_text().join("\n"),
            actual.screen_text().join("\n")
        );
    }
    assert_eq!(
        expected.alt_screen(),
        actual.alt_screen(),
        "{ctx}: alt screen"
    );
}

/// Renders history-line VT into cells, so lines can be compared by what
/// they display rather than by their exact encoding (a soft-wrapped line is
/// encoded at full width, a hard-terminated one with trailing blanks
/// trimmed).
pub struct LineRenderer(PaneTerminal);

impl LineRenderer {
    pub const WIDTH: u16 = 512;

    pub fn new() -> Self {
        Self(term(Self::WIDTH, 1))
    }

    pub fn cells(&mut self, vt: &[u8]) -> Vec<civet_term::Cell> {
        self.0.feed(b"\x1b[0m\x1b[H\x1b[2K");
        self.0.feed(vt);
        let s = self.0.snapshot();
        (0..Self::WIDTH)
            .map(|c| s.cell(0, c).cloned().unwrap_or_default())
            .collect()
    }
}

/// Plain text of a history line (strips escape sequences).
pub fn line_text(vt: &[u8]) -> String {
    let s = String::from_utf8_lossy(vt);
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn snap(t: &mut PaneTerminal) -> Arc<Snapshot> {
    t.snapshot()
}
