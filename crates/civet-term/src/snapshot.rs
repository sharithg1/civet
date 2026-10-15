//! Immutable screen snapshots and conversion from alacritty's grid.

use std::fmt;
use std::sync::Arc;

use alacritty_terminal::grid::Row as AlRow;
use alacritty_terminal::index::Column;
use alacritty_terminal::term::cell::{Cell as AlCell, Flags as AlFlags, Hyperlink as AlHyperlink};
use alacritty_terminal::vte::ansi::{Color as AlColor, NamedColor};
use civet_proto::TermSize;

use crate::types::{Cell, CellFlags, Color, CursorState, TermModes};

/// One visible row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) cells: Box<[Cell]>,
    /// The row soft-wraps into the next one.
    pub(crate) wrapped: bool,
}

impl Row {
    pub(crate) fn blank(cols: usize) -> Self {
        Self {
            cells: vec![Cell::BLANK; cols].into_boxed_slice(),
            wrapped: false,
        }
    }
}

/// Immutable screen state. Rows are shared (`Arc`) between snapshots when
/// unchanged, so retaining several snapshots is cheap.
#[derive(Clone)]
pub struct Snapshot {
    pub(crate) state: u64,
    pub(crate) size: TermSize,
    pub(crate) history_lines: u64,
    pub(crate) title: Arc<str>,
    pub(crate) cursor: CursorState,
    pub(crate) modes: TermModes,
    pub(crate) rows: Vec<Arc<Row>>,
}

impl fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Snapshot")
            .field("state", &self.state)
            .field("size", &self.size)
            .field("history_lines", &self.history_lines)
            .field("title", &self.title)
            .field("cursor", &self.cursor)
            .field("modes", &self.modes)
            .finish_non_exhaustive()
    }
}

impl Snapshot {
    /// State number this snapshot was taken at.
    pub fn state(&self) -> u64 {
        self.state
    }

    /// Terminal size.
    pub fn size(&self) -> TermSize {
        self.size
    }

    /// Absolute history line counter at this state.
    pub fn history_lines(&self) -> u64 {
        self.history_lines
    }

    /// True if the alternate screen is shown.
    pub fn alt_screen(&self) -> bool {
        self.modes.alt_screen
    }

    /// Window title (OSC 0/2).
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Cursor.
    pub fn cursor(&self) -> CursorState {
        self.cursor
    }

    /// Client-relevant modes.
    pub fn modes(&self) -> TermModes {
        self.modes
    }

    /// 0-based visible row/col; None if out of range.
    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        self.rows.get(usize::from(row))?.cells.get(usize::from(col))
    }

    /// True if visible `row` soft-wraps into the next row.
    pub fn row_wrapped(&self, row: u16) -> bool {
        self.rows.get(usize::from(row)).is_some_and(|r| r.wrapped)
    }

    /// Row text (wide-char spacers skipped, combining chars included),
    /// trailing spaces trimmed.
    pub fn row_text(&self, row: u16) -> String {
        let Some(r) = self.rows.get(usize::from(row)) else {
            return String::new();
        };
        let mut s = String::with_capacity(r.cells.len());
        for c in r.cells.iter() {
            if c.flags.contains(CellFlags::WIDE_SPACER) {
                continue;
            }
            s.push(c.ch);
            if let Some(z) = &c.zerowidth {
                s.push_str(z);
            }
        }
        s.truncate(s.trim_end_matches(' ').len());
        s
    }

    /// Visible rows as text.
    pub fn screen_text(&self) -> Vec<String> {
        (0..self.rows.len() as u16)
            .map(|r| self.row_text(r))
            .collect()
    }

    /// True if the visible content (cells, wrap flags aside), cursor, modes,
    /// title and size equal `other`'s. State numbers and history counters are
    /// ignored. Used by tests and by callers that want to skip no-op
    /// updates.
    pub fn same_screen(&self, other: &Snapshot) -> bool {
        self.size.cols == other.size.cols
            && self.size.rows == other.size.rows
            && self.cursor == other.cursor
            && self.modes == other.modes
            && self.title == other.title
            && self
                .rows
                .iter()
                .zip(other.rows.iter())
                .all(|(a, b)| Arc::ptr_eq(a, b) || a.cells == b.cells)
    }

    /// First visible position where `self` and `other` differ, for test
    /// diagnostics.
    pub fn first_difference(&self, other: &Snapshot) -> Option<String> {
        if (self.size.cols, self.size.rows) != (other.size.cols, other.size.rows) {
            return Some(format!("size {:?} vs {:?}", self.size, other.size));
        }
        for (r, (a, b)) in self.rows.iter().zip(other.rows.iter()).enumerate() {
            for (c, (x, y)) in a.cells.iter().zip(b.cells.iter()).enumerate() {
                if x != y {
                    return Some(format!("cell ({r},{c}): {x:?} vs {y:?}"));
                }
            }
        }
        if self.cursor != other.cursor {
            return Some(format!("cursor {:?} vs {:?}", self.cursor, other.cursor));
        }
        if self.modes != other.modes {
            return Some(format!("modes {:?} vs {:?}", self.modes, other.modes));
        }
        if self.title != other.title {
            return Some(format!("title {:?} vs {:?}", self.title, other.title));
        }
        None
    }
}

/// Maps alacritty cells to [`Cell`]s, caching hyperlink URIs so unchanged
/// links do not allocate on every snapshot.
#[derive(Debug, Default)]
pub(crate) struct CellConverter {
    link_cache: Option<(AlHyperlink, Arc<str>)>,
}

