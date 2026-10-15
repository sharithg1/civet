//! Core property: for any byte stream fed to terminal A, applying
//! `encode_diff(N → now)` to a terminal B that shows state N reproduces A's
//! visible screen (cells + attributes + cursor + modes + title), and
//! `encode_full` reproduces it from scratch.

mod common;

use std::sync::Arc;

use civet_proto::TermSize;
use civet_term::{PaneTerminal, Snapshot, encode_diff, encode_full};
use common::{LineRenderer, apply, assert_same, client_from_full, term};
use proptest::prelude::*;

#[derive(Debug, Clone)]
enum Op {
    /// Bytes, fed in pieces split at the given (relative) points.
    Bytes(Vec<u8>, Vec<usize>),
    Resize(u16, u16),
    /// Compare; the selector picks continuous / from-older / full.
    Check(u8),
}

fn s(x: &str) -> Vec<u8> {
    x.as_bytes().to_vec()
}

fn text() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(
        prop_oneof![
            6 => "[a-zA-Z0-9 .,:/_-]{1,12}".prop_map(|t| s(&t)),
            1 => Just(s("漢字")),
            1 => Just(s("😀")),
            1 => Just(s("e\u{301}")),
            1 => Just(s("👍🏽")),
            1 => Just(s("\u{2764}\u{fe0f}")),
            1 => Just(s("ｗｉｄｅ")),
            1 => Just(s("─│┌┐")),
        ],
        1..5,
    )
    .prop_map(|v| v.concat())
}

fn color(base: u32) -> impl Strategy<Value = String> {
    prop_oneof![
        (0u32..8).prop_map(move |n| format!("{}", base + n)),
        (0u32..8).prop_map(move |n| format!("{}", base + 60 + n)),
        (0u32..256).prop_map(move |n| format!("{};5;{}", base + 8, n)),
        (any::<u8>(), any::<u8>(), any::<u8>()).prop_map(move |(r, g, b)| format!(
            "{};2;{};{};{}",
            base + 8,
            r,
            g,
            b
        )),
        Just(format!("{}", base + 9)),
    ]
}

fn sgr() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(
        prop_oneof![
            Just("0".to_string()),
            Just("1".to_string()),
            Just("2".to_string()),
            Just("3".to_string()),
            Just("4".to_string()),
            Just("4:2".to_string()),
            Just("4:3".to_string()),
            Just("4:4".to_string()),
            Just("4:5".to_string()),
            Just("5".to_string()),
            Just("7".to_string()),
            Just("8".to_string()),
            Just("9".to_string()),
            Just("21".to_string()),
            Just("22".to_string()),
            Just("23".to_string()),
            Just("24".to_string()),
            Just("27".to_string()),
            Just("28".to_string()),
            Just("29".to_string()),
            Just("59".to_string()),
            color(30),
            color(40),
            (0u32..256).prop_map(|n| format!("58;5;{n}")),
            (any::<u8>(), any::<u8>(), any::<u8>())
                .prop_map(|(r, g, b)| format!("58;2;{r};{g};{b}")),
        ],
        1..4,
    )
    .prop_map(|v| format!("\x1b[{}m", v.join(";")).into_bytes())
}

fn control() -> impl Strategy<Value = Vec<u8>> {
    let n = 0u16..40;
    prop_oneof![
        Just(s("\r")),
        Just(s("\n")),
        Just(s("\r\n")),
        Just(s("\t")),
        Just(s("\x08")),
        Just(s("\x0b")),
        (0u16..30, 0u16..50).prop_map(|(r, c)| format!("\x1b[{r};{c}H").into_bytes()),
        (0u16..30).prop_map(|r| format!("\x1b[{r}d").into_bytes()),
        (0u16..50).prop_map(|c| format!("\x1b[{c}G").into_bytes()),
        n.clone().prop_map(|k| format!("\x1b[{k}A").into_bytes()),
        n.clone().prop_map(|k| format!("\x1b[{k}B").into_bytes()),
        n.clone().prop_map(|k| format!("\x1b[{k}C").into_bytes()),
        n.clone().prop_map(|k| format!("\x1b[{k}D").into_bytes()),
        (0u8..3).prop_map(|k| format!("\x1b[{k}K").into_bytes()),
        (0u8..4).prop_map(|k| format!("\x1b[{k}J").into_bytes()),
        n.clone().prop_map(|k| format!("\x1b[{k}X").into_bytes()),
        n.clone().prop_map(|k| format!("\x1b[{k}P").into_bytes()),
        n.clone().prop_map(|k| format!("\x1b[{k}@").into_bytes()),
        (0u16..8).prop_map(|k| format!("\x1b[{k}L").into_bytes()),
        (0u16..8).prop_map(|k| format!("\x1b[{k}M").into_bytes()),
        (0u16..8).prop_map(|k| format!("\x1b[{k}S").into_bytes()),
        (0u16..8).prop_map(|k| format!("\x1b[{k}T").into_bytes()),
        (0u16..60).prop_map(|k| format!("x\x1b[{k}b").into_bytes()),
        (0u16..20, 0u16..30).prop_map(|(t, b)| format!("\x1b[{t};{b}r").into_bytes()),
        Just(s("\x1b[r")),
        Just(s("\x1bM")),
        Just(s("\x1bD")),
        Just(s("\x1bE")),
        Just(s("\x1b7")),
        Just(s("\x1b8")),
        Just(s("\x1b[s")),
        Just(s("\x1b[u")),
        Just(s("\x1b[4h")),
        Just(s("\x1b[4l")),
        Just(s("\x1b(0")),
        Just(s("\x1b(B")),
        Just(s("\x1b#8")),
    ]
}

