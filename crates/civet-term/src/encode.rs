//! Snapshot → VT encoding: full repaints, diffs, scrollback lines.
//!
//! # Client invariant
//!
//! Every payload assumes the client terminal is in the state our previous
//! payload left it in: full scroll region, origin mode off, insert mode off,
//! G0 = ASCII, SGR reset, no open hyperlink, autowrap = the base snapshot's
//! `auto_wrap`. A `Full` payload establishes it with RIS. The client never
//! interprets raw PTY output, so nothing else can disturb it.

use std::sync::Arc;

use civet_proto::{
    FULL_REPLAY_LINES, MAX_REPLAY_LINES, PaneId, ScreenUpdate, TermSize, UpdateKind,
};

use crate::scrollback::ScrollbackStore;
use crate::snapshot::{Row, Snapshot};
use crate::types::{Cell, CellFlags, Color, CursorShape, MouseMode, TermModes};

/// Max bytes of history replay in one update. Replay that would exceed it
/// drops the *oldest* lines (`history_start` advances), leaving a gap the
/// client fills with a scrollback fetch, so one update always fits in a frame
/// ([`civet_proto::MAX_FRAME_LEN`]) no matter how heavy the history lines
/// are.
pub const MAX_REPLAY_BYTES: usize = 8 << 20;

/// Upper bound on the payload of any update produced by [`encode_full`] /
/// [`encode_diff`] (comfortably below [`civet_proto::MAX_FRAME_LEN`]). A diff
/// over it falls back to a full repaint; a full repaint over it drops the
/// replay, then repaints text only (no attributes, links or combining
/// characters) as a last resort.
pub const MAX_UPDATE_BYTES: usize = 12 << 20;

/// Framing bytes [`Writer::replay`] adds per line (`" \r"` or `"\r\n"`).
const REPLAY_LINE_OVERHEAD: usize = 2;

/// Max title bytes in a text-only fallback repaint.
const PLAIN_TITLE_BYTES: usize = 1024;

/// Unchanged cells between two changed runs that are cheaper to reprint than
/// to skip with a cursor move.
const GAP: usize = 3;

/// The result of encoding one screen update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedUpdate {
    /// Full repaint or diff.
    pub kind: UpdateKind,
    /// State this update brings the client to.
    pub state: u64,
    /// State the diff applies to (0 for Full).
    pub base_state: u64,
    /// Terminal size at `state`.
    pub size: TermSize,
    /// Absolute history counter at `state`.
    pub history_lines: u64,
    /// First history line replayed by `payload`.
    pub history_start: u64,
    /// VT bytes.
    pub payload: Vec<u8>,
}

impl EncodedUpdate {
    /// Wrap into the wire type.
    pub fn into_screen_update(self, pane: PaneId, acked_input_seq: u64) -> ScreenUpdate {
        ScreenUpdate {
            pane,
            state: self.state,
            base_state: self.base_state,
            kind: self.kind,
            acked_input_seq,
            size: self.size,
            history_lines: self.history_lines,
            history_start: self.history_start,
            payload: self.payload,
        }
    }
}

/// SGR rendition plus hyperlink.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pen {
    fg: Color,
    bg: Color,
    ul: Option<Color>,
    flags: CellFlags,
    link: Option<Arc<str>>,
}

impl Pen {
    const DEFAULT: Pen = Pen {
        fg: Color::Default,
        bg: Color::Default,
        ul: None,
        flags: CellFlags::EMPTY,
        link: None,
    };

    fn of(c: &Cell) -> Pen {
        Pen {
            fg: c.fg,
            bg: c.bg,
            ul: c.underline_color,
            flags: c.flags & CellFlags::SGR_MASK,
            link: c.hyperlink.clone(),
        }
    }

    fn same_sgr(&self, o: &Pen) -> bool {
        self.fg == o.fg && self.bg == o.bg && self.ul == o.ul && self.flags == o.flags
    }
}

