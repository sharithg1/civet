//! Public value types: configuration, cells, colors, cursor, modes, feed
//! results.

use std::ops::{BitAnd, BitOr, BitOrAssign, Not};
use std::sync::Arc;

use civet_proto::TermSize;

/// Configuration for a [`crate::PaneTerminal`].
#[derive(Debug, Clone)]
pub struct TermConfig {
    /// Initial size.
    pub size: TermSize,
    /// Lines kept in the [`crate::ScrollbackStore`]. Default 10_000.
    pub scrollback_limit: usize,
}

impl Default for TermConfig {
    fn default() -> Self {
        Self {
            size: TermSize::new(80, 24),
            scrollback_limit: 10_000,
        }
    }
}

/// Errors from [`crate::PaneTerminal`].
#[derive(Debug, thiserror::Error)]
pub enum TermError {
    /// Zero columns or rows.
    #[error("invalid size {0:?}")]
    InvalidSize(TermSize),
}

/// Result of one [`crate::PaneTerminal::feed`] call.
#[derive(Debug, Default)]
pub struct FeedOutcome {
    /// `state()` changed during this call.
    pub changed: bool,
    /// Bytes the terminal must answer to the program (DSR/CPR, DA1/DA2,
    /// DECRQM, OSC 10/11 color queries, XTVERSION...): write them to the PTY.
    pub pty_replies: Vec<u8>,
    /// New title if OSC 0/2 (or a title-stack pop / RIS) changed it.
    pub title: Option<String>,
    /// BEL (0x07 outside of OSC/DCS) seen.
    pub bell: bool,
    /// OSC 9 / OSC 777;notify notifications, in order.
    pub notifications: Vec<OscNotification>,
    /// OSC 133 shell-integration marks, in order.
    pub prompt_marks: Vec<PromptMark>,
    /// OSC 7 working directory (file://host/path → path).
    pub cwd: Option<String>,
    /// Alternate screen entered/left during this call (final value).
    pub alt_screen_changed: Option<bool>,
    /// OSC 52 clipboard write (decoded text; the last one if several), at
    /// most [`civet_proto::MAX_CLIPBOARD_BYTES`]. Queries are never answered.
    pub clipboard: Option<String>,
}

/// A desktop notification requested by the program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OscNotification {
    /// None for OSC 9; the title field for OSC 777.
    pub title: Option<String>,
    /// Notification text.
    pub body: String,
}

/// OSC 133 (FinalTerm / shell integration) marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptMark {
    /// OSC 133;A
    PromptStart,
    /// OSC 133;B
    CommandStart,
    /// OSC 133;C
    CommandExecuted,
    /// OSC 133;D[;code]
    CommandFinished {
        /// Exit code, if the shell reported one.
        exit_code: Option<i32>,
    },
}

/// Cursor position (0-based, visible screen), visibility and shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorState {
    /// Row, 0-based.
    pub row: u16,
    /// Column, 0-based.
    pub col: u16,
    /// DECTCEM.
    pub visible: bool,
    /// DECSCUSR shape.
    pub shape: CursorShape,
}

/// Cursor shape as set by DECSCUSR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    /// Steady block (the default).
    #[default]
    Block,
    /// Steady underline.
    Underline,
    /// Steady bar.
    Bar,
    /// Blinking block.
    BlinkingBlock,
    /// Blinking underline.
    BlinkingUnderline,
    /// Blinking bar.
    BlinkingBar,
}

/// Modes that affect how the *client* encodes input or renders; mirrored to
/// xterm.js by the encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TermModes {
    /// DECCKM `?1`.
    pub app_cursor: bool,
    /// DECKPAM / DECKPNM.
    pub app_keypad: bool,
    /// `?2004`.
    pub bracketed_paste: bool,
    /// `?1000` / `?1002` / `?1003`.
    pub mouse: MouseMode,
    /// `?1006`.
    pub mouse_sgr: bool,
    /// `?1004`.
    pub focus_events: bool,
    /// `?1049` / `?1047` / `?47`.
    pub alt_screen: bool,
    /// `?7` (default true in practice).
    pub auto_wrap: bool,
    /// `?5`. alacritty_terminal does not implement DECSCNM, so this is
    /// always false for snapshots taken from a [`crate::PaneTerminal`].
    pub reverse_video: bool,
}