pub(crate) fn map_color(c: AlColor) -> Color {
    match c {
        AlColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        AlColor::Indexed(i) => Color::Indexed(i),
        AlColor::Named(n) => {
            let i = n as usize;
            if i < 16 {
                Color::Indexed(i as u8)
            } else {
                match n {
                    NamedColor::DimBlack => Color::Indexed(0),
                    NamedColor::DimRed => Color::Indexed(1),
                    NamedColor::DimGreen => Color::Indexed(2),
                    NamedColor::DimYellow => Color::Indexed(3),
                    NamedColor::DimBlue => Color::Indexed(4),
                    NamedColor::DimMagenta => Color::Indexed(5),
                    NamedColor::DimCyan => Color::Indexed(6),
                    NamedColor::DimWhite => Color::Indexed(7),
                    _ => Color::Default,
                }
            }
        }
    }
}

fn map_flags(f: AlFlags) -> CellFlags {
    let mut out = CellFlags::EMPTY;
    if f.is_empty() {
        return out;
    }
    let table = [
        (AlFlags::BOLD, CellFlags::BOLD),
        (AlFlags::DIM, CellFlags::DIM),
        (AlFlags::ITALIC, CellFlags::ITALIC),
        (AlFlags::UNDERLINE, CellFlags::UNDERLINE),
        (AlFlags::DOUBLE_UNDERLINE, CellFlags::DOUBLE_UNDERLINE),
        (AlFlags::UNDERCURL, CellFlags::UNDERCURL),
        (AlFlags::DOTTED_UNDERLINE, CellFlags::DOTTED_UNDERLINE),
        (AlFlags::DASHED_UNDERLINE, CellFlags::DASHED_UNDERLINE),
        (AlFlags::INVERSE, CellFlags::INVERSE),
        (AlFlags::HIDDEN, CellFlags::HIDDEN),
        (AlFlags::STRIKEOUT, CellFlags::STRIKETHROUGH),
        (AlFlags::WIDE_CHAR, CellFlags::WIDE),
        (AlFlags::WIDE_CHAR_SPACER, CellFlags::WIDE_SPACER),
    ];
    for (a, c) in table {
        if f.contains(a) {
            out |= c;
        }
    }
    out
}

impl CellConverter {
    fn convert(&mut self, c: &AlCell) -> Cell {
        let ch = if c.c == '\t' { ' ' } else { c.c };
        let mut cell = Cell {
            ch,
            zerowidth: None,
            fg: map_color(c.fg),
            bg: map_color(c.bg),
            underline_color: None,
            flags: map_flags(c.flags),
            hyperlink: None,
        };
        if c.extra.is_some() {
            if let Some(z) = c.zerowidth()
                && !z.is_empty()
            {
                cell.zerowidth = Some(z.iter().collect::<String>().into_boxed_str());
            }
            cell.underline_color = c
                .underline_color()
                .map(map_color)
                .filter(|c| *c != Color::Default);
            if let Some(link) = c.hyperlink() {
                cell.hyperlink = Some(self.link_uri(link));
            }
        }
        if c.flags.contains(AlFlags::LEADING_WIDE_CHAR_SPACER) {
            // Placeholder for a wide char that wrapped to the next line:
            // renders as an ordinary blank.
            cell.ch = ' ';
            cell.zerowidth = None;
        }
        cell
    }

    fn link_uri(&mut self, link: AlHyperlink) -> Arc<str> {
        if let Some((cached, uri)) = &self.link_cache
            && cached.uri() == link.uri()
        {
            return uri.clone();
        }
        let uri: Arc<str> = Arc::from(link.uri());
        self.link_cache = Some((link, uri.clone()));
        uri
    }

    /// Convert one alacritty row into `out` (cleared first), normalizing
    /// orphaned wide-char halves (which DCH/ICH/ECH can create) into plain
    /// blanks so every `WIDE` cell is followed by exactly one `WIDE_SPACER`.
    /// With `pad`, `out` gets the full row width; without, only up to the
    /// last non-blank cell. Returns the soft-wrap flag.
    pub(crate) fn convert_row(
        &mut self,
        row: &AlRow<AlCell>,
        out: &mut Vec<Cell>,
        pad: bool,
    ) -> bool {
        out.clear();
        let cols = row.len();
        let mut occ = cols;
        while occ > 0 && al_is_blank(&row[Column(occ - 1)]) {
            occ -= 1;
        }
        for i in 0..occ {
            out.push(self.convert(&row[Column(i)]));
        }
        normalize_wide(out);
        if pad {
            out.resize(cols, Cell::BLANK);
        }
        cols > 0 && row[Column(cols - 1)].flags.contains(AlFlags::WRAPLINE)
    }
}

/// A default blank cell (what [`Cell::BLANK`] converts from).
fn al_is_blank(c: &AlCell) -> bool {
    c.c == ' '
        && c.flags.is_empty()
        && c.extra.is_none()
        && c.fg == AlColor::Named(NamedColor::Foreground)
        && c.bg == AlColor::Named(NamedColor::Background)
}

/// Make wide/spacer pairs consistent.
pub(crate) fn normalize_wide(cells: &mut [Cell]) {
    let n = cells.len();
    let mut i = 0;
    while i < n {
        let f = cells[i].flags;
        if f.contains(CellFlags::WIDE) {
            if i + 1 < n && cells[i + 1].flags.contains(CellFlags::WIDE_SPACER) {
                cells[i + 1].flags.set(CellFlags::WIDE, false);
                i += 2;
                continue;
            }
            orphan(&mut cells[i]);
        } else if f.contains(CellFlags::WIDE_SPACER) {
            orphan(&mut cells[i]);
        }
        i += 1;
    }
}

fn orphan(c: &mut Cell) {
    c.flags.set(CellFlags::WIDE | CellFlags::WIDE_SPACER, false);
    c.ch = ' ';
    c.zerowidth = None;
}
