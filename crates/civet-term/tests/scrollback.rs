//! Scrollback capture and paging by absolute line number.

mod common;

use civet_proto::{PaneId, ScrollbackLine, ScrollbackQuery, ScrollbackRequest};
use civet_term::ScrollbackStore;
use common::{line_text, term, term_with_limit};

fn req(query: ScrollbackQuery) -> ScrollbackRequest {
    ScrollbackRequest {
        req_id: 42,
        pane: PaneId(3),
        query,
    }
}

#[test]
fn seq_100000_is_contiguous() {
    let mut t = term_with_limit(80, 24, 200_000);
    let mut out = String::new();
    for i in 1..=100_000 {
        out.push_str(&i.to_string());
        out.push('\n');
    }
    // Feed in PTY-sized reads, as a shell running `seq 1 100000` would.
    let bytes = out.replace('\n', "\r\n").into_bytes();
    for c in bytes.chunks(65_536) {
        t.feed(c);
    }
    // 100000 lines + trailing newline: the screen shows the last 23 numbers
    // and an empty cursor row.
    let n = t.history_lines();
    assert_eq!(n, 100_000 - 23);
    let s = t.scrollback();
    assert_eq!(s.oldest(), 0);
    for (i, l) in s.range(0, n) {
        assert_eq!(line_text(&l.vt), (i + 1).to_string(), "line {i}");
        assert!(!l.wrapped);
    }
    assert_eq!(t.screen_text()[0], (n + 1).to_string());
}

#[test]
fn one_huge_feed_loses_nothing() {
    let mut t = term_with_limit(10, 5, 1_000_000);
    let mut out = Vec::new();
    for i in 0..300_000u32 {
        out.extend(format!("{i}\r\n").bytes());
    }
    t.feed(&out);
    assert_eq!(t.history_lines(), 300_000 - 4);
    let s = t.scrollback();
    for i in [0u64, 1, 4095, 4096, 150_000, 299_995] {
        assert_eq!(line_text(&s.get(i).unwrap().vt), i.to_string());
    }
}

#[test]
fn ris_and_clear_scrollback_do_not_drop_lines_in_same_chunk() {
    let mut t = term(10, 3);
    t.feed(b"a\r\nb\r\nc\r\nd\r\ne\x1bcx\r\ny\r\nz\r\nw\x1b[3Jq");
    let texts: Vec<String> = t
        .scrollback()
        .range(0, 100)
        .map(|(_, l)| line_text(&l.vt))
        .collect();
    assert_eq!(texts, vec!["a", "b", "x"]);
}

#[test]
fn alt_screen_lines_never_reach_history() {
    let mut t = term(10, 3);
    t.feed(b"1\r\n2\r\n3\r\n4");
    let before = t.history_lines();
    assert_eq!(before, 1);
    t.feed(b"\x1b[?1049h");
    for i in 0..100 {
        t.feed(format!("alt {i}\r\n").as_bytes());
    }
    t.feed(b"\x1b[?1049l\r\n5");
    assert_eq!(t.history_lines(), before + 1);
    let texts: Vec<String> = t
        .scrollback()
        .range(0, 100)
        .map(|(_, l)| line_text(&l.vt))
        .collect();
    assert_eq!(texts, vec!["1", "2"]);
}

#[test]
fn wrapped_lines_and_attributes() {
    let mut t = term(5, 2);
    t.feed(b"\x1b[1;31mabcdefgh\x1b[0m\r\nij\r\nk");
    let s = t.scrollback();
    let l0 = s.get(0).unwrap();
    assert!(l0.wrapped);
    assert_eq!(l0.vt, b"\x1b[1;31mabcde\x1b[0m");
    let l1 = s.get(1).unwrap();
    assert!(!l1.wrapped);
    assert_eq!(l1.vt, b"\x1b[1;31mfgh\x1b[0m");
    assert!(s.get(2).is_none());
    assert_eq!(t.screen_text(), vec!["ij", "k"]);
}

