//! Local-echo prediction (docs/INTERFACES.md §6.6, ARCHITECTURE.md
//! "Local echo prediction"), after mosh.
//!
//! One [`Predictor`] per attached pane. It keeps a client-side mirror
//! [`PaneTerminal`] fed with every server payload, so it always knows the
//! true screen at the pane's current server state. On input it guesses the
//! effect of printable characters, Backspace and cursor Left/Right on the
//! primary screen, and returns an overlay (a kind-4 record payload) drawing
//! the guesses underlined. Before every server payload it emits "undo" bytes
//! that restore the predicted cells from the mirror (a diff assumes the
//! client shows exactly its base state), then the payload, then redraws the
//! guesses that are still pending. xterm.js never knows predictions exist.
//!
//! A guess is retired once the daemon acknowledged its keystroke
//! (`acked_input_seq`, echo-ack rule) and the mirror agrees. If an
//! acknowledged guess disagrees with the mirror (password prompt, line
//! editor doing something else) all guesses are rolled back and the pane
//! turns *tentative*: guesses are still tracked but not shown until one is
//! confirmed again, so a password prompt never flashes characters.

use std::time::Duration;

use civet_proto::{FULL_REPLAY_LINES, ScreenUpdate, TermSize, UpdateKind};
use civet_term::{CellFlags, PaneTerminal, Snapshot, TermConfig, encode_cells, encode_full};

/// Show predictions only when the link RTT is above this (mosh "adaptive").
pub const DEFAULT_MIN_RTT: Duration = Duration::from_millis(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Glyph {
    Char(char),
    Erase,
}

#[derive(Debug, Clone, Copy)]
struct Prediction {
    /// Input seq of the keystroke.
    seq: u64,
    /// Predicted cell content, if the keystroke changes a cell.
    cell: Option<(u16, u16, Glyph)>,
    /// Predicted cursor (row, col) after the keystroke.
    cursor: (u16, u16),
    /// False if the same input chunk also contained unpredictable bytes, so
    /// the cursor after the whole seq cannot be checked.
    exact: bool,
}

/// Per-pane predictor and client-side screen mirror.
pub struct Predictor {
    mirror: PaneTerminal,
    has_screen: bool,
    preds: Vec<Prediction>,
    /// Cells currently showing predicted content on the client.
    drawn: Vec<(u16, u16)>,
    /// The client cursor sits at a predicted position.
    cursor_moved: bool,
    /// An unpredictable keystroke was sent; no new guesses until the daemon
    /// acknowledged this seq (the mirror then reflects its effect).
    blocked_until: Option<u64>,
    /// Last guesses were wrong: track but do not show until one is right.
    tentative: bool,
}

impl std::fmt::Debug for Predictor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Predictor")
            .field("size", &self.mirror.size())
            .field("pending", &self.preds.len())
            .field("drawn", &self.drawn.len())
            .field("tentative", &self.tentative)
            .field("blocked_until", &self.blocked_until)
            .finish()
    }
}

fn cup(out: &mut Vec<u8>, row: u16, col: u16) {
    out.extend_from_slice(
        format!("\x1b[{};{}H", u32::from(row) + 1, u32::from(col) + 1).as_bytes(),
    );
}

fn predictable_char(c: char) -> bool {
    (' '..='~').contains(&c) || ('\u{a0}'..='\u{2ff}').contains(&c)
}

impl Predictor {
    /// Mirror of a pane of the given size (replaced by the first Full).
    pub fn new(size: TermSize) -> Self {
        let mut mirror = PaneTerminal::new(TermConfig {
            size: size.sanitized(),
            scrollback_limit: FULL_REPLAY_LINES as usize,
        });
        mirror.set_snapshot_retention(1);
        Self {
            mirror,
            has_screen: false,
            preds: Vec::new(),
            drawn: Vec::new(),
            cursor_moved: false,
            blocked_until: None,
            tentative: false,
        }
    }

    /// The true screen as of the last server update.
    pub fn mirror(&self) -> &PaneTerminal {
        &self.mirror
    }

    /// A server update has been applied at least once.
    pub fn has_screen(&self) -> bool {
        self.has_screen
    }

    /// Number of unconfirmed guesses.
    pub fn pending(&self) -> usize {
        self.preds.len()
    }

    /// Guesses are tracked but hidden after a misprediction.
    pub fn is_tentative(&self) -> bool {
        self.tentative
    }

