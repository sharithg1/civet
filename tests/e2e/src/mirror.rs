//! Client-side copy of a pane, fed with the daemon's payloads exactly as
//! xterm.js would be, plus a server-numbered transcript of every history
//! line that was replayed into it (to check "no lost output").

use std::collections::BTreeMap;

use civet_proto::{ScreenUpdate, TermSize, UpdateKind};
use civet_term::{PaneTerminal, TermConfig};

/// Strip CSI / OSC escape sequences from a VT line.
pub fn strip_vt(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('[') => {
                for c in it.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = it.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' {
                        it.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// One pane as the client sees it.
#[derive(Debug)]
pub struct Mirror {
    /// The emulator the payloads are played into.
    pub term: PaneTerminal,
    /// Last applied server state.
    pub state: u64,
    /// Full updates applied.
    pub fulls: usize,
    /// Diff updates applied.
    pub diffs: usize,
    /// Diffs whose base did not match (must stay 0: the session filters).
    pub base_mismatches: usize,
    /// Payload bytes received.
    pub bytes: u64,
    /// Server-numbered history lines replayed into this mirror (stripped
    /// text), if transcript tracking is on.
    pub transcript: Option<BTreeMap<u64, String>>,
    /// Updates whose replayed line count did not match the header.
    pub replay_mismatches: usize,
    /// `[start, end)` ranges of history that were *not* replayed (only
    /// available via `fetch_scrollback`).
    pub gaps: Vec<(u64, u64)>,
    /// Server `history_lines` after the last update.
    pub history_lines: u64,
    /// `(state, base_state, history_start, history_lines)` of every update
    /// (diagnostics).
    pub headers: Vec<(u64, u64, u64, u64)>,
}

impl Mirror {
    /// New mirror; `transcript` enables history tracking (costs CPU).
    pub fn new(transcript: bool) -> Self {
        let config = TermConfig {
            size: TermSize::new(80, 24),
            scrollback_limit: if transcript { 1_000_000 } else { 10_000 },
        };
        Self {
            term: PaneTerminal::new(config),
            state: 0,
            fulls: 0,
            diffs: 0,
            base_mismatches: 0,
            bytes: 0,
            transcript: transcript.then(BTreeMap::new),
            replay_mismatches: 0,
            gaps: Vec::new(),
            history_lines: 0,
            headers: Vec::new(),
        }
    }

    /// Apply one update (as the session delivered it).
    pub fn apply(&mut self, u: &ScreenUpdate) {
        match u.kind {
            UpdateKind::Diff => {
                if u.base_state != self.state {
                    self.base_mismatches += 1;
                }
                self.diffs += 1;
            }
            UpdateKind::Full => self.fulls += 1,
        }
        if u.size != self.term.size() {
            let _ = self.term.resize(u.size);
        }
        self.headers
            .push((u.state, u.base_state, u.history_start, u.history_lines));
        if u.history_start > self.history_lines {
            self.gaps.push((self.history_lines, u.history_start));
        }
        let before = self.term.scrollback().end();
        self.term.feed(&u.payload);
        let after = self.term.scrollback().end();
        if let Some(t) = &mut self.transcript {
            let replayed = u.history_lines.saturating_sub(u.history_start);
            if after - before == replayed {
                for (i, (_, line)) in self.term.scrollback().range(before, after).enumerate() {
                    t.insert(u.history_start + i as u64, strip_vt(&line.vt));
                }
            } else {
                self.replay_mismatches += 1;
            }
        }
        self.bytes += u.payload.len() as u64;
        self.state = u.state;
        self.history_lines = self.history_lines.max(u.history_lines);
    }

    /// Visible rows, trailing spaces trimmed.
    pub fn rows(&self) -> Vec<String> {
        self.term.screen_text()
    }

    /// The shell's `$ ` prompt is on screen. Why: typing before the shell
    /// reads leaves the tty's echo of the command above the prompt and its
    /// output on the prompt row (`$ hello`), which a row match never sees;
    /// dash on Linux starts slowly enough to hit this. Matches `$` (PS1) and
    /// macOS's `sh-3.2$` (bash 3.2 as sh ignores PS1 from the environment).
    pub fn has_prompt(&self) -> bool {
        self.rows().iter().any(|r| r.trim_end().ends_with('$'))
    }

    /// Any visible row equal to `row` (trailing spaces ignored).
    pub fn has_row(&self, row: &str) -> bool {
        self.rows().iter().any(|r| r.trim_end() == row)
    }

    /// Number of visible rows equal to `row`.
    pub fn count_rows(&self, row: &str) -> usize {
        self.rows().iter().filter(|r| r.trim_end() == row).count()
    }

    /// History numbers in `0..history_lines` missing from the transcript,
    /// as `[start, end)` ranges.
    pub fn missing_ranges(&self) -> Vec<(u64, u64)> {
        let Some(t) = &self.transcript else {
            return vec![(0, self.history_lines)];
        };
        let mut out = Vec::new();
        let mut start = None;
        for n in 0..self.history_lines {
            match (t.contains_key(&n), start) {
                (false, None) => start = Some(n),
                (true, Some(s)) => {
                    out.push((s, n));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            out.push((s, self.history_lines));
        }
        out
    }
}

/// Visible rows with trailing blank rows removed.
pub fn trim_rows(rows: &[String]) -> Vec<String> {
    let mut v: Vec<String> = rows.iter().map(|r| r.trim_end().to_string()).collect();
    while v.last().is_some_and(|r| r.is_empty()) {
        v.pop();
    }
    v
}

/// Assert `lines` contains `expected` as one contiguous run, exactly once.
pub fn assert_contiguous(lines: &[String], expected: &[String]) {
    assert!(!expected.is_empty());
    let starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.trim_end() == expected[0])
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        starts.len(),
        1,
        "first expected line {:?} must appear exactly once (found {})",
        expected[0],
        starts.len()
    );
    let start = starts[0];
    for (i, want) in expected.iter().enumerate() {
        let got = lines.get(start + i).map(|l| l.trim_end());
        assert_eq!(
            got,
            Some(want.as_str()),
            "line {} of the expected run (context: {:?})",
            i,
            &lines[start + i.saturating_sub(3)..(start + i + 3).min(lines.len())]
        );
    }
    for want in expected {
        let n = lines.iter().filter(|l| l.trim_end() == want).count();
        assert_eq!(n, 1, "{want:?} appears {n} times (duplicated output)");
    }
}
