//! `PaneTerminal`: alacritty_terminal + side parser + scrollback capture +
//! state numbering + snapshot ring.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Line;
use alacritty_terminal::term::{Config, Osc52, Term, TermMode};
use alacritty_terminal::vte::ansi::{CursorShape as AlShape, Processor, Rgb, Timeout};
use civet_proto::{
    MAX_CLIPBOARD_BYTES, MAX_TERM_COLS, MAX_TERM_ROWS, ScrollbackLine, ScrollbackPage,
    ScrollbackRequest, TermSize,
};

use crate::encode::{EncodedUpdate, encode_diff, encode_full, encode_line};
use crate::scan::ScrollScanner;
use crate::scrollback::ScrollbackStore;
use crate::side::SideParser;
use crate::snapshot::{CellConverter, Row, Snapshot};
use crate::types::{
    Cell, CursorShape, CursorState, FeedOutcome, MouseMode, TermConfig, TermError, TermModes,
};

/// Snapshots retained by default for [`PaneTerminal::snapshot_at`].
///
/// Only the last fallback of `Subscription::reset_baseline` (after the
/// acked and in-flight snapshots); a miss just costs a Full repaint, so a
/// small ring keeps per-pane memory low.
pub const DEFAULT_SNAPSHOT_RING: usize = 2;

/// Lines a chunk may push into alacritty's history before it is drained.
/// alacritty keeps up to 1000 spare rows after `clear_history`; staying below
/// that lets it recycle rows instead of reallocating them.
const CHUNK_LINES: u64 = 900;

/// alacritty needs at least two columns (a wide char must fit).
const MIN_COLS: u16 = 2;

/// OSC 52: the program may *set* the clipboard (forwarded to clients as
/// [`FeedOutcome::clipboard`]), never read it: queries (`OSC 52 ; c ; ?`)
/// are ignored, so a remote program cannot exfiltrate the local clipboard.
const OSC52_POLICY: Osc52 = Osc52::OnlyCopy;

/// Default colors reported for OSC 10/11/4 queries when the program has not
/// set them (a dark theme).
const DEFAULT_FG: Rgb = Rgb {
    r: 0xd4,
    g: 0xd4,
    b: 0xd4,
};
const DEFAULT_BG: Rgb = Rgb {
    r: 0x1e,
    g: 0x1e,
    b: 0x1e,
};

/// Disables synchronized-update buffering (`?2026`): bytes are applied as
/// they arrive, so the scrollback bound per chunk holds and no timer is
/// needed. Snapshots are only taken at the end of a `feed()` call anyway.
#[derive(Debug, Default)]
struct NoSync;

impl Timeout for NoSync {
    fn set_timeout(&mut self, _duration: Duration) {}
    fn clear_timeout(&mut self) {}
    fn pending_timeout(&self) -> bool {
        false
    }
}

#[derive(Clone, Default)]
struct Listener(Arc<Mutex<Vec<Event>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        if matches!(
            event,
            Event::PtyWrite(_)
                | Event::ColorRequest(..)
                | Event::TextAreaSizeRequest(_)
                | Event::Bell
                | Event::ClipboardStore(..)
        ) {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).push(event);
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Dims {
    cols: usize,
    rows: usize,
}

impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// At least `MIN_COLS` x 1, at most [`MAX_TERM_COLS`] x [`MAX_TERM_ROWS`]
/// (an untrusted 65535x65535 would allocate billions of cells).
fn sanitize_size(size: TermSize) -> TermSize {
    TermSize {
        cols: size.cols.clamp(MIN_COLS, MAX_TERM_COLS),
        rows: size.rows.clamp(1, MAX_TERM_ROWS),
        ..size
    }
}

/// History capacity given to alacritty; the feed loop moves lines out after
/// every chunk, and chunks are bounded to push at most half of it.
fn history_cap(size: TermSize) -> usize {
    let rows = usize::from(size.rows);
    let cols = usize::from(size.cols);
    4096.max(4 * rows).max(2 * (65_536 / cols + 2))
}

/// One pane's terminal. `Send`, not `Sync`: civet-session keeps it in a
/// Mutex.
pub struct PaneTerminal {
    term: Term<Listener>,
    processor: Processor<NoSync>,
    events: Listener,
    side: SideParser,
    scanner: ScrollScanner,
    store: ScrollbackStore,
    conv: CellConverter,
    scratch: Vec<Cell>,
    size: TermSize,
    cap: usize,
    state: u64,
    current: Arc<Snapshot>,
    ring: VecDeque<Arc<Snapshot>>,
    ring_cap: usize,
}

impl fmt::Debug for PaneTerminal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PaneTerminal")
            .field("state", &self.state)
            .field("size", &self.size)
            .field("history_lines", &self.store.end())
            .field("alt_screen", &self.current.modes.alt_screen)
            .finish_non_exhaustive()
    }
}