    /// Apply a server update to the mirror and return the bytes to forward
    /// to xterm.js: undo of drawn guesses, the server payload, redraw of the
    /// guesses still pending.
    pub fn apply_server(&mut self, u: &ScreenUpdate) -> Vec<u8> {
        let resized = u.size.sanitized() != self.mirror.size();
        let repaint = u.kind == UpdateKind::Full || resized || !self.has_screen;
        let mut out = Vec::with_capacity(u.payload.len() + 64);
        if repaint {
            // Full starts with RIS and a resize repaints fully: the guesses
            // are wiped on the client anyway.
            self.forget_drawn();
        } else {
            out.extend(self.undo());
        }
        if resized {
            let _ = self.mirror.resize(u.size.sanitized());
        }
        self.mirror.feed(&u.payload);
        self.has_screen = true;
        out.extend_from_slice(&u.payload);
        if repaint || self.mirror.alt_screen() {
            self.preds.clear();
            self.blocked_until = None;
            return out;
        }
        self.reconcile(u.acked_input_seq);
        if !self.tentative {
            let preds = self.preds.clone();
            out.extend(self.draw(&preds));
        }
        out
    }

    /// The daemon applied input up to `acked` without a screen update
    /// carrying it (`InputAck`). Returns an overlay if guesses were retired
    /// or rolled back.
    pub fn on_ack(&mut self, acked: u64) -> Option<Vec<u8>> {
        if !self.preds.iter().any(|p| p.seq <= acked) {
            if self.blocked_until.is_some_and(|b| acked >= b) {
                self.blocked_until = None;
            }
            return None;
        }
        let mut out = self.undo();
        self.reconcile(acked);
        if !self.tentative {
            let preds = self.preds.clone();
            out.extend(self.draw(&preds));
        }
        (!out.is_empty()).then_some(out)
    }

    /// Input `data` was sent with `seq`. `show` = prediction enabled and
    /// the RTT is high enough to bother. Returns an overlay (kind-4 record
    /// payload) if new guesses are visible.
    pub fn on_input(&mut self, seq: u64, data: &[u8], show: bool) -> Option<Vec<u8>> {
        if !self.has_screen || self.mirror.alt_screen() {
            return None;
        }
        if !show || self.blocked_until.is_some() {
            if !self.preds.is_empty() || self.blocked_until.is_some() {
                // Unpredicted input after pending guesses: cursor unknown
                // until the daemon caught up.
                self.blocked_until = Some(seq);
            }
            return None;
        }
        let snap = self.mirror.snapshot();
        let size = snap.size();
        let c = snap.cursor();
        let mut cur = self.preds.last().map_or((c.row, c.col), |p| p.cursor);
        let mut new: Vec<Prediction> = Vec::new();
        let mut blocked = false;
        let text = std::str::from_utf8(data).unwrap_or("\u{0}");
        let mut chars = text.char_indices().peekable();
        while let Some((i, ch)) = chars.next() {
            let rest = &text[i..];
            let ok = match ch {
                '\x1b' => {
                    let left = rest.starts_with("\x1b[D") || rest.starts_with("\x1bOD");
                    let right = rest.starts_with("\x1b[C") || rest.starts_with("\x1bOC");
                    // Consume the two bytes after ESC.
                    chars.next();
                    chars.next();
                    if left && cur.1 > 0 {
                        cur.1 -= 1;
                        new.push(Prediction {
                            seq,
                            cell: None,
                            cursor: cur,
                            exact: true,
                        });
                        true
                    } else if right
                        && cur.1 + 1 < size.cols
                        && !self.blank_at(&snap, &new, cur.0, cur.1)
                    {
                        cur.1 += 1;
                        new.push(Prediction {
                            seq,
                            cell: None,
                            cursor: cur,
                            exact: true,
                        });
                        true
                    } else {
                        false
                    }
                }
                '\x7f' | '\x08' => {
                    if cur.1 > 0 && self.tail_blank(&snap, &new, cur.0, cur.1, size.cols) {
                        cur.1 -= 1;
                        new.push(Prediction {
                            seq,
                            cell: Some((cur.0, cur.1, Glyph::Erase)),
                            cursor: cur,
                            exact: true,
                        });
                        true
                    } else {
                        false
                    }
                }
                // Only at the end of the line (no insert-mode shifting to
                // guess) and never into the last column (autowrap).
                ch if predictable_char(ch)
                    && cur.1 + 1 < size.cols
                    && self.tail_blank(&snap, &new, cur.0, cur.1, size.cols) =>
                {
                    new.push(Prediction {
                        seq,
                        cell: Some((cur.0, cur.1, Glyph::Char(ch))),
                        cursor: (cur.0, cur.1 + 1),
                        exact: true,
                    });
                    cur.1 += 1;
                    true
                }
                _ => false,
            };
            if !ok {
                blocked = true;
                break;
            }
        }
        if blocked {
            self.blocked_until = Some(seq);
            for p in &mut new {
                p.exact = false;
            }
        }
        if new.is_empty() {
            return None;
        }
        self.preds.extend_from_slice(&new);
        if self.tentative {
            return None;
        }
        let out = self.draw(&new);
        (!out.is_empty()).then_some(out)
    }