const FLAG_CODES: [(CellFlags, &[u8]); 12] = [
    (CellFlags::BOLD, b"1"),
    (CellFlags::DIM, b"2"),
    (CellFlags::ITALIC, b"3"),
    (CellFlags::UNDERLINE, b"4"),
    (CellFlags::DOUBLE_UNDERLINE, b"4:2"),
    (CellFlags::UNDERCURL, b"4:3"),
    (CellFlags::DOTTED_UNDERLINE, b"4:4"),
    (CellFlags::DASHED_UNDERLINE, b"4:5"),
    (CellFlags::BLINK, b"5"),
    (CellFlags::INVERSE, b"7"),
    (CellFlags::HIDDEN, b"8"),
    (CellFlags::STRIKETHROUGH, b"9"),
];

fn push_num(out: &mut Vec<u8>, n: u32) {
    if n >= 10 {
        push_num(out, n / 10);
    }
    out.push(b'0' + (n % 10) as u8);
}

/// Writes VT while tracking the client's pen and cursor.
struct Writer<'a> {
    out: &'a mut Vec<u8>,
    pen: Pen,
    /// Known cursor position; `col == cols` means "pending wrap".
    cur: Option<(u16, u16)>,
    cols: u16,
    rows: u16,
}

impl<'a> Writer<'a> {
    fn new(out: &'a mut Vec<u8>, size: TermSize) -> Self {
        Self {
            out,
            pen: Pen::DEFAULT,
            cur: None,
            cols: size.cols,
            rows: size.rows,
        }
    }

    fn raw(&mut self, b: &[u8]) {
        self.out.extend_from_slice(b);
    }

    fn csi_num(&mut self, n: u32, fin: &[u8]) {
        self.raw(b"\x1b[");
        push_num(self.out, n);
        self.raw(fin);
    }

    fn cup(&mut self, row: u16, col: u16) {
        self.raw(b"\x1b[");
        if row != 0 || col != 0 {
            push_num(self.out, u32::from(row) + 1);
            if col != 0 {
                self.out.push(b';');
                push_num(self.out, u32::from(col) + 1);
            }
        }
        self.out.push(b'H');
        self.cur = Some((row, col));
    }

    fn move_to(&mut self, row: u16, col: u16) {
        if let Some((r, c)) = self.cur {
            if r == row && c == col && c < self.cols {
                return;
            }
            if col == 0 && r == row {
                self.out.push(b'\r');
                self.cur = Some((row, 0));
                return;
            }
            if col == 0 && row == r + 1 {
                self.raw(b"\r\n");
                self.cur = Some((row, 0));
                return;
            }
            if r == row && c < self.cols && col > c {
                let n = col - c;
                if n == 1 {
                    self.raw(b"\x1b[C");
                } else {
                    self.csi_num(u32::from(n), b"C");
                }
                self.cur = Some((row, col));
                return;
            }
        }
        self.cup(row, col);
    }

    /// `short` = (base for 0-7, base for 8-15) for fg/bg; None for the
    /// underline color, which only has the extended forms.
    fn color(&mut self, c: Color, short: Option<(u32, u32)>, ext: u32) {
        match (c, short) {
            (Color::Default, _) => push_num(self.out, ext + 1),
            (Color::Indexed(i), Some((base, _))) if i < 8 => {
                push_num(self.out, base + u32::from(i))
            }
            (Color::Indexed(i), Some((_, bright))) if i < 16 => {
                push_num(self.out, bright + u32::from(i) - 8)
            }
            (Color::Indexed(i), _) => {
                push_num(self.out, ext);
                self.raw(b";5;");
                push_num(self.out, u32::from(i));
            }
            (Color::Rgb(r, g, b), _) => {
                push_num(self.out, ext);
                self.raw(b";2;");
                push_num(self.out, u32::from(r));
                self.out.push(b';');
                push_num(self.out, u32::from(g));
                self.out.push(b';');
                push_num(self.out, u32::from(b));
            }
        }
    }