impl PaneTerminal {
    /// New blank terminal at state 1. Sizes are clamped to at least 2x1.
    pub fn new(config: TermConfig) -> Self {
        let size = sanitize_size(config.size);
        let cap = history_cap(size);
        let events = Listener::default();
        let term_config = Config {
            scrolling_history: cap,
            osc52: OSC52_POLICY,
            ..Config::default()
        };
        let dims = Dims {
            cols: usize::from(size.cols),
            rows: usize::from(size.rows),
        };
        let term = Term::new(term_config, &dims, events.clone());
        let blank = Arc::new(Row::blank(usize::from(size.cols)));
        let current = Arc::new(Snapshot {
            state: 1,
            size,
            history_lines: 0,
            title: Arc::from(""),
            cursor: CursorState {
                row: 0,
                col: 0,
                visible: true,
                shape: CursorShape::Block,
            },
            modes: TermModes {
                auto_wrap: true,
                ..TermModes::default()
            },
            rows: vec![blank; usize::from(size.rows)],
        });
        let mut t = Self {
            term,
            processor: Processor::new(),
            events,
            side: SideParser::new(),
            scanner: ScrollScanner::default(),
            store: ScrollbackStore::new(config.scrollback_limit),
            conv: CellConverter::default(),
            scratch: Vec::with_capacity(usize::from(size.cols)),
            size,
            cap,
            state: 1,
            current,
            ring: VecDeque::new(),
            ring_cap: DEFAULT_SNAPSHOT_RING,
        };
        // Make the initial snapshot exactly what alacritty reports, without
        // bumping the state.
        t.capture(false);
        t.ring.push_back(t.current.clone());
        t
    }

    /// Parse PTY output. Any visible change (cells, cursor, modes, title,
    /// alt-screen, history growth) bumps `state()` by exactly one per call.
    pub fn feed(&mut self, bytes: &[u8]) -> FeedOutcome {
        let mut outcome = FeedOutcome::default();
        if bytes.is_empty() {
            return outcome;
        }
        let alt_before = self.current.modes.alt_screen;
        let cols = u64::from(self.size.cols);
        let rows = u64::from(self.size.rows);
        let limit = (self.cap as u64 / 2).min(CHUNK_LINES) * cols;
        let mut rest = bytes;
        while !rest.is_empty() {
            let n = self.scanner.next_chunk(rest, cols, rows, limit);
            let chunk = &rest[..n];
            self.processor.advance(&mut self.term, chunk);
            self.side.advance(chunk);
            self.drain_history();
            rest = &rest[n..];
        }
        self.collect_events(&mut outcome);
        let st = &mut self.side.st;
        outcome.pty_replies.append(&mut st.replies);
        outcome.notifications = std::mem::take(&mut st.notifications);
        outcome.prompt_marks = std::mem::take(&mut st.prompt_marks);
        outcome.cwd = st.cwd.take();
        let old_title = self.current.title.clone();
        outcome.changed = self.capture(true);
        if *self.current.title != *old_title {
            outcome.title = Some(self.current.title.to_string());
        }
        if self.current.modes.alt_screen != alt_before {
            outcome.alt_screen_changed = Some(self.current.modes.alt_screen);
        }
        outcome
    }

    fn collect_events(&mut self, outcome: &mut FeedOutcome) {
        let events = std::mem::take(&mut *self.events.0.lock().unwrap_or_else(|e| e.into_inner()));
        for ev in events {
            match ev {
                Event::PtyWrite(s) => outcome.pty_replies.extend_from_slice(s.as_bytes()),
                Event::ColorRequest(index, fmt) => {
                    let rgb = self.term.colors()[index].unwrap_or_else(|| default_color(index));
                    outcome.pty_replies.extend_from_slice(fmt(rgb).as_bytes());
                }
                Event::TextAreaSizeRequest(fmt) => {
                    let ws = WindowSize {
                        num_lines: self.size.rows,
                        num_cols: self.size.cols,
                        cell_width: self.size.pixel_width / self.size.cols,
                        cell_height: self.size.pixel_height / self.size.rows,
                    };
                    outcome.pty_replies.extend_from_slice(fmt(ws).as_bytes());
                }
                Event::Bell => outcome.bell = true,
                // Both `c` and `p`/`s`: the client OS has a single clipboard.
                Event::ClipboardStore(_, text) => {
                    if text.len() <= MAX_CLIPBOARD_BYTES {
                        outcome.clipboard = Some(text);
                    } else {
                        tracing::debug!(
                            len = text.len(),
                            "OSC 52 clipboard write too large, dropped"
                        );
                    }
                }
                _ => {}
            }
        }
    }