    /// Drop every guess (prediction turned off). Returns undo bytes if any
    /// guess was visible.
    pub fn clear(&mut self) -> Option<Vec<u8>> {
        self.preds.clear();
        self.blocked_until = None;
        self.tentative = false;
        let out = self.undo();
        (!out.is_empty()).then_some(out)
    }

    /// Full repaint of the mirror (plus visible guesses) for a re-attached
    /// view. Returns the payload and how many history lines it replays.
    pub fn full_repaint(&mut self) -> (Vec<u8>, u64) {
        let snap = self.mirror.snapshot();
        let enc = encode_full(&snap, self.mirror.scrollback());
        let replayed = enc.history_lines.saturating_sub(enc.history_start);
        let mut payload = enc.payload;
        self.forget_drawn();
        if !self.tentative {
            let preds = self.preds.clone();
            payload.extend(self.draw(&preds));
        }
        (payload, replayed)
    }

    fn forget_drawn(&mut self) {
        self.drawn.clear();
        self.cursor_moved = false;
    }

    fn effective(
        &self,
        snap: &Snapshot,
        extra: &[Prediction],
        row: u16,
        col: u16,
    ) -> Option<Glyph> {
        self.preds
            .iter()
            .chain(extra)
            .rev()
            .find_map(|p| match p.cell {
                Some((r, c, g)) if r == row && c == col => Some(g),
                _ => None,
            })
            .or_else(|| {
                snap.cell(row, col).map(|cell| {
                    if cell.ch == ' ' && !cell.flags.contains(CellFlags::WIDE_SPACER) {
                        Glyph::Erase
                    } else {
                        Glyph::Char(cell.ch)
                    }
                })
            })
    }

    fn blank_at(&self, snap: &Snapshot, extra: &[Prediction], row: u16, col: u16) -> bool {
        matches!(self.effective(snap, extra, row, col), Some(Glyph::Erase))
    }

    /// Everything from `col` to the end of the row is blank: typing appends
    /// (no insert-mode shifting to guess).
    fn tail_blank(
        &self,
        snap: &Snapshot,
        extra: &[Prediction],
        row: u16,
        col: u16,
        cols: u16,
    ) -> bool {
        (col..cols).all(|c| self.blank_at(snap, extra, row, c))
    }

    /// Retire guesses covered by `acked`; roll back on any mismatch.
    fn reconcile(&mut self, acked: u64) {
        // Unpredicted input (after the guesses) was processed too: the screen
        // may legitimately differ from the guesses, so they cannot be
        // checked. Retire them silently.
        let unverifiable = self.blocked_until.is_some_and(|b| acked >= b);
        if unverifiable {
            self.blocked_until = None;
        }
        let Some(last) = self.preds.iter().rposition(|p| p.seq <= acked) else {
            return;
        };
        if unverifiable {
            self.preds.drain(..=last);
            return;
        }
        let snap = self.mirror.snapshot();
        let mut failed = false;
        let mut confirmed = false;
        let mut seen: Vec<(u16, u16)> = Vec::new();
        // Newest guess per cell wins (typing then Backspace on one cell).
        for p in self.preds[..=last].iter().rev() {
            if let Some((r, c, g)) = p.cell {
                if seen.contains(&(r, c)) {
                    continue;
                }
                seen.push((r, c));
                let truth = snap.cell(r, c).map(|cell| cell.ch);
                let ok = match g {
                    Glyph::Char(ch) => truth == Some(ch),
                    Glyph::Erase => truth == Some(' '),
                };
                failed |= !ok;
                confirmed |= ok;
            }
        }
        let p = self.preds[last];
        if p.exact && p.seq == acked {
            let cur = snap.cursor();
            if (cur.row, cur.col) == p.cursor {
                confirmed = true;
            } else {
                failed = true;
            }
        }
        if failed {
            tracing::debug!(acked, "misprediction; rolling back");
            self.preds.clear();
            self.blocked_until = None;
            self.tentative = true;
        } else {
            if confirmed {
                self.tentative = false;
            }
            self.preds.drain(..=last);
        }
    }