fn modes() -> impl Strategy<Value = Vec<u8>> {
    let hl = prop_oneof![Just('h'), Just('l')];
    (
        prop_oneof![
            Just(1u16),
            Just(7),
            Just(25),
            Just(1000),
            Just(1002),
            Just(1003),
            Just(1004),
            Just(1006),
            Just(1049),
            Just(2004),
            Just(12),
            Just(6),
        ],
        hl,
    )
        .prop_map(|(m, c)| format!("\x1b[?{m}{c}").into_bytes())
}

fn misc() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        Just(s("\x1b=")),
        Just(s("\x1b>")),
        (0u8..7).prop_map(|k| format!("\x1b[{k} q").into_bytes()),
        "[a-z ]{0,10}".prop_map(|t| format!("\x1b]2;{t}\x07").into_bytes()),
        "[a-z ]{0,10}".prop_map(|t| format!("\x1b]0;{t}\x1b\\").into_bytes()),
        Just(s("\x1b[22;0t")),
        Just(s("\x1b[23;0t")),
        "[a-z]{1,8}".prop_map(|u| format!("\x1b]8;;https://{u}\x1b\\").into_bytes()),
        Just(s("\x1b]8;;\x1b\\")),
        Just(s("\x1b]9;hello\x07")),
        Just(s("\x1b]133;A\x07")),
        Just(s("\x1b[6n")),
        Just(s("\x1bc")),
        Just(s("\x1b[!p")),
        Just(s("\x07")),
    ]
}

/// A vim-like full-screen redraw.
fn redraw() -> impl Strategy<Value = Vec<u8>> {
    (prop::collection::vec(text(), 1..12), sgr(), any::<bool>()).prop_map(|(lines, a, alt)| {
        let mut v = Vec::new();
        if alt {
            v.extend(b"\x1b[?1049h");
        }
        v.extend(b"\x1b[?25l\x1b[H");
        for (i, l) in lines.iter().enumerate() {
            v.extend(format!("\x1b[{};1H", i + 1).bytes());
            if i % 3 == 0 {
                v.extend(&a);
            }
            v.extend(l);
            v.extend(b"\x1b[K\x1b[0m");
        }
        v.extend(b"\x1b[?25h");
        v
    })
}

/// Log-like output that scrolls (with wrapping lines).
fn log_lines() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec((text(), sgr(), 0usize..4), 1..30).prop_map(|v| {
        let mut out = Vec::new();
        for (t, a, rep) in v {
            out.extend(a);
            for _ in 0..=rep {
                out.extend(&t);
            }
            out.extend(b"\x1b[0m\r\n");
        }
        out
    })
}

fn chunk() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(
        prop_oneof![
            8 => text(),
            4 => sgr(),
            6 => control(),
            2 => modes(),
            2 => misc(),
            1 => redraw(),
            2 => log_lines(),
            1 => prop::collection::vec(any::<u8>(), 0..24),
        ],
        1..12,
    )
    .prop_map(|v| v.concat())
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        10 => (chunk(), prop::collection::vec(any::<usize>(), 0..3))
            .prop_map(|(b, sp)| Op::Bytes(b, sp)),
        1 => (2u16..50, 1u16..16).prop_map(|(c, r)| Op::Resize(c, r)),
        6 => any::<u8>().prop_map(Op::Check),
    ]
}

fn feed_split(a: &mut PaneTerminal, bytes: &[u8], splits: &[usize]) {
    let mut points: Vec<usize> = splits
        .iter()
        .map(|p| if bytes.is_empty() { 0 } else { p % bytes.len() })
        .collect();
    points.sort_unstable();
    let mut last = 0;
    for p in points {
        a.feed(&bytes[last..p]);
        last = p;
    }
    a.feed(&bytes[last..]);
}

