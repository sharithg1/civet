//! Feed-side behavior: state numbering, replies, OSC reporting, title.

mod common;

use civet_term::{CursorShape, OscNotification, PromptMark};
use common::term;

#[test]
fn state_starts_at_one_and_bumps_once_per_changing_feed() {
    let mut t = term(20, 5);
    assert_eq!(t.state(), 1);
    let s = t.snapshot();
    assert!(std::sync::Arc::ptr_eq(&s, &t.snapshot()), "cached");
    let o = t.feed(b"a\r\nb\r\nc");
    assert!(o.changed);
    assert_eq!(t.state(), 2);
    // No visible change: SGR only, DSR query, empty input.
    assert!(!t.feed(b"\x1b[31m").changed);
    assert!(!t.feed(b"").changed);
    let o = t.feed(b"\x1b[5n");
    assert!(!o.changed);
    assert_eq!(o.pty_replies, b"\x1b[0n");
    assert_eq!(t.state(), 2);
    assert!(t.feed(b"\x1b[?25l").changed);
    assert_eq!(t.state(), 3);
    assert!(!t.cursor().visible);
}

#[test]
fn cursor_position_report() {
    let mut t = term(20, 5);
    let o = t.feed(b"\x1b[3;7H\x1b[6n");
    assert_eq!(o.pty_replies, b"\x1b[3;7R");
    let c = t.cursor();
    assert_eq!((c.row, c.col), (2, 6));
}

#[test]
fn device_attributes_and_version() {
    let mut t = term(20, 5);
    let o = t.feed(b"\x1b[c");
    assert!(o.pty_replies.starts_with(b"\x1b[?"));
    let o = t.feed(b"\x1b[>q");
    assert!(o.pty_replies.starts_with(b"\x1bP>|civet "));
    let o = t.feed(b"\x1b[?2004$p");
    assert!(!o.pty_replies.is_empty(), "DECRQM answered");
}

#[test]
fn color_queries() {
    let mut t = term(20, 5);
    let o = t.feed(b"\x1b]11;?\x07");
    let r = String::from_utf8(o.pty_replies).unwrap();
    assert!(r.starts_with("\x1b]11;rgb:"), "{r:?}");
    let o = t.feed(b"\x1b]10;?\x1b\\");
    assert!(
        String::from_utf8(o.pty_replies)
            .unwrap()
            .starts_with("\x1b]10;rgb:")
    );
}

#[test]
fn osc_notifications_marks_cwd() {
    let mut t = term(20, 5);
    let o = t.feed(
        b"\x1b]9;build done\x07\x1b]9;4;1;50\x07\x1b]777;notify;Claude;needs input\x1b\\\
          \x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;2\x07\x1b]133;D\x07\
          \x1b]7;file://box/home/me/my%20repo\x07",
    );
    assert_eq!(
        o.notifications,
        vec![
            OscNotification {
                title: None,
                body: "build done".into()
            },
            OscNotification {
                title: Some("Claude".into()),
                body: "needs input".into()
            },
        ]
    );
    assert_eq!(
        o.prompt_marks,
        vec![
            PromptMark::PromptStart,
            PromptMark::CommandStart,
            PromptMark::CommandExecuted,
            PromptMark::CommandFinished { exit_code: Some(2) },
            PromptMark::CommandFinished { exit_code: None },
        ]
    );
    assert_eq!(o.cwd.as_deref(), Some("/home/me/my repo"));
    assert!(!o.bell, "BEL terminating an OSC is not a bell");
    assert!(t.feed(b"\x07").bell);
}

#[test]
fn title_osc_stack_and_ris() {
    let mut t = term(20, 5);
    let o = t.feed(b"\x1b]0;one\x07");
    assert_eq!(o.title.as_deref(), Some("one"));
    assert!(o.changed);
    assert_eq!(t.title(), "one");
    t.feed(b"\x1b[22;0t\x1b]2;two;with;semis\x1b\\");
    assert_eq!(t.title(), "two;with;semis");
    let o = t.feed(b"\x1b[23;0t");
    assert_eq!(o.title.as_deref(), Some("one"));
    let o = t.feed(b"\x1bc");
    assert_eq!(o.title.as_deref(), Some(""));
    assert!(t.feed(b"\x1b]2;same\x07").title.is_some());
    assert!(t.feed(b"\x1b]2;same\x07").title.is_none());
}