#[test]
fn paging_before_and_range() {
    let mut t = term_with_limit(20, 2, 50);
    for i in 0..121 {
        t.feed(format!("L{i}\r\n").as_bytes());
    }
    // 121 lines + empty row: 120 scrolled off, 50 retained (70..120).
    assert_eq!(t.history_lines(), 120);
    assert_eq!(t.scrollback().oldest(), 70);
    assert_eq!(t.scrollback().len(), 50);

    let p = t.scrollback_page(&req(ScrollbackQuery::Before {
        end: None,
        limit: 10,
    }));
    assert_eq!((p.req_id, p.pane), (42, PaneId(3)));
    assert_eq!(p.start, 110);
    assert_eq!(p.end(), 120);
    assert_eq!((p.oldest_available, p.end_available), (70, 120));
    assert_eq!(line_text(&p.lines[0].vt), "L110");
    assert!(p.has_older());

    // Page upward until exhausted.
    let mut end = p.start;
    let mut seen = 10;
    loop {
        let p = t.scrollback_page(&req(ScrollbackQuery::Before {
            end: Some(end),
            limit: 15,
        }));
        if p.lines.is_empty() {
            assert_eq!(p.start, 70);
            break;
        }
        assert_eq!(p.end(), end);
        for (k, l) in p.lines.iter().enumerate() {
            assert_eq!(line_text(&l.vt), format!("L{}", p.start + k as u64));
        }
        seen += p.lines.len();
        end = p.start;
    }
    assert_eq!(seen, 50);

    // Range clamps to what is retained.
    let p = t.scrollback_page(&req(ScrollbackQuery::Range { start: 0, end: 75 }));
    assert_eq!(p.start, 70);
    assert_eq!(p.lines.len(), 5);
    let p = t.scrollback_page(&req(ScrollbackQuery::Range {
        start: 118,
        end: 1_000,
    }));
    assert_eq!((p.start, p.lines.len()), (118, 2));
    let p = t.scrollback_page(&req(ScrollbackQuery::Range { start: 90, end: 80 }));
    assert!(p.lines.is_empty());
    // Before an evicted line → empty page at the oldest line.
    let p = t.scrollback_page(&req(ScrollbackQuery::Before {
        end: Some(10),
        limit: 5,
    }));
    assert!(p.lines.is_empty());
    assert_eq!(p.start, 70);
}

#[test]
fn store_eviction_and_numbering() {
    let mut s = ScrollbackStore::new(3);
    for i in 0..10u8 {
        s.push(ScrollbackLine {
            vt: vec![b'0' + i],
            wrapped: false,
        });
    }
    assert_eq!((s.oldest(), s.end(), s.len()), (7, 10, 3));
    assert!(s.get(6).is_none());
    assert_eq!(s.get(9).unwrap().vt, b"9");
    let r: Vec<u64> = s.range(0, 100).map(|(n, _)| n).collect();
    assert_eq!(r, vec![7, 8, 9]);
    let mut z = ScrollbackStore::new(0);
    z.push(ScrollbackLine {
        vt: vec![],
        wrapped: false,
    });
    assert_eq!((z.oldest(), z.end()), (1, 1));
}

#[test]
fn full_update_history_start_and_replay() {
    let mut t = term_with_limit(10, 3, 5000);
    for i in 0..1500 {
        t.feed(format!("{i}\r\n").as_bytes());
    }
    let s = t.snapshot();
    let upd = civet_term::encode_full(&s, t.scrollback());
    assert_eq!(s.history_lines(), 1498);
    assert_eq!(upd.history_start, 1498 - 1000);
    assert!(upd.payload.starts_with(b"\x1bc"));
    let mut c = term(10, 3);
    common::apply(&mut c, &upd);
    assert_eq!(c.history_lines(), 1000);
    assert_eq!(line_text(&c.scrollback().get(0).unwrap().vt), "498");
}

#[test]
fn clear_screen_pushes_visible_lines_like_alacritty() {
    // alacritty's ED 2 scrolls the non-empty part of the screen into
    // history (so `clear` keeps what was visible fetchable).
    let mut t = term(10, 4);
    t.feed(b"a\r\nb\x1b[H\x1b[2J");
    let texts: Vec<String> = t
        .scrollback()
        .range(0, 10)
        .map(|(_, l)| line_text(&l.vt))
        .collect();
    assert_eq!(texts, vec!["a", "b"]);
}