/// Client B plus the server snapshot it is supposed to show.
struct Client {
    term: PaneTerminal,
    at: Arc<Snapshot>,
    /// A's absolute history line corresponding to B's line 0, while B's
    /// history is still comparable (no resize, no gap).
    hist_origin: Option<u64>,
}

fn check_history(a: &PaneTerminal, c: &Client) {
    let Some(origin) = c.hist_origin else {
        return;
    };
    let bs = c.term.scrollback();
    let a_store = a.scrollback();
    let mut r = LineRenderer::new();
    for (i, bl) in bs.range(bs.oldest(), bs.end()) {
        let n = origin + i;
        if n >= c.at.history_lines() {
            panic!(
                "client has more history ({}) than server state ({})",
                origin + bs.end(),
                c.at.history_lines()
            );
        }
        if let Some(al) = a_store.get(n) {
            assert_eq!(
                r.cells(&al.vt),
                r.cells(&bl.vt),
                "history line {n} differs: {:?} vs {:?}",
                String::from_utf8_lossy(&al.vt),
                String::from_utf8_lossy(&bl.vt)
            );
        }
    }
    assert_eq!(
        origin + bs.end(),
        c.at.history_lines(),
        "client history count"
    );
}

fn run(cols: u16, rows: u16, ops: &[Op]) {
    let mut a = term(cols, rows);
    let mut client: Option<Client> = None;
    let mut taken: Vec<Arc<Snapshot>> = Vec::new();
    let mut last_state = a.state();
    // History lines pushed at another width replay as a different number
    // of client rows, so history is only compared while A was never resized.
    let mut a_resized = false;
    for (idx, op) in ops.iter().enumerate() {
        match op {
            Op::Bytes(b, sp) => feed_split(&mut a, b, sp),
            Op::Resize(c, r) => {
                a_resized |= a.resize(TermSize::new(*c, *r)).unwrap();
            }
            Op::Check(sel) => {
                let to = a.snapshot();
                assert!(to.state() >= last_state, "state went backwards");
                last_state = to.state();
                let ctx = format!("op {idx} sel {sel}");
                match sel % 4 {
                    0 | 1 => match client.as_mut() {
                        Some(c) => {
                            let upd = encode_diff(&c.at, &to, a.scrollback());
                            assert_eq!(upd.base_state, c.at.state());
                            assert_eq!(upd.state, to.state());
                            let resized = (c.at.size().cols, c.at.size().rows)
                                != (to.size().cols, to.size().rows);
                            if a_resized || resized || upd.history_start != c.at.history_lines() {
                                c.hist_origin = None;
                            }
                            apply(&mut c.term, &upd);
                            c.at = to.clone();
                            assert_same(&to, &c.term.snapshot(), &format!("{ctx} continuous"));
                            check_history(&a, c);
                        }
                        None => {
                            let upd = encode_full(&to, a.scrollback());
                            let mut b = term(to.size().cols, to.size().rows);
                            apply(&mut b, &upd);
                            assert_same(&to, &b.snapshot(), &format!("{ctx} first full"));
                            let c = Client {
                                hist_origin: (!a_resized).then_some(upd.history_start),
                                term: b,
                                at: to.clone(),
                            };
                            check_history(&a, &c);
                            client = Some(c);
                        }
                    },
                    2 if !taken.is_empty() => {
                        let older = taken[usize::from(*sel / 4) % taken.len()].clone();
                        let mut b = client_from_full(&a, &older);
                        assert_same(&older, &b.snapshot(), &format!("{ctx} older full"));
                        let upd = encode_diff(&older, &to, a.scrollback());
                        apply(&mut b, &upd);
                        assert_same(&to, &b.snapshot(), &format!("{ctx} from older"));
                    }
                    _ => {
                        let mut b = client_from_full(&a, &to);
                        assert_same(&to, &b.snapshot(), &format!("{ctx} full"));
                    }
                }
                taken.push(to);
            }
        }
    }
    // Always end with a check of both paths.
    let to = a.snapshot();
    if let Some(c) = client.as_mut() {
        let upd = encode_diff(&c.at, &to, a.scrollback());
        apply(&mut c.term, &upd);
        c.at = to.clone();
        assert_same(&to, &c.term.snapshot(), "final continuous");
    }
    let mut b = client_from_full(&a, &to);
    assert_same(&to, &b.snapshot(), "final full");
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 512,
        max_shrink_iters: 4096,
        ..ProptestConfig::default()
    })]

    #[test]
    fn diff_reproduces_screen(
        cols in 2u16..48,
        rows in 1u16..16,
        ops in prop::collection::vec(op(), 1..40),
    ) {
        run(cols, rows, &ops);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// Realistic sizes, longer streams.
    #[test]
    fn diff_reproduces_screen_large(
        ops in prop::collection::vec(op(), 20..80),
    ) {
        run(120, 40, &ops);
    }
}
