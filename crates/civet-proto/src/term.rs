//! Terminal traffic: input, screen state sync, scrollback paging.
//!
//! # State sync model (mosh-style)
//!
//! * The daemon owns the authoritative screen of every pane. Each change to
//!   the screen produces a new, monotonically increasing **state number**
//!   (`u64`, per pane, starting at 1; 0 means "nothing").
//! * The client sends keystrokes as [`InputMessage`]s with a per-(client,
//!   pane) **input sequence number**, starting at 1 and incremented by one per
//!   message. The daemon applies each `seq` at most once, in order, so the
//!   client may safely resend unacknowledged input after a reconnect.
//! * The daemon sends [`ScreenUpdate`]s: VT bytes that transform the screen at
//!   `base_state` into the screen at `state` (`UpdateKind::Diff`) or repaint
//!   it from scratch (`UpdateKind::Full`). Each update also echoes the highest
//!   input `seq` applied (`acked_input_seq`), which drives local-echo
//!   prediction and input retransmission.
//! * The client acknowledges the newest state it has rendered with
//!   [`ScreenAck`]. The daemon diffs future updates against the newest acked
//!   state it still holds; if it no longer has that state, it sends `Full`.
//! * History that scrolls off the top is not re-sent in screen updates; the
//!   client fetches it lazily with [`ScrollbackRequest`].

use serde::{Deserialize, Serialize};

use crate::ids::{PaneId, RequestId};

/// Terminal dimensions in character cells (plus optional pixel size, passed to
/// the PTY for programs that care).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TermSize {
    /// Columns (>= 1).
    pub cols: u16,
    /// Rows (>= 1).
    pub rows: u16,
    /// Width of the whole terminal area in pixels, 0 if unknown.
    pub pixel_width: u16,
    /// Height of the whole terminal area in pixels, 0 if unknown.
    pub pixel_height: u16,
}

impl TermSize {
    /// Size in cells with unknown pixel dimensions.
    pub const fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    /// Clamp to at least 1x1.
    pub fn sanitized(self) -> Self {
        Self {
            cols: self.cols.max(1),
            rows: self.rows.max(1),
            ..self
        }
    }
}

impl Default for TermSize {
    fn default() -> Self {
        Self::new(80, 24)
    }
}

/// Keystrokes / pasted bytes for a pane, client -> daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputMessage {
    /// Target pane.
    pub pane: PaneId,
    /// Per-(client, pane) sequence number, 1-based, contiguous.
    pub seq: u64,
    /// Raw bytes to write to the PTY (UTF-8 text, control codes, escape
    /// sequences for keys/mouse, bracketed paste...).
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

/// Whether a [`ScreenUpdate`] is a diff or a full repaint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdateKind {
    /// `payload` repaints the whole screen (clears first). `base_state` is 0.
    Full,
    /// `payload` turns the screen at `base_state` into the screen at `state`.
    Diff,
}

/// Screen change for one pane, daemon -> client.
///
/// `payload` is a VT/ANSI byte stream that xterm.js can `write()` directly.
/// It always leaves the cursor at the correct position/visibility and the
/// SGR attributes reset, and includes any mode changes (alternate screen,
/// bracketed paste, mouse reporting, application cursor keys) needed to make
/// the client terminal match the server's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenUpdate {
    /// Pane this update applies to.
    pub pane: PaneId,
    /// New server state number after applying this update.
    pub state: u64,
    /// State this diff is relative to (0 for [`UpdateKind::Full`]). The
    /// client must apply a `Diff` only if its current state equals
    /// `base_state`; otherwise it should drop it and wait for a `Full`
    /// (it can force one by re-subscribing with `known_state: None`).
    pub base_state: u64,
    /// Full repaint vs diff.
    pub kind: UpdateKind,
    /// Highest input `seq` from this client the daemon has written to the
    /// PTY at the time of this state.
    pub acked_input_seq: u64,
    /// Terminal size this screen state was rendered at.
    pub size: TermSize,
    /// Total number of lines that have ever scrolled into this pane's
    /// scrollback (absolute line counter). If it advanced by more than the
    /// client saw stream past, the client can fetch the gap via scrollback.
    pub history_lines: u64,
    /// VT bytes; see type docs.
    #[serde(with = "serde_bytes")]
    pub payload: Vec<u8>,
}

/// Client acknowledges it has applied screen `state` of `pane`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenAck {
    /// Pane.
    pub pane: PaneId,
    /// Newest state number the client has applied.
    pub state: u64,
}

/// Which slice of history to fetch.
///
/// Lines are addressed by **absolute line number**: 0 is the first line that
/// ever scrolled off the top of this pane; numbers never get reused even when
/// old history is evicted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScrollbackQuery {
    /// Up to `limit` lines immediately before line `end` (exclusive), i.e.
    /// paging upwards. `end: None` means "before the newest history line"
    /// (start from the bottom of the scrollback).
    Before {
        /// Exclusive upper bound, or None for the newest history.
        end: Option<u64>,
        /// Maximum number of lines.
        limit: u32,
    },
    /// Lines `start..end` (end exclusive), clamped to what is retained.
    Range {
        /// Inclusive start.
        start: u64,
        /// Exclusive end.
        end: u64,
    },
}

/// Request a page of scrollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrollbackRequest {
    /// Echoed in the response.
    pub req_id: RequestId,
    /// Pane.
    pub pane: PaneId,
    /// Which lines.
    pub query: ScrollbackQuery,
}

/// One history line, rendered as VT bytes (SGR attributes inline, ending with
/// attributes reset, no trailing newline).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrollbackLine {
    /// VT bytes for the line's content.
    #[serde(with = "serde_bytes")]
    pub vt: Vec<u8>,
    /// True if this line soft-wraps into the next one (no hard newline).
    pub wrapped: bool,
}

/// Response to a [`ScrollbackRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrollbackPage {
    /// Echo of the request id.
    pub req_id: RequestId,
    /// Pane.
    pub pane: PaneId,
    /// Absolute line number of `lines[0]`.
    pub start: u64,
    /// Lines in order, oldest first.
    pub lines: Vec<ScrollbackLine>,
    /// Oldest absolute line number still retained (older lines were evicted).
    pub oldest_available: u64,
    /// One past the newest history line (== `history_lines` in
    /// [`ScreenUpdate`]).
    pub end_available: u64,
}

impl ScrollbackPage {
    /// Absolute line number one past the last line in this page.
    pub fn end(&self) -> u64 {
        self.start + self.lines.len() as u64
    }

    /// True if there is retained history older than this page.
    pub fn has_older(&self) -> bool {
        self.start > self.oldest_available
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_sanitize() {
        assert_eq!(TermSize::new(0, 0).sanitized(), TermSize::new(1, 1));
        assert_eq!(TermSize::default(), TermSize::new(80, 24));
    }

    #[test]
    fn page_helpers() {
        let p = ScrollbackPage {
            req_id: 1,
            pane: PaneId(1),
            start: 10,
            lines: vec![
                ScrollbackLine {
                    vt: b"a".to_vec(),
                    wrapped: false
                };
                5
            ],
            oldest_available: 0,
            end_available: 100,
        };
        assert_eq!(p.end(), 15);
        assert!(p.has_older());
    }

    #[test]
    fn input_bytes_are_compact_in_postcard() {
        let m = InputMessage {
            pane: PaneId(1),
            seq: 1,
            data: vec![0xff; 200],
        };
        let bin = postcard::to_stdvec(&m).unwrap();
        // pane(1) + seq(1) + len varint(2) + 200 bytes
        assert_eq!(bin.len(), 204);
    }
}