/// Mouse reporting protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseMode {
    /// No reporting.
    #[default]
    Off,
    /// `?1000`: button press/release.
    Click,
    /// `?1002`: plus motion while a button is held.
    Drag,
    /// `?1003`: all motion.
    Motion,
}

/// Cell color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Color {
    /// Terminal default foreground/background.
    Default,
    /// Palette index (0-15 are the ANSI colors).
    Indexed(u8),
    /// Truecolor.
    Rgb(u8, u8, u8),
}

/// Cell attribute bit set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct CellFlags(pub u16);

impl CellFlags {
    /// No flags.
    pub const EMPTY: Self = Self(0);
    /// SGR 1.
    pub const BOLD: Self = Self(1);
    /// SGR 2.
    pub const DIM: Self = Self(1 << 1);
    /// SGR 3.
    pub const ITALIC: Self = Self(1 << 2);
    /// SGR 4.
    pub const UNDERLINE: Self = Self(1 << 3);
    /// SGR 4:2.
    pub const DOUBLE_UNDERLINE: Self = Self(1 << 4);
    /// SGR 4:3.
    pub const UNDERCURL: Self = Self(1 << 5);
    /// SGR 5 (alacritty_terminal does not track blink; never set by it).
    pub const BLINK: Self = Self(1 << 6);
    /// SGR 7.
    pub const INVERSE: Self = Self(1 << 7);
    /// SGR 8.
    pub const HIDDEN: Self = Self(1 << 8);
    /// SGR 9.
    pub const STRIKETHROUGH: Self = Self(1 << 9);
    /// First half of a wide (2-cell) char.
    pub const WIDE: Self = Self(1 << 10);
    /// Second half of a wide char (renders nothing).
    pub const WIDE_SPACER: Self = Self(1 << 11);
    /// SGR 4:4.
    pub const DOTTED_UNDERLINE: Self = Self(1 << 12);
    /// SGR 4:5.
    pub const DASHED_UNDERLINE: Self = Self(1 << 13);

    /// Every underline style.
    pub const ALL_UNDERLINES: Self = Self(
        Self::UNDERLINE.0
            | Self::DOUBLE_UNDERLINE.0
            | Self::UNDERCURL.0
            | Self::DOTTED_UNDERLINE.0
            | Self::DASHED_UNDERLINE.0,
    );
    /// Flags that describe rendition (SGR), as opposed to cell layout.
    pub const SGR_MASK: Self = Self(!(Self::WIDE.0 | Self::WIDE_SPACER.0));

    /// True if every bit of `other` is set.
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// True if any bit of `other` is set.
    pub fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// True if no bit is set.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Set or clear `other`.
    pub fn set(&mut self, other: Self, on: bool) {
        if on {
            self.0 |= other.0;
        } else {
            self.0 &= !other.0;
        }
    }
}

impl BitOr for CellFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for CellFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for CellFlags {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl Not for CellFlags {
    type Output = Self;
    fn not(self) -> Self {
        Self(!self.0)
    }
}

/// One screen cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// The character (' ' for blank cells and wide-char spacers).
    pub ch: char,
    /// Zero-width / combining characters following `ch`.
    pub zerowidth: Option<Box<str>>,
    /// Foreground.
    pub fg: Color,
    /// Background.
    pub bg: Color,
    /// Underline color (SGR 58), None = same as fg.
    pub underline_color: Option<Color>,
    /// Attributes.
    pub flags: CellFlags,
    /// OSC 8 hyperlink URI, if any.
    pub hyperlink: Option<Arc<str>>,
}

impl Default for Cell {
    fn default() -> Self {
        Self::BLANK
    }
}

impl Cell {
    /// A blank cell with default colors.
    pub const BLANK: Cell = Cell {
        ch: ' ',
        zerowidth: None,
        fg: Color::Default,
        bg: Color::Default,
        underline_color: None,
        flags: CellFlags::EMPTY,
        hyperlink: None,
    };

    /// True for a cell that an erase (EL/ED/ECH) with background `bg` would
    /// produce: a space with default foreground, no attributes, no extras.
    pub fn is_erased(&self) -> bool {
        self.ch == ' '
            && self.fg == Color::Default
            && self.flags.is_empty()
            && self.zerowidth.is_none()
            && self.underline_color.is_none()
            && self.hyperlink.is_none()
    }

    /// True for a blank cell with default colors.
    pub fn is_default_blank(&self) -> bool {
        self.is_erased() && self.bg == Color::Default
    }
}
