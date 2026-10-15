//! Release-mode micro benchmarks for civet-term.
//!
//! Run: `cargo run --release -p civet-term --example bench`

use std::hint::black_box;
use std::time::{Duration, Instant};

use civet_proto::TermSize;
use civet_term::{PaneTerminal, TermConfig, encode_diff, encode_full};

const COLS: u16 = 200;
const ROWS: u16 = 60;

fn term() -> PaneTerminal {
    PaneTerminal::new(TermConfig {
        size: TermSize::new(COLS, ROWS),
        scrollback_limit: 10_000,
    })
}

/// Run `f` repeatedly for ~0.5 s; return the mean time per call.
fn time<F: FnMut()>(mut f: F) -> Duration {
    for _ in 0..10 {
        f();
    }
    let start = Instant::now();
    let mut n = 0u32;
    while start.elapsed() < Duration::from_millis(500) {
        f();
        n += 1;
    }
    start.elapsed() / n
}

fn colored_log(bytes: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(bytes + 256);
    let mut i = 0u64;
    while v.len() < bytes {
        v.extend(
            format!(
                "\x1b[32m[INFO]\x1b[0m \x1b[38;5;244m12:00:{:02}.{:03}\x1b[0m worker={} \
                 request /api/v1/items/{} took \x1b[1;33m{}ms\x1b[0m status=200 bytes={}\r\n",
                i % 60,
                i % 1000,
                i % 16,
                i,
                i % 97,
                i * 31 % 10_000
            )
            .bytes(),
        );
        i += 1;
    }
    v
}

fn dense_screen(t: &mut PaneTerminal) {
    t.feed(b"\x1b[H");
    for r in 0..ROWS {
        let mut line = String::new();
        for c in 0..COLS / 10 {
            line.push_str(&format!(
                "\x1b[3{};4{}m{:<9} ",
                (r + c) % 8,
                (r * 3 + c) % 8,
                format!("r{r}c{c}")
            ));
        }
        t.feed(format!("\x1b[{};1H{line}\x1b[0m", r + 1).as_bytes());
    }
}

fn main() {
    println!("civet-term bench, {COLS}x{ROWS}");

    // --- feed throughput -------------------------------------------------
    let log = colored_log(32 << 20);
    let mut t = term();
    let start = Instant::now();
    for c in log.chunks(65_536) {
        black_box(t.feed(c));
    }
    let el = start.elapsed();
    println!(
        "feed colored log: {:.1} MB/s ({} MB, {} history lines)",
        log.len() as f64 / el.as_secs_f64() / 1e6,
        log.len() >> 20,
        t.history_lines()
    );

    let plain: Vec<u8> = (0..400_000)
        .flat_map(|i| {
            format!("plain text line number {i} lorem ipsum dolor sit amet\r\n").into_bytes()
        })
        .collect();
    let mut t = term();
    let start = Instant::now();
    for c in plain.chunks(65_536) {
        black_box(t.feed(c));
    }
    let el = start.elapsed();
    println!(
        "feed plain text:  {:.1} MB/s",
        plain.len() as f64 / el.as_secs_f64() / 1e6
    );

    // --- keystroke: feed + snapshot capture -------------------------------
    let mut t = term();
    dense_screen(&mut t);
    let mut k = 0u8;
    let d = time(|| {
        k = k.wrapping_add(1);
        black_box(t.feed(&[b'\x1b', b'[', b'3', b'0', b';', b'1', b'H', b'a' + k % 26]));
    });
    println!("feed 1 keystroke + snapshot (dense screen): {d:?}");

    // --- diff: a few changed cells ----------------------------------------
    let mut t = term();
    dense_screen(&mut t);
    let s1 = t.snapshot();
    t.feed(b"\x1b[10;10HXYZ\x1b[40;150H\x1b[1;31m!\x1b[0m\x1b[60;1H$ ");
    let s2 = t.snapshot();
    let upd = encode_diff(&s1, &s2, t.scrollback());
    let d = time(|| {
        black_box(encode_diff(&s1, &s2, t.scrollback()));
    });
    println!(
        "encode_diff few cells:     {d:?} ({} bytes)",
        upd.payload.len()
    );

    // --- diff: cursor move only -------------------------------------------
    t.feed(b"\x1b[5;5H");
    let s3 = t.snapshot();
    let d = time(|| {
        black_box(encode_diff(&s2, &s3, t.scrollback()));
    });
    println!("encode_diff cursor only:   {d:?}");

    // --- diff: every row changed (vim page down) --------------------------
    let mut t = term();
    dense_screen(&mut t);
    let s1 = t.snapshot();
    t.feed(b"\x1b[H");
    for r in 0..ROWS {
        t.feed(
            format!(
                "\x1b[{};1H\x1b[33m{:>4}\x1b[0m fn page_two_{r}() {{ let value = {}; }}\x1b[K",
                r + 1,
                r + 100,
                r * 7
            )
            .as_bytes(),
        );
    }
    let s2 = t.snapshot();
    let upd = encode_diff(&s1, &s2, t.scrollback());
    let d = time(|| {
        black_box(encode_diff(&s1, &s2, t.scrollback()));
    });
    println!(
        "encode_diff full page:     {d:?} ({} bytes)",
        upd.payload.len()
    );

    // --- diff: one line scrolled (log tail) -------------------------------
    let mut t = term();
    let log = colored_log(200_000);
    t.feed(&log);
    let s1 = t.snapshot();
    t.feed(b"\x1b[32m[INFO]\x1b[0m one more line\r\n");
    let s2 = t.snapshot();
    let upd = encode_diff(&s1, &s2, t.scrollback());
    let d = time(|| {
        black_box(encode_diff(&s1, &s2, t.scrollback()));
    });
    println!(
        "encode_diff 1-line scroll: {d:?} ({} bytes)",
        upd.payload.len()
    );

    // --- diff: 30 lines scrolled (general replay path) ---------------------
    let mut t = term();
    t.feed(&log);
    let s1 = t.snapshot();
    t.feed(&colored_log(30 * 100)[..]);
    let s2 = t.snapshot();
    let upd = encode_diff(&s1, &s2, t.scrollback());
    let d = time(|| {
        black_box(encode_diff(&s1, &s2, t.scrollback()));
    });
    println!(
        "encode_diff {}-line scroll: {d:?} ({} bytes)",
        s2.history_lines() - s1.history_lines(),
        upd.payload.len()
    );

    // --- full repaint ------------------------------------------------------
    let mut t = term();
    dense_screen(&mut t);
    let s = t.snapshot();
    let upd = encode_full(&s, t.scrollback());
    let d = time(|| {
        black_box(encode_full(&s, t.scrollback()));
    });
    println!(
        "encode_full dense screen (no history): {d:?} ({} bytes)",
        upd.payload.len()
    );
    let mut t = term();
    t.feed(&colored_log(400_000));
    dense_screen(&mut t);
    let s = t.snapshot();
    let upd = encode_full(&s, t.scrollback());
    let d = time(|| {
        black_box(encode_full(&s, t.scrollback()));
    });
    println!(
        "encode_full dense screen + 1000 history lines: {d:?} ({} bytes)",
        upd.payload.len()
    );
}