    fn set_link(&mut self, link: &Option<Arc<str>>) {
        if &self.pen.link == link {
            return;
        }
        self.raw(b"\x1b]8;;");
        if let Some(uri) = link {
            for ch in uri.chars().filter(|c| !c.is_control()) {
                let mut buf = [0u8; 4];
                self.raw(ch.encode_utf8(&mut buf).as_bytes());
            }
        }
        self.raw(b"\x1b\\");
        self.pen.link = link.clone();
    }

    fn set_pen(&mut self, want: &Pen) {
        self.set_link(&want.link);
        if self.pen.same_sgr(want) {
            return;
        }
        let removed = self.pen.flags & !want.flags;
        self.raw(b"\x1b[");
        let mut first = true;
        let base = if removed.is_empty() {
            self.pen.clone()
        } else {
            self.out.push(b'0');
            first = false;
            Pen::DEFAULT
        };
        let mut sep = |w: &mut Self| {
            if !first {
                w.out.push(b';');
            }
            first = false;
        };
        let added = want.flags & !base.flags;
        for (f, code) in FLAG_CODES {
            if added.contains(f) {
                sep(self);
                self.raw(code);
            }
        }
        if want.fg != base.fg {
            sep(self);
            self.color(want.fg, Some((30, 90)), 38);
        }
        if want.bg != base.bg {
            sep(self);
            self.color(want.bg, Some((40, 100)), 48);
        }
        if want.ul != base.ul {
            sep(self);
            self.color(want.ul.unwrap_or(Color::Default), None, 58);
        }
        self.out.push(b'm');
        let link = self.pen.link.take();
        self.pen = Pen {
            link,
            ..want.clone()
        };
    }

    fn reset_pen(&mut self) {
        self.set_link(&None);
        if !self.pen.same_sgr(&Pen::DEFAULT) {
            self.raw(b"\x1b[0m");
            self.pen = Pen::DEFAULT;
        }
    }

    /// Print one (non-spacer) cell at the current position.
    fn print(&mut self, c: &Cell) {
        let p = &self.pen;
        if !(p.fg == c.fg
            && p.bg == c.bg
            && p.ul == c.underline_color
            && p.flags == c.flags & CellFlags::SGR_MASK
            && p.link == c.hyperlink)
        {
            self.set_pen(&Pen::of(c));
        }
        if c.ch.is_ascii_graphic() || c.ch == ' ' {
            self.out.push(c.ch as u8);
        } else {
            let ch = if c.ch.is_control() { ' ' } else { c.ch };
            let mut buf = [0u8; 4];
            self.raw(ch.encode_utf8(&mut buf).as_bytes());
        }
        if let Some(z) = &c.zerowidth {
            self.raw(z.as_bytes());
        }
        let w = if c.flags.contains(CellFlags::WIDE) {
            2
        } else {
            1
        };
        if let Some((r, col)) = self.cur {
            self.cur = Some((r, (col + w).min(self.cols)));
        }
    }

    /// Paint `cells[start..end]` of `row` (wide chars must not be split).
    fn paint(&mut self, row: u16, cells: &[Cell], start: usize, end: usize) {
        self.move_to(row, start as u16);
        for c in &cells[start..end] {
            if c.flags.contains(CellFlags::WIDE_SPACER) {
                continue;
            }
            self.print(c);
        }
    }

    /// Erase from `col` to end of line with background `bg`.
    fn erase_tail(&mut self, row: u16, col: u16, bg: Color) {
        let want = Pen { bg, ..Pen::DEFAULT };
        self.set_pen(&want);
        self.move_to(row, col);
        self.raw(b"\x1b[K");
    }

