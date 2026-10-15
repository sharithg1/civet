//! Deterministic, realistic streams: every chunk is checked through both
//! a continuous diff client and a fresh full repaint.

mod common;

use std::sync::Arc;

use civet_proto::{MAX_REPLAY_LINES, TermSize, UpdateKind};
use civet_term::{CellFlags, Color, PaneTerminal, Snapshot, encode_cells, encode_diff};
use common::{apply, assert_same, client_from_full, line_text, term};

/// Feed `chunks` to a server terminal, keep a client in sync by diffs, and
/// compare after every chunk. Returns the server, the client and the total
/// diff payload size.
fn sync_all(cols: u16, rows: u16, chunks: &[Vec<u8>]) -> (PaneTerminal, PaneTerminal, usize) {
    let mut a = term(cols, rows);
    let mut b = term(cols, rows);
    let mut at: Arc<Snapshot> = a.snapshot();
    let upd = civet_term::encode_full(&at, a.scrollback());
    apply(&mut b, &upd);
    let mut bytes = 0;
    for (i, c) in chunks.iter().enumerate() {
        a.feed(c);
        let to = a.snapshot();
        let upd = encode_diff(&at, &to, a.scrollback());
        bytes += upd.payload.len();
        apply(&mut b, &upd);
        assert_same(&to, &b.snapshot(), &format!("chunk {i} (diff)"));
        let mut f = client_from_full(&a, &to);
        assert_same(&to, &f.snapshot(), &format!("chunk {i} (full)"));
        at = to;
    }
    (a, b, bytes)
}

fn vim_frame(rows: u16, cols: u16, top: usize, cursor: (u16, u16)) -> Vec<u8> {
    let mut v = b"\x1b[?25l\x1b[H".to_vec();
    for r in 0..rows - 1 {
        let n = top + usize::from(r);
        v.extend(format!("\x1b[{};1H\x1b[33m{n:>4} \x1b[0m", r + 1).bytes());
        v.extend(format!("fn line_{n}() {{ let x = \"漢字 {n}\"; }}").bytes());
        v.extend(b"\x1b[K");
    }
    v.extend(
        format!(
            "\x1b[{rows};1H\x1b[7m NORMAL \x1b[0m\x1b[44;97m main.rs \x1b[0m\x1b[K\x1b[{};{}H\x1b[?25h",
            cursor.0 + 1,
            cursor.1 + 1
        )
        .bytes(),
    );
    let _ = cols;
    v
}

#[test]
fn vim_session() {
    let (rows, cols) = (24u16, 80u16);
    let mut chunks = vec![b"$ vim main.rs\r\n".to_vec()];
    chunks.push(b"\x1b[?1049h\x1b[22;0t\x1b[?1h\x1b=\x1b[?2004h\x1b[?1002h\x1b[?1006h".to_vec());
    for top in 0..30 {
        chunks.push(vim_frame(rows, cols, top, (5, 7)));
    }
    // Cursor-only movements.
    for c in 0..10 {
        chunks.push(format!("\x1b[6;{}H", c + 1).into_bytes());
    }
    // Insert mode typing.
    chunks.push(b"\x1b[6;10Hhello".to_vec());
    chunks.push(b"\x1b[1 q".to_vec());
    chunks.push(b"\x1b[?1006l\x1b[?1002l\x1b[?2004l\x1b[?1l\x1b>\x1b[?1049l\x1b[23;0t".to_vec());
    chunks.push(b"$ ".to_vec());
    let (mut a, _b, _) = sync_all(cols, rows, &chunks);
    assert!(!a.alt_screen());
    assert_eq!(a.screen_text()[0], "$ vim main.rs");
    assert_eq!(a.snapshot().modes().mouse, civet_term::MouseMode::Off);
}