    /// Move alacritty's history (oldest first) into the store.
    fn drain_history(&mut self) {
        let h = self.term.grid().history_size();
        if h == 0 {
            return;
        }
        debug_assert!(h < self.cap, "scrollback chunk bound exceeded");
        let cols = self.size.cols;
        for i in (1..=h).rev() {
            let row = &self.term.grid()[Line(-(i as i32))];
            let wrapped = self.conv.convert_row(row, &mut self.scratch, false);
            let vt = encode_line(&self.scratch, wrapped);
            self.store
                .push_with_cols(ScrollbackLine { vt, wrapped }, cols);
        }
        self.term.grid_mut().clear_history();
    }

    /// Rebuild the current snapshot from alacritty; bump the state if
    /// anything visible changed. Returns whether it did.
    fn capture(&mut self, bump: bool) -> bool {
        let prev = self.current.clone();
        let rows = usize::from(self.size.rows);
        let same_dims = prev.rows.len() == rows && prev.size.cols == self.size.cols;
        let mut all_same = same_dims;
        let mut new_rows = Vec::with_capacity(rows);
        for line in 0..rows {
            let row = &self.term.grid()[Line(line as i32)];
            let wrapped = self.conv.convert_row(row, &mut self.scratch, true);
            match prev.rows.get(line) {
                Some(p) if same_dims && p.wrapped == wrapped && *p.cells == *self.scratch => {
                    new_rows.push(p.clone());
                }
                _ => {
                    all_same = false;
                    new_rows.push(Arc::new(Row {
                        cells: self.scratch.clone().into_boxed_slice(),
                        wrapped,
                    }));
                }
            }
        }
        let cursor = self.read_cursor();
        let modes = self.read_modes();
        let title: Arc<str> = if *prev.title == *self.side.st.title {
            prev.title.clone()
        } else {
            Arc::from(self.side.st.title.as_str())
        };
        let history_lines = self.store.end();
        if all_same
            && cursor == prev.cursor
            && modes == prev.modes
            && Arc::ptr_eq(&title, &prev.title)
            && history_lines == prev.history_lines
            && self.size == prev.size
        {
            return false;
        }
        if bump {
            self.state += 1;
        }
        let snap = Arc::new(Snapshot {
            state: self.state,
            size: self.size,
            history_lines,
            title,
            cursor,
            modes,
            rows: new_rows,
        });
        self.current = snap.clone();
        if bump {
            self.ring.push_back(snap);
            while self.ring.len() > self.ring_cap {
                self.ring.pop_front();
            }
        }
        bump
    }

    fn read_cursor(&self) -> CursorState {
        let p = self.term.grid().cursor.point;
        let style = self.term.cursor_style();
        let shape = match (style.shape, style.blinking) {
            (AlShape::Underline, false) => CursorShape::Underline,
            (AlShape::Underline, true) => CursorShape::BlinkingUnderline,
            (AlShape::Beam, false) => CursorShape::Bar,
            (AlShape::Beam, true) => CursorShape::BlinkingBar,
            (_, true) => CursorShape::BlinkingBlock,
            (_, false) => CursorShape::Block,
        };
        CursorState {
            row: p.line.0.clamp(0, i32::from(self.size.rows) - 1) as u16,
            col: (p.column.0 as u16).min(self.size.cols - 1),
            visible: self.term.mode().contains(TermMode::SHOW_CURSOR),
            shape,
        }
    }

    fn read_modes(&self) -> TermModes {
        let m = *self.term.mode();
        let mouse = if m.contains(TermMode::MOUSE_MOTION) {
            MouseMode::Motion
        } else if m.contains(TermMode::MOUSE_DRAG) {
            MouseMode::Drag
        } else if m.contains(TermMode::MOUSE_REPORT_CLICK) {
            MouseMode::Click
        } else {
            MouseMode::Off
        };
        TermModes {
            app_cursor: m.contains(TermMode::APP_CURSOR),
            app_keypad: m.contains(TermMode::APP_KEYPAD),
            bracketed_paste: m.contains(TermMode::BRACKETED_PASTE),
            mouse,
            mouse_sgr: m.contains(TermMode::SGR_MOUSE),
            focus_events: m.contains(TermMode::FOCUS_IN_OUT),
            alt_screen: m.contains(TermMode::ALT_SCREEN),
            auto_wrap: m.contains(TermMode::LINE_WRAP),
            reverse_video: false,
        }
    }

    /// Current state number. Starts at 1 for the initial blank screen.
    pub fn state(&self) -> u64 {
        self.state
    }

