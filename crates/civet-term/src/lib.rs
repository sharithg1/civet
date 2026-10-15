//! Server-side terminal emulation for Civet.
//!
//! One [`PaneTerminal`] per pane: an `alacritty_terminal` emulator fed raw
//! PTY bytes, immutable reference-counted [`Snapshot`]s numbered by a
//! monotonically increasing state, a snapshot → VT encoder producing minimal
//! diffs ([`encode_diff`]) or full repaints ([`encode_full`]) that xterm.js
//! plays directly, and an absolute-numbered [`ScrollbackStore`].
//!
//! Pure and synchronous (no tokio). Also usable client-side as a mirror for
//! local-echo prediction. See `docs/INTERFACES.md` §3 for the contract and
//! `SPIKE.md` for the emulator choice and performance numbers.

mod encode;
mod scan;
mod scrollback;
mod side;
mod snapshot;
mod terminal;
mod types;

pub use encode::{
    EncodedUpdate, MAX_REPLAY_BYTES, MAX_UPDATE_BYTES, encode_cells, encode_diff, encode_full,
};
pub use scrollback::ScrollbackStore;
pub use snapshot::Snapshot;
pub use terminal::{DEFAULT_SNAPSHOT_RING, PaneTerminal};
pub use types::{
    Cell, CellFlags, Color, CursorShape, CursorState, FeedOutcome, MouseMode, OscNotification,
    PromptMark, TermConfig, TermError, TermModes,
};
