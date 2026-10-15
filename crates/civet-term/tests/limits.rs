//! Resource limits: update payloads always fit in a frame, sizes are clamped.

mod common;

use civet_proto::{MAX_FRAME_LEN, MAX_TERM_COLS, MAX_TERM_ROWS, TermSize, UpdateKind};
use civet_term::{MAX_REPLAY_BYTES, MAX_UPDATE_BYTES, PaneTerminal, TermConfig};
use common::{apply, term, term_with_limit};

/// One full-width line where every cell has its own truecolor fg and bg:
/// ~40 bytes of VT per cell once encoded.
fn heavy_line(cols: u16, seed: u32) -> Vec<u8> {
    let mut out = Vec::new();
    for c in 0..u32::from(cols) {
        let v = seed.wrapping_mul(31).wrapping_add(c * 7);
        out.extend(
            format!(
                "\x1b[38;2;{};{};{};48;2;{};{};{}m{}",
                v % 251,
                (v / 3) % 241,
                (v / 7) % 239,
                (v / 11) % 233,
                (v / 13) % 229,
                (v / 17) % 227,
                char::from(b'a' + (v % 26) as u8)
            )
            .bytes(),
        );
    }
    out.extend_from_slice(b"\x1b[0m\r\n");
    out
}

fn feed_heavy(t: &mut PaneTerminal, lines: u32) {
    let cols = t.size().cols;
    let mut buf = Vec::new();
    for i in 0..lines {
        buf.extend(heavy_line(cols, i));
        if buf.len() > 1 << 20 {
            t.feed(&buf);
            buf.clear();
        }
    }
    t.feed(&buf);
}

/// Bytes the heavy history would take to replay without a byte budget.
fn replay_bytes(t: &PaneTerminal, start: u64) -> usize {
    t.scrollback()
        .range(start, t.history_lines())
        .map(|(_, l)| l.vt.len() + 2)
        .sum()
}

#[test]
fn heavy_history_full_and_diff_fit_in_a_frame() {
    let mut t = term_with_limit(1000, 10, 10_000);
    let base = t.snapshot();
    feed_heavy(&mut t, 600);
    let n = t.history_lines();
    assert!(n >= 590);
    // Without the budget the replay alone would not fit in a frame.
    assert!(replay_bytes(&t, 0) > MAX_FRAME_LEN);

    let full = t.encode_update(None);
    assert_eq!(full.kind, UpdateKind::Full);
    assert!(
        full.payload.len() <= MAX_UPDATE_BYTES,
        "{}",
        full.payload.len()
    );
    assert!(full.history_start > 0, "oldest replay lines dropped");
    assert!(full.history_start < n, "newest lines still replayed");
    assert!(replay_bytes(&t, full.history_start) <= MAX_REPLAY_BYTES);

    let diff = t.encode_update(Some(&base));
    assert_eq!(diff.kind, UpdateKind::Diff);
    assert!(
        diff.payload.len() <= MAX_UPDATE_BYTES,
        "{}",
        diff.payload.len()
    );
    assert!(
        diff.history_start > base.history_lines(),
        "gap left to fetch"
    );
    assert!(diff.history_start < n);

    // The client still ends up showing exactly the daemon's screen.
    let mut client = term(1000, 10);
    apply(&mut client, &diff);
    common::assert_same(&t.snapshot(), &client.snapshot(), "heavy diff");
    let mut client = term(1000, 10);
    apply(&mut client, &full);
    common::assert_same(&t.snapshot(), &client.snapshot(), "heavy full");
}

#[test]
fn pathological_screen_falls_back_to_text_only_full() {
    let mut t = term_with_limit(MAX_TERM_COLS, MAX_TERM_ROWS, 100);
    // Fill every visible cell with its own colors (no trailing newline on the
    // last row so nothing scrolls off).
    let mut buf = Vec::new();
    for r in 0..u32::from(MAX_TERM_ROWS) {
        let mut l = heavy_line(MAX_TERM_COLS, r);
        l.truncate(l.len() - 2);
        buf.extend(format!("\x1b[{};1H", r + 1).bytes());
        buf.extend(l);
    }
    t.feed(&buf);
    let full = t.encode_update(None);
    assert_eq!(full.kind, UpdateKind::Full);
    assert!(
        full.payload.len() <= MAX_UPDATE_BYTES,
        "{}",
        full.payload.len()
    );
    assert!(
        !full.payload.windows(5).any(|w| w == b"48;2;"),
        "text-only fallback"
    );
    let mut client = term(MAX_TERM_COLS, MAX_TERM_ROWS);
    apply(&mut client, &full);
    assert_eq!(client.screen_text(), t.screen_text());
}

#[test]
fn oversized_sizes_are_clamped() {
    let huge = TermSize::new(u16::MAX, u16::MAX);
    let mut t = PaneTerminal::new(TermConfig {
        size: huge,
        scrollback_limit: 100,
    });
    assert_eq!(
        (t.size().cols, t.size().rows),
        (MAX_TERM_COLS, MAX_TERM_ROWS)
    );
    t.resize(TermSize::new(80, 24)).unwrap();
    t.resize(huge).unwrap();
    assert_eq!(
        (t.size().cols, t.size().rows),
        (MAX_TERM_COLS, MAX_TERM_ROWS)
    );
    let s = huge.sanitized();
    assert_eq!((s.cols, s.rows), (MAX_TERM_COLS, MAX_TERM_ROWS));
    let s = TermSize::new(0, 0).sanitized();
    assert_eq!((s.cols, s.rows), (1, 1));
}

#[test]
fn snapshot_ring_keeps_recent_states_only() {
    assert_eq!(civet_term::DEFAULT_SNAPSHOT_RING, 2);
    let mut t = term(20, 5);
    let mut states = Vec::new();
    for i in 0..5 {
        t.feed(format!("{i}\r\n").as_bytes());
        states.push(t.state());
    }
    assert!(t.snapshot_at(states[4]).is_some());
    assert!(t.snapshot_at(states[0]).is_none());
    // A missing base yields a correct Full.
    let upd = t.encode_update_since(Some(states[0]));
    assert_eq!(upd.kind, UpdateKind::Full);
}