    /// Current size (columns clamped to at least 2).
    pub fn size(&self) -> TermSize {
        self.size
    }

    /// Resize the emulator (reflow as alacritty does). Bumps state if
    /// changed. Returns Err for 0 cols/rows.
    pub fn resize(&mut self, size: TermSize) -> Result<bool, TermError> {
        if size.cols == 0 || size.rows == 0 {
            return Err(TermError::InvalidSize(size));
        }
        let size = sanitize_size(size);
        if size == self.size {
            return Ok(false);
        }
        let cap = history_cap(size).max(history_cap(self.size));
        if cap != self.cap {
            self.cap = cap;
            let config = Config {
                scrolling_history: cap,
                osc52: OSC52_POLICY,
                ..Config::default()
            };
            self.term.set_options(config);
        }
        self.term.resize(Dims {
            cols: usize::from(size.cols),
            rows: usize::from(size.rows),
        });
        self.size = size;
        self.drain_history();
        Ok(self.capture(true))
    }

    /// Window title (OSC 0/2).
    pub fn title(&self) -> &str {
        &self.current.title
    }

    /// True while the alternate screen is shown.
    pub fn alt_screen(&self) -> bool {
        self.current.modes.alt_screen
    }

    /// Total lines ever scrolled off the primary screen (absolute counter).
    pub fn history_lines(&self) -> u64 {
        self.store.end()
    }

    /// Immutable copy of the current screen; repeated calls at the same
    /// state return the same Arc.
    pub fn snapshot(&mut self) -> Arc<Snapshot> {
        self.current.clone()
    }

    /// A recent snapshot by state number, if still in the ring.
    pub fn snapshot_at(&self, state: u64) -> Option<Arc<Snapshot>> {
        self.ring.iter().rev().find(|s| s.state == state).cloned()
    }

    /// How many recent snapshots [`Self::snapshot_at`] keeps (default
    /// [`DEFAULT_SNAPSHOT_RING`], minimum 1).
    pub fn set_snapshot_retention(&mut self, n: usize) {
        self.ring_cap = n.max(1);
        while self.ring.len() > self.ring_cap {
            self.ring.pop_front();
        }
    }

    /// Diff from `base` to the current state (or Full when `base` is None or
    /// cannot be diffed).
    pub fn encode_update(&mut self, base: Option<&Snapshot>) -> EncodedUpdate {
        let to = self.current.clone();
        match base {
            Some(b) => encode_diff(b, &to, &self.store),
            None => encode_full(&to, &self.store),
        }
    }

    /// Diff from the retained snapshot `known_state` to now, or Full if it is
    /// None or no longer retained.
    pub fn encode_update_since(&mut self, known_state: Option<u64>) -> EncodedUpdate {
        let base = known_state.and_then(|s| self.snapshot_at(s));
        self.encode_update(base.as_deref())
    }

    /// The history store.
    pub fn scrollback(&self) -> &ScrollbackStore {
        &self.store
    }

    /// Serve a ScrollbackQuery (clamped to what is retained).
    pub fn scrollback_page(&self, req: &ScrollbackRequest) -> ScrollbackPage {
        self.store.page(req)
    }

    /// Visible rows as plain text, trailing spaces trimmed.
    pub fn screen_text(&self) -> Vec<String> {
        self.current.screen_text()
    }

    /// Cursor.
    pub fn cursor(&self) -> CursorState {
        self.current.cursor
    }
}

/// xterm's default palette (dark theme fg/bg).
fn default_color(index: usize) -> Rgb {
    const ANSI: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0xcd, 0x00, 0x00),
        (0x00, 0xcd, 0x00),
        (0xcd, 0xcd, 0x00),
        (0x00, 0x00, 0xee),
        (0xcd, 0x00, 0xcd),
        (0x00, 0xcd, 0xcd),
        (0xe5, 0xe5, 0xe5),
        (0x7f, 0x7f, 0x7f),
        (0xff, 0x00, 0x00),
        (0x00, 0xff, 0x00),
        (0xff, 0xff, 0x00),
        (0x5c, 0x5c, 0xff),
        (0xff, 0x00, 0xff),
        (0x00, 0xff, 0xff),
        (0xff, 0xff, 0xff),
    ];
    match index {
        0..16 => {
            let (r, g, b) = ANSI[index];
            Rgb { r, g, b }
        }
        16..232 => {
            let i = index - 16;
            let level = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
            Rgb {
                r: level(i / 36),
                g: level((i / 6) % 6),
                b: level(i % 6),
            }
        }
        232..=255 => {
            let v = (8 + (index - 232) * 10) as u8;
            Rgb { r: v, g: v, b: v }
        }
        257 => DEFAULT_BG,
        _ => DEFAULT_FG,
    }
}