    /// Update one row from `old` to `new` (same width).
    fn diff_row(&mut self, row: u16, old: &[Cell], new: &[Cell]) {
        let n = new.len();
        // Tail of identical erased cells → EL.
        let mut t = n;
        if n > 0 && new[n - 1].is_erased() {
            let bg = new[n - 1].bg;
            while t > 0 && new[t - 1].is_erased() && new[t - 1].bg == bg {
                t -= 1;
            }
            // A short tail right after changed cells is cheaper to print.
            if n - t < 4 && t > 0 && old[t - 1] != new[t - 1] {
                t = n;
            }
        }
        let mut i = 0;
        while i < t {
            if old[i] == new[i] {
                i += 1;
                continue;
            }
            let mut start = i;
            let mut end = i + 1;
            let mut j = end;
            while j < t && j - end <= GAP {
                if old[j] != new[j] {
                    end = j + 1;
                }
                j += 1;
            }
            if start > 0 && new[start].flags.contains(CellFlags::WIDE_SPACER) {
                start -= 1;
            }
            if end < n && new[end - 1].flags.contains(CellFlags::WIDE) {
                end += 1;
            }
            self.paint(row, new, start, end);
            i = end;
        }
        if t < n && old[t..] != new[t..] {
            self.erase_tail(row, t as u16, new[n - 1].bg);
        }
    }

    fn home_clear(&mut self) {
        self.reset_pen();
        self.raw(b"\x1b[H\x1b[J");
        self.cur = Some((0, 0));
    }

    /// Push history lines into the client's scrollback from a blank screen
    /// with the cursor at home. Leaves the screen blank, cursor at
    /// `(rows - 1, 0)`.
    fn replay(&mut self, store: &ScrollbackStore, start: u64, end: u64) {
        let mut lines = store.range_with_cols(start, end).peekable();
        if lines.peek().is_none() {
            return;
        }
        self.reset_pen();
        let mut need_newline = false;
        while let Some((line, cols)) = lines.next() {
            if need_newline {
                // Trigger the soft wrap with the default pen so a scroll at
                // the bottom erases the new row with the default background
                // (BCE), then overwrite the placeholder.
                self.raw(b" \r");
            }
            self.raw(&line.vt);
            let continues = line.wrapped
                && cols == self.cols
                && lines
                    .peek()
                    .is_some_and(|(next, _)| next.vt.as_slice() != b"\x1b[0m");
            if continues {
                need_newline = true;
            } else {
                self.raw(b"\r\n");
                need_newline = false;
            }
        }
        if need_newline {
            self.raw(b"\r\n");
        }
        for _ in 1..self.rows {
            self.out.push(b'\n');
        }
        self.cur = Some((self.rows - 1, 0));
    }

    fn private_mode(&mut self, code: u32, on: bool) {
        self.raw(b"\x1b[?");
        push_num(self.out, code);
        self.out.push(if on { b'h' } else { b'l' });
    }

    /// Set/reset client modes that differ (alt screen and autowrap are
    /// handled by the caller).
    fn modes(&mut self, from: &TermModes, to: &TermModes) {
        if from.app_cursor != to.app_cursor {
            self.private_mode(1, to.app_cursor);
        }
        if from.app_keypad != to.app_keypad {
            self.raw(if to.app_keypad { b"\x1b=" } else { b"\x1b>" });
        }
        if from.bracketed_paste != to.bracketed_paste {
            self.private_mode(2004, to.bracketed_paste);
        }
        if from.mouse != to.mouse {
            let code = |m: MouseMode| match m {
                MouseMode::Off => 0,
                MouseMode::Click => 1000,
                MouseMode::Drag => 1002,
                MouseMode::Motion => 1003,
            };
            if to.mouse == MouseMode::Off {
                self.private_mode(code(from.mouse), false);
            } else {
                self.private_mode(code(to.mouse), true);
            }
        }
        if from.mouse_sgr != to.mouse_sgr {
            self.private_mode(1006, to.mouse_sgr);
        }
        if from.focus_events != to.focus_events {
            self.private_mode(1004, to.focus_events);
        }
        if from.reverse_video != to.reverse_video {
            self.private_mode(5, to.reverse_video);
        }
    }