    /// Restore every drawn cell and the cursor from the mirror.
    fn undo(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.drawn.is_empty() && !self.cursor_moved {
            return out;
        }
        let snap = self.mirror.snapshot();
        let mut cells = std::mem::take(&mut self.drawn);
        cells.sort_unstable();
        cells.dedup();
        for (r, c) in cells {
            out.extend(encode_cells(&snap, r, c..c + 1));
        }
        let cur = snap.cursor();
        cup(&mut out, cur.row, cur.col);
        self.cursor_moved = false;
        out
    }

    /// Draw `preds` (underlined guesses) and move the cursor to the last
    /// predicted position.
    fn draw(&mut self, preds: &[Prediction]) -> Vec<u8> {
        let mut out = Vec::new();
        let Some(last) = preds.last() else {
            return out;
        };
        for p in preds {
            if let Some((r, c, g)) = p.cell {
                cup(&mut out, r, c);
                match g {
                    Glyph::Char(ch) => {
                        out.extend_from_slice(b"\x1b[0;4m");
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                    }
                    Glyph::Erase => out.extend_from_slice(b"\x1b[0m "),
                }
                self.drawn.push((r, c));
            }
        }
        if !out.is_empty() {
            out.extend_from_slice(b"\x1b[0m");
        }
        cup(&mut out, last.cursor.0, last.cursor.1);
        self.cursor_moved = true;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use civet_proto::PaneId;
    use civet_term::Snapshot;
    use std::sync::Arc;

    /// A "daemon" terminal producing real Full/Diff updates, and a "client"
    /// terminal standing in for xterm.js that receives every forwarded byte.
    struct Rig {
        server: PaneTerminal,
        sent: Option<Arc<Snapshot>>,
        client: PaneTerminal,
        p: Predictor,
    }

    impl Rig {
        fn new(initial: &str) -> Self {
            let size = TermSize::new(40, 5);
            let cfg = TermConfig {
                size,
                scrollback_limit: 100,
            };
            let mut server = PaneTerminal::new(cfg.clone());
            server.feed(initial.as_bytes());
            let mut rig = Self {
                server,
                sent: None,
                client: PaneTerminal::new(cfg),
                p: Predictor::new(size),
            };
            rig.server_update(0);
            rig
        }

        /// Server emits the update since the last sent state.
        fn server_update(&mut self, acked: u64) {
            let enc = self.server.encode_update(self.sent.as_deref());
            self.sent = Some(self.server.snapshot());
            let u = enc.into_screen_update(PaneId(1), acked);
            let out = self.p.apply_server(&u);
            self.client.feed(&out);
        }

        fn type_(&mut self, seq: u64, s: &str) -> Option<Vec<u8>> {
            let o = self.p.on_input(seq, s.as_bytes(), true);
            if let Some(o) = &o {
                self.client.feed(o);
            }
            o
        }

        fn ack(&mut self, seq: u64) {
            if let Some(o) = self.p.on_ack(seq) {
                self.client.feed(&o);
            }
        }

        fn client_matches_server(&mut self) {
            let c = self.client.snapshot();
            let s = self.server.snapshot();
            assert_eq!(c.screen_text(), s.screen_text());
            assert_eq!(c.cursor(), s.cursor());
            for row in 0..s.size().rows {
                for col in 0..s.size().cols {
                    assert_eq!(
                        c.cell(row, col).unwrap().flags,
                        s.cell(row, col).unwrap().flags,
                        "attributes at {row},{col}"
                    );
                }
            }
        }

        fn client_row(&self, row: usize) -> String {
            self.client.screen_text()[row].clone()
        }

        fn underlined(&mut self, row: u16, col: u16) -> bool {
            self.client
                .snapshot()
                .cell(row, col)
                .unwrap()
                .flags
                .contains(CellFlags::UNDERLINE)
        }
    }

    #[test]
    fn predicts_typing_and_confirms_on_echo() {
        let mut r = Rig::new("$ ");
        let o = r.type_(1, "ls").expect("overlay");
        assert!(String::from_utf8_lossy(&o).contains("\x1b[0;4ml"));
        assert_eq!(r.client_row(0), "$ ls");
        assert!(r.underlined(0, 2) && r.underlined(0, 3));
        assert_eq!(r.client.cursor().col, 4);
        assert_eq!(r.p.pending(), 2);

        // Echo of "l" arrives but the daemon has not acked yet: keep both.
        r.server.feed(b"l");
        r.server_update(0);
        assert_eq!(r.p.pending(), 2);
        assert_eq!(r.client_row(0), "$ ls");
        assert!(r.underlined(0, 3));

        // Full echo, acked: guesses retire, client equals the server exactly
        // (no underline left).
        r.server.feed(b"s");
        r.server_update(1);
        assert_eq!(r.p.pending(), 0);
        assert!(!r.p.is_tentative());
        r.client_matches_server();
    }

    #[test]
    fn rolls_back_when_not_echoed() {
        let mut r = Rig::new("Password: ");
        r.type_(1, "x").expect("first guess is shown");
        assert_eq!(r.client_row(0), "Password: x");
        // Daemon applied the keystroke, nothing was echoed.
        r.ack(1);
        assert_eq!(r.client_row(0), "Password:");
        r.client_matches_server();
        assert!(r.p.is_tentative());
        // Further keystrokes are tracked but never shown.
        assert!(r.type_(2, "y").is_none());
        r.ack(2);
        assert!(r.type_(3, "z").is_none());
        r.client_matches_server();
    }

    #[test]
    fn rollback_on_mismatching_echo_then_recovers() {
        let mut r = Rig::new("$ ");
        r.type_(1, "a");
        // The line editor echoed something else.
        r.server.feed(b"A");
        r.server_update(1);
        assert!(r.p.is_tentative());
        r.client_matches_server();
        // Next guess is hidden; once it is confirmed prediction resumes.
        assert!(r.type_(2, "b").is_none());
        r.server.feed(b"b");
        r.server_update(2);
        assert!(!r.p.is_tentative());
        assert!(r.type_(3, "c").is_some());
        assert_eq!(r.client_row(0), "$ Abc");
    }

    #[test]
    fn cursor_keys_and_backspace() {
        let mut r = Rig::new("$ ");
        r.type_(1, "abc");
        r.type_(2, "\x7f");
        assert_eq!(r.client_row(0), "$ ab");
        assert_eq!(r.client.cursor().col, 4);
        r.type_(3, "\x1b[D");
        assert_eq!(r.client.cursor().col, 3);
        r.type_(4, "\x1b[C");
        assert_eq!(r.client.cursor().col, 4);
        // Right at the end of the text is not predicted (shells refuse it).
        assert!(r.type_(5, "\x1b[C").is_none());
        // ... and blocks further guesses until the daemon caught up.
        assert!(r.type_(6, "z").is_none());
        // Server: "abc", BS, left, right, (refused right), "z".
        r.server.feed(b"abc\x08 \x08\x1b[D\x1b[Cz");
        r.server_update(6);
        assert_eq!(r.p.pending(), 0);
        r.client_matches_server();
        assert_eq!(r.client_row(0), "$ abz");
    }

    #[test]
    fn backspace_over_a_guess_is_verified_per_cell() {
        let mut r = Rig::new("$ ");
        r.type_(1, "abc");
        r.type_(2, "\x7f");
        r.server.feed(b"abc\x08 \x08");
        r.server_update(2);
        assert_eq!(r.p.pending(), 0);
        assert!(!r.p.is_tentative());
        r.client_matches_server();
    }

    #[test]
    fn no_prediction_in_alt_screen_or_when_disabled() {
        let mut r = Rig::new("\x1b[?1049h\x1b[Hvim");
        assert!(r.p.mirror().alt_screen());
        assert!(r.type_(1, "i").is_none());
        assert_eq!(r.p.pending(), 0);

        let mut r = Rig::new("$ ");
        assert!(r.p.on_input(1, b"x", false).is_none());
        assert_eq!(r.p.pending(), 0);
    }

    #[test]
    fn control_input_blocks_until_acked() {
        let mut r = Rig::new("$ ");
        assert!(r.type_(1, "\r").is_none());
        assert!(r.type_(2, "x").is_none(), "blocked");
        r.server.feed(b"\r\n$ x");
        r.server_update(2);
        assert!(r.type_(3, "y").is_some());
        r.server.feed(b"y");
        r.server_update(3);
        r.client_matches_server();
    }

    #[test]
    fn full_update_drops_guesses_and_disable_undoes() {
        let mut r = Rig::new("$ ");
        r.type_(1, "q");
        let undo = r.p.clear().expect("undo bytes");
        r.client.feed(&undo);
        r.client_matches_server();
        r.type_(2, "w");
        r.sent = None; // force a Full
        r.server_update(0);
        assert_eq!(r.p.pending(), 0);
        r.client_matches_server();
    }

    #[test]
    fn full_repaint_reproduces_the_mirror() {
        let mut r = Rig::new("hello\r\nworld\x1b[1;31m!\x1b[0m");
        let (payload, _) = r.p.full_repaint();
        let mut fresh = PaneTerminal::new(TermConfig {
            size: TermSize::new(40, 5),
            scrollback_limit: 10,
        });
        fresh.feed(&payload);
        assert_eq!(fresh.screen_text(), r.server.screen_text());
        assert_eq!(fresh.cursor(), r.server.cursor());
    }
}