#[test]
fn colored_log_scrolls_with_small_diffs() {
    let mut chunks = Vec::new();
    for i in 0..300 {
        chunks.push(
            format!(
                "\x1b[32m[INFO]\x1b[0m \x1b[38;5;244m2026-10-14T12:00:{:02}\x1b[0m request {i} \x1b[1;38;2;255;100;0mok\x1b[0m\r\n",
                i % 60
            )
            .into_bytes(),
        );
    }
    let (a, b, bytes) = sync_all(100, 30, &chunks);
    assert_eq!(a.history_lines(), 300 - 29);
    assert_eq!(b.history_lines(), a.history_lines());
    // Scroll fast path: each update is roughly one new line, not a repaint.
    assert!(bytes < 300 * 200, "diff bytes {bytes}");
    for (n, l) in a.scrollback().range(0, 5) {
        assert!(line_text(&l.vt).contains(&format!("request {n} ok")));
    }
}

#[test]
fn burst_output_coalesced_into_one_update() {
    let mut a = term(80, 24);
    let mut b = term(80, 24);
    let s0 = a.snapshot();
    apply(&mut b, &civet_term::encode_full(&s0, a.scrollback()));
    for i in 0..5000 {
        a.feed(format!("line {i} {}\r\n", "x".repeat(i % 120)).as_bytes());
    }
    let to = a.snapshot();
    let upd = encode_diff(&s0, &to, a.scrollback());
    assert_eq!(upd.kind, UpdateKind::Diff);
    assert_eq!(
        upd.history_start,
        to.history_lines() - MAX_REPLAY_LINES,
        "replay capped"
    );
    apply(&mut b, &upd);
    assert_same(&to, &b.snapshot(), "burst");
    assert_eq!(b.history_lines(), MAX_REPLAY_LINES);
}

#[test]
fn progress_bar_carriage_returns() {
    let mut chunks = Vec::new();
    for p in 0..=100 {
        chunks.push(
            format!(
                "\r\x1b[36m{:>3}%\x1b[0m [{}{}]",
                p,
                "#".repeat(p / 4),
                " ".repeat(25 - p / 4)
            )
            .into_bytes(),
        );
    }
    let (_, _, bytes) = sync_all(60, 10, &chunks);
    assert!(bytes < 101 * 40, "progress diffs should be tiny: {bytes}");
}

#[test]
fn scroll_region_and_alt_screen() {
    let mut chunks = vec![b"\x1b[?1049h\x1b[2J\x1b[1;1Htop bar\x1b[24;1Hbottom bar".to_vec()];
    chunks.push(b"\x1b[2;23r\x1b[23;1H".to_vec());
    for i in 0..40 {
        chunks.push(format!("\n\x1b[31mregion line {i}\x1b[0m").into_bytes());
    }
    chunks.push(b"\x1b[r\x1b[?1049l".to_vec());
    let (a, _, _) = sync_all(80, 24, &chunks);
    assert_eq!(a.history_lines(), 0, "alt screen has no history");
}

#[test]
fn line_wrapping_and_wide_chars() {
    let chunks = vec![
        "漢字かなカナ😀👍🏽e\u{301}".repeat(10).into_bytes(),
        b"\r\n".to_vec(),
        "x".repeat(500).into_bytes(),
        b"\x1b[3;5H\x1b[2P\x1b[4@\x1b[3X".to_vec(),
        "😀".repeat(30).into_bytes(),
    ];
    let (mut a, _, _) = sync_all(17, 6, &chunks);
    let s = a.snapshot();
    // Every WIDE cell has its spacer.
    for r in 0..6u16 {
        for c in 0..17u16 {
            let cell = s.cell(r, c).unwrap();
            if cell.flags.contains(CellFlags::WIDE) {
                assert!(
                    s.cell(r, c + 1)
                        .unwrap()
                        .flags
                        .contains(CellFlags::WIDE_SPACER)
                );
            }
        }
    }
}

#[test]
fn resize_sequence() {
    let mut a = term(80, 24);
    let mut b = term(80, 24);
    let mut at = a.snapshot();
    apply(&mut b, &civet_term::encode_full(&at, a.scrollback()));
    for i in 0..60 {
        a.feed(format!("\x1b[3{}mrow {i} {}\x1b[0m\r\n", i % 8, "=".repeat(i)).as_bytes());
    }
    for (c, r) in [(40, 10), (120, 50), (2, 1), (100, 30), (100, 30)] {
        let changed = a.resize(TermSize::new(c, r)).unwrap();
        let to = a.snapshot();
        assert_eq!(changed, to.state() != at.state());
        let upd = encode_diff(&at, &to, a.scrollback());
        apply(&mut b, &upd);
        assert_same(&to, &b.snapshot(), &format!("resize {c}x{r}"));
        at = to;
    }
    assert!(a.resize(TermSize::new(0, 10)).is_err());
    assert!(a.resize(TermSize::new(10, 0)).is_err());
}