    fn title(&mut self, title: &str) {
        self.raw(b"\x1b]2;");
        for ch in title.chars().filter(|c| !c.is_control()) {
            let mut buf = [0u8; 4];
            self.raw(ch.encode_utf8(&mut buf).as_bytes());
        }
        self.raw(b"\x1b\\");
    }

    fn cursor_shape(&mut self, s: CursorShape) {
        let n = match s {
            CursorShape::BlinkingBlock => 1,
            // 0 = the terminal's default (alacritty: steady block).
            CursorShape::Block => 0,
            CursorShape::BlinkingUnderline => 3,
            CursorShape::Underline => 4,
            CursorShape::BlinkingBar => 5,
            CursorShape::Bar => 6,
        };
        self.csi_num(n, b" q");
    }
}

/// What the client looked like before the screen part of the payload.
struct Base<'a> {
    modes: TermModes,
    title: Option<&'a str>,
    shape: CursorShape,
    visible: bool,
}

/// Modes, title, pen reset, cursor shape/position/visibility, autowrap.
fn finish(w: &mut Writer<'_>, base: &Base<'_>, client_wrap: bool, to: &Snapshot) {
    w.modes(&base.modes, &to.modes);
    if base.title != Some(&*to.title) {
        w.title(&to.title);
    }
    w.reset_pen();
    if base.shape != to.cursor.shape {
        w.cursor_shape(to.cursor.shape);
    }
    let (r, c) = (to.cursor.row, to.cursor.col);
    if w.cur != Some((r, c)) {
        w.cup(r, c);
    }
    if base.visible != to.cursor.visible {
        w.private_mode(25, to.cursor.visible);
    }
    if client_wrap != to.modes.auto_wrap {
        w.private_mode(7, to.modes.auto_wrap);
    }
}

fn paint_from_blank(w: &mut Writer<'_>, to: &Snapshot) {
    let blank = Row::blank(usize::from(to.size.cols));
    for (r, row) in to.rows.iter().enumerate() {
        w.diff_row(r as u16, &blank.cells, &row.cells);
    }
}

/// Full repaint: RIS, history replay (newest [`FULL_REPLAY_LINES`], at most
/// [`MAX_REPLAY_BYTES`]), modes, every visible cell, title, cursor. The
/// payload never exceeds [`MAX_UPDATE_BYTES`].
pub fn encode_full(to: &Snapshot, store: &ScrollbackStore) -> EncodedUpdate {
    let lines_start = store
        .oldest()
        .max(to.history_lines.saturating_sub(FULL_REPLAY_LINES))
        .min(to.history_lines);
    let history_start = replay_budget_start(store, lines_start, to.history_lines, MAX_REPLAY_BYTES);
    let update = encode_full_from(to, store, history_start);
    if update.payload.len() <= MAX_UPDATE_BYTES {
        return update;
    }
    // The screen itself is heavy: drop the replay (the client fetches it).
    let update = encode_full_from(to, store, to.history_lines);
    if update.payload.len() <= MAX_UPDATE_BYTES {
        return update;
    }
    // Pathological screen (every cell a different color/link, combining
    // marks): repaint text only. Bounded by cols * rows * 4 bytes.
    let mut update = encode_full_from(&plain_snapshot(to), store, to.history_lines);
    update.size = to.size;
    update
}

/// `start` advanced so replaying `start..end` costs at most `budget` bytes.
fn replay_budget_start(store: &ScrollbackStore, start: u64, end: u64, budget: usize) -> u64 {
    if start >= end {
        return start;
    }
    store
        .budget_start(start, end, REPLAY_LINE_OVERHEAD, budget)
        .max(start)
}