#[test]
fn alt_screen_and_cursor_shape() {
    let mut t = term(20, 5);
    let o = t.feed(b"\x1b[?1049h\x1b[5 q");
    assert_eq!(o.alt_screen_changed, Some(true));
    assert!(t.alt_screen());
    assert_eq!(t.cursor().shape, CursorShape::BlinkingBar);
    let o = t.feed(b"\x1b[?1049l\x1b[?1049h\x1b[?1049l");
    assert_eq!(o.alt_screen_changed, Some(false));
    let o = t.feed(b"x");
    assert_eq!(o.alt_screen_changed, None);
}

#[test]
fn synchronized_update_is_applied_immediately() {
    let mut t = term(20, 5);
    t.feed(b"\x1b[?2026hhello");
    assert_eq!(t.screen_text()[0], "hello");
    t.feed(b" world\x1b[?2026l");
    assert_eq!(t.screen_text()[0], "hello world");
}

#[test]
fn feed_throughput_sanity_on_garbage() {
    // Arbitrary bytes never panic and always keep a consistent snapshot.
    let mut t = term(7, 3);
    let mut x: u32 = 12345;
    let mut buf = Vec::new();
    for _ in 0..200_000 {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        buf.push((x & 0xff) as u8);
    }
    for c in buf.chunks(997) {
        t.feed(c);
    }
    let s = t.snapshot();
    assert_eq!(s.screen_text().len(), 3);
}

#[test]
fn pane_terminal_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<civet_term::PaneTerminal>();
    assert_send::<std::sync::Arc<civet_term::Snapshot>>();
}

#[test]
fn osc52_set_yields_clipboard_text() {
    let mut t = term(20, 5);
    // "hello ✓" base64, BEL- and ST-terminated, `c` and empty selectors.
    let o = t.feed(b"\x1b]52;c;aGVsbG8g4pyT\x07");
    assert_eq!(o.clipboard.as_deref(), Some("hello ✓"));
    assert!(o.pty_replies.is_empty());
    assert!(!o.changed, "a clipboard write is not a screen change");
    let o = t.feed(b"\x1b]52;;Zm9v\x1b\\");
    assert_eq!(o.clipboard.as_deref(), Some("foo"));
    // Split across feeds, several in one call: the last wins.
    assert!(t.feed(b"\x1b]52;c;YW").clipboard.is_none());
    let o = t.feed(b"Jj\x07\x1b]52;c;YmFy\x07");
    assert_eq!(o.clipboard.as_deref(), Some("bar"));
    // Nothing on plain output.
    assert!(t.feed(b"x").clipboard.is_none());
}

#[test]
fn osc52_query_is_ignored() {
    let mut t = term(20, 5);
    t.feed(b"\x1b]52;c;c2VjcmV0\x07");
    for q in [
        &b"\x1b]52;c;?\x07"[..],
        b"\x1b]52;p;?\x1b\\",
        b"\x1b]52;;?\x07",
    ] {
        let o = t.feed(q);
        assert!(o.pty_replies.is_empty(), "query must not be answered");
        assert!(o.clipboard.is_none());
    }
}

#[test]
fn osc52_oversized_or_invalid_is_dropped() {
    let mut t = term(20, 5);
    let big = vec![b'A'; (civet_proto::MAX_CLIPBOARD_BYTES / 3 + 1) * 4];
    let mut seq = b"\x1b]52;c;".to_vec();
    seq.extend_from_slice(&big);
    seq.push(0x07);
    assert!(t.feed(&seq).clipboard.is_none());
    assert!(t.feed(b"\x1b]52;c;!!notbase64\x07").clipboard.is_none());
}