#[test]
fn attributes_round_trip() {
    let mut a = term(40, 4);
    a.feed(
        b"\x1b[1mB\x1b[2mD\x1b[0;3mI\x1b[4mU\x1b[4:2mD\x1b[4:3mC\x1b[4:4md\x1b[4:5md\x1b[0;7mR\x1b[8mH\x1b[0;9mS\x1b[0m\
          \x1b[38;2;1;2;3;48;5;200;58;2;9;8;7mT\x1b[0;91;102mb\x1b[0m\x1b]8;;https://x.y\x1b\\L\x1b]8;;\x1b\\",
    );
    let s = a.snapshot();
    let mut b = client_from_full(&a, &s);
    assert_same(&s, &b.snapshot(), "attrs");
    let c = s.cell(0, 11).unwrap();
    assert_eq!(c.fg, Color::Rgb(1, 2, 3));
    assert_eq!(c.bg, Color::Indexed(200));
    assert_eq!(c.underline_color, Some(Color::Rgb(9, 8, 7)));
    assert_eq!(
        s.cell(0, 13).unwrap().hyperlink.as_deref(),
        Some("https://x.y")
    );
    assert!(s.cell(0, 5).unwrap().flags.contains(CellFlags::UNDERCURL));
}

#[test]
fn encode_cells_undoes_prediction() {
    let mut a = term(20, 3);
    a.feed(b"\x1b[31mhello\x1b[0m world\x1b[2;3H");
    let truth = a.snapshot();
    let mut b = client_from_full(&a, &truth);
    // Client-side prediction draws over cells 2..9 of row 0.
    b.feed(b"\x1b7\x1b[1;3H\x1b[4mXXXXXXX\x1b[0m\x1b8");
    assert_ne!(b.screen_text()[0], truth.screen_text()[0]);
    b.feed(&encode_cells(&truth, 0, 2..9));
    assert_same(&truth, &b.snapshot(), "undo");
}

#[test]
fn diff_from_same_state_is_empty() {
    let mut a = term(20, 3);
    a.feed(b"abc");
    let s = a.snapshot();
    let upd = encode_diff(&s, &s, a.scrollback());
    assert!(upd.payload.is_empty());
    assert_eq!(upd.history_start, s.history_lines());
}

#[test]
fn keystroke_echo_diff_is_small() {
    let mut a = term(200, 60);
    for i in 0..59 {
        a.feed(format!("\x1b[3{}m{}\x1b[0m\r\n", i % 8, "word ".repeat(39)).as_bytes());
    }
    a.feed(b"$ ");
    let s1 = a.snapshot();
    a.feed(b"l");
    let s2 = a.snapshot();
    let upd = encode_diff(&s1, &s2, a.scrollback());
    assert!(
        upd.payload.len() <= 12,
        "{:?}",
        String::from_utf8_lossy(&upd.payload)
    );
}

#[test]
fn update_since_uses_ring_or_full() {
    let mut a = term(20, 3);
    a.set_snapshot_retention(4);
    let first = a.state();
    for i in 0..10 {
        a.feed(format!("{i}").as_bytes());
    }
    assert_eq!(a.encode_update_since(Some(first)).kind, UpdateKind::Full);
    assert_eq!(a.encode_update_since(None).kind, UpdateKind::Full);
    let recent = a.state() - 2;
    let upd = a.encode_update_since(Some(recent));
    assert_eq!(upd.kind, UpdateKind::Diff);
    assert_eq!(upd.base_state, recent);
    let su = upd.into_screen_update(civet_proto::PaneId(7), 3);
    assert_eq!(su.pane, civet_proto::PaneId(7));
    assert_eq!(su.acked_input_seq, 3);
}