/// `to` with every attribute, hyperlink and combining mark removed and the
/// title truncated.
fn plain_snapshot(to: &Snapshot) -> Snapshot {
    let rows = to
        .rows
        .iter()
        .map(|r| {
            let cells: Box<[Cell]> = r
                .cells
                .iter()
                .map(|c| Cell {
                    ch: c.ch,
                    flags: c.flags & !CellFlags::SGR_MASK,
                    ..Cell::BLANK
                })
                .collect();
            Arc::new(Row {
                cells,
                wrapped: r.wrapped,
            })
        })
        .collect();
    let mut cut = to.title.len().min(PLAIN_TITLE_BYTES);
    while !to.title.is_char_boundary(cut) {
        cut -= 1;
    }
    Snapshot {
        title: Arc::from(&to.title[..cut]),
        rows,
        ..to.clone()
    }
}

fn encode_full_from(to: &Snapshot, store: &ScrollbackStore, history_start: u64) -> EncodedUpdate {
    let mut out = Vec::with_capacity(4096);
    {
        let mut w = Writer::new(&mut out, to.size);
        w.raw(b"\x1bc");
        w.cur = Some((0, 0));
        w.replay(store, history_start, to.history_lines);
        if to.modes.alt_screen {
            w.private_mode(1049, true);
        }
        paint_from_blank(&mut w, to);
        let base = Base {
            modes: TermModes {
                auto_wrap: true,
                alt_screen: to.modes.alt_screen,
                ..TermModes::default()
            },
            title: None,
            shape: CursorShape::Block,
            visible: true,
        };
        finish(&mut w, &base, true, to);
    }
    EncodedUpdate {
        kind: UpdateKind::Full,
        state: to.state,
        base_state: 0,
        size: to.size,
        history_lines: to.history_lines,
        history_start,
        payload: out,
    }
}

/// Encode one row the way history lines are encoded (shared by the
/// scrollback store and the scroll fast path).
pub(crate) fn encode_line(cells: &[Cell], wrapped: bool) -> Vec<u8> {
    let end = if wrapped {
        cells.len()
    } else {
        cells
            .iter()
            .rposition(|c| !c.is_default_blank())
            .map_or(0, |p| p + 1)
    };
    let mut out = Vec::with_capacity(end + 8);
    {
        let mut w = Writer::new(&mut out, TermSize::new(cells.len().max(1) as u16, 1));
        for c in &cells[..end] {
            if c.flags.contains(CellFlags::WIDE_SPACER) {
                continue;
            }
            w.print(c);
        }
        w.set_link(&None);
    }
    out.extend_from_slice(b"\x1b[0m");
    out
}

/// True if `from`'s top `k` rows are exactly history lines
/// `from.history_lines..+k`, i.e. the change is a plain scroll.
fn top_rows_scrolled(from: &Snapshot, store: &ScrollbackStore, k: u64) -> bool {
    (0..k).all(|i| {
        let row = &from.rows[i as usize];
        store.get(from.history_lines + i).is_some_and(|line| {
            line.wrapped == row.wrapped && line.vt == encode_line(&row.cells, row.wrapped)
        })
    })
}

/// Diff `from` → `to`. Falls back to [`encode_full`] if `from` is not an
/// earlier state of the same terminal, or if the diff payload would exceed
/// [`MAX_UPDATE_BYTES`]. Replay is capped at [`MAX_REPLAY_LINES`] and
/// [`MAX_REPLAY_BYTES`] (oldest lines dropped first).
pub fn encode_diff(from: &Snapshot, to: &Snapshot, store: &ScrollbackStore) -> EncodedUpdate {
    if from.history_lines > to.history_lines
        || from.state > to.state
        || from.rows.len() != usize::from(from.size.rows)
    {
        return encode_full(to, store);
    }
    let lines_start = from
        .history_lines
        .max(to.history_lines.saturating_sub(MAX_REPLAY_LINES))
        .max(store.oldest())
        .min(to.history_lines);
    let history_start = replay_budget_start(store, lines_start, to.history_lines, MAX_REPLAY_BYTES);
    let mut out = Vec::new();
    let size_changed = (from.size.cols, from.size.rows) != (to.size.cols, to.size.rows);
    if from.state != to.state || size_changed {
        let mut w = Writer::new(&mut out, to.size);
        let rows = to.size.rows;
        let mut client_alt = from.modes.alt_screen;
        let mut client_wrap = from.modes.auto_wrap;
        let k = to.history_lines - from.history_lines;
        let alt_switch = from.modes.alt_screen != to.modes.alt_screen;
        if !size_changed
            && !alt_switch
            && !to.modes.alt_screen
            && k > 0
            && k < u64::from(rows)
            && history_start == from.history_lines
            && top_rows_scrolled(from, store, k)
        {
            // Plain scroll: let the client scroll its own rows into its
            // scrollback, then diff against the shifted screen.
            w.cup(rows - 1, 0);
            for _ in 0..k {
                w.out.push(b'\n');
            }
            let k = k as usize;
            let blank = Row::blank(usize::from(to.size.cols));
            for (r, new) in to.rows.iter().enumerate() {
                let old = from.rows.get(r + k).map_or(&*blank.cells, |o| &o.cells);
                if old != &*new.cells {
                    w.diff_row(r as u16, old, &new.cells);
                }
            }
        } else if size_changed || alt_switch || k > 0 {
            let mut blank_screen = false;
            if history_start < to.history_lines {
                if client_alt {
                    w.private_mode(1049, false);
                    client_alt = false;
                }
                if !client_wrap {
                    w.private_mode(7, true);
                    client_wrap = true;
                }
                w.home_clear();
                w.replay(store, history_start, to.history_lines);
                blank_screen = true;
            }
            if client_alt != to.modes.alt_screen {
                w.reset_pen();
                w.private_mode(1049, to.modes.alt_screen);
                // Entering the alt screen clears it; leaving shows stale
                // primary content.
                blank_screen = to.modes.alt_screen;
                w.cur = None;
            }
            if !blank_screen {
                w.home_clear();
            }
            paint_from_blank(&mut w, to);
        } else {
            for (r, (old, new)) in from.rows.iter().zip(to.rows.iter()).enumerate() {
                if !Arc::ptr_eq(old, new) && old.cells != new.cells {
                    w.diff_row(r as u16, &old.cells, &new.cells);
                }
            }
        }
        let base = Base {
            modes: TermModes {
                alt_screen: to.modes.alt_screen,
                ..from.modes
            },
            title: Some(&from.title),
            shape: from.cursor.shape,
            visible: from.cursor.visible,
        };
        finish(&mut w, &base, client_wrap, to);
    }
    if out.len() > MAX_UPDATE_BYTES {
        return encode_full(to, store);
    }
    EncodedUpdate {
        kind: UpdateKind::Diff,
        state: to.state,
        base_state: from.state,
        size: to.size,
        history_lines: to.history_lines,
        history_start,
        payload: out,
    }
}

/// Repaint cells `cols` of visible `row` exactly as in `snap` (attributes
/// included), preserving the cursor position (DECSC … DECRC) and leaving SGR
/// reset. Used by client-side prediction to undo predicted cells.
pub fn encode_cells(snap: &Snapshot, row: u16, cols: std::ops::Range<u16>) -> Vec<u8> {
    let mut out = Vec::new();
    let Some(r) = snap.rows.get(usize::from(row)) else {
        return out;
    };
    let n = r.cells.len();
    let mut start = usize::from(cols.start).min(n);
    let mut end = usize::from(cols.end).min(n);
    if start >= end {
        return out;
    }
    if start > 0 && r.cells[start].flags.contains(CellFlags::WIDE_SPACER) {
        start -= 1;
    }
    if end < n && r.cells[end - 1].flags.contains(CellFlags::WIDE) {
        end += 1;
    }
    out.extend_from_slice(b"\x1b7");
    {
        let mut w = Writer::new(&mut out, snap.size);
        w.cup(row, start as u16);
        w.paint(row, &r.cells, start, end);
        w.set_link(&None);
        w.raw(b"\x1b[0m");
    }
    out.extend_from_slice(b"\x1b8");
    out
}
