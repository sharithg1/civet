//! One attached (or recently attached) pane: client-side mirror and
//! predictor, record encoding, and the batching pump that feeds the UI's
//! binary channel.
//!
//! Views outlive their channel: a detached or reloaded UI leaves the view
//! subscribed (for a linger period after `detach_pane`, until the next
//! attach after a webview reload), with the mirror kept current. Re-attaching
//! then paints a Full from the mirror instantly instead of waiting for the
//! daemon (the client-side session cache).

use std::sync::Arc;

use civet_proto::{PaneId, ScreenUpdate, SubscribeInfo, TermSize, UpdateKind};
use tokio::runtime::Handle;
use tokio::sync::mpsc;

use crate::batch::{Batcher, PumpMsg, RecordSink, run_pump};
use crate::predict::Predictor;
use crate::records::{RecordHeader, RecordKind, encode, flags};

/// Per-pane state shared by the host task (screen updates) and commands
/// (attach, input). Kept behind a mutex by the host.
#[derive(Debug)]
pub struct PaneView {
    pane: PaneId,
    predictor: Predictor,
    server_state: u64,
    acked_input_seq: u64,
    history_lines: u64,
    /// Oldest absolute history line the mirror holds without a gap.
    contiguous_from: u64,
    /// Size of the last record on the current channel (RESIZED flag).
    last_sent_size: Option<TermSize>,
    attached: bool,
    attach_gen: u64,
    subscribed: bool,
    closed: bool,
    pump: mpsc::UnboundedSender<PumpMsg>,
}

impl PaneView {
    /// New view; spawns its pump on `rt`.
    pub fn new(pane: PaneId, size: TermSize, rt: &Handle) -> Self {
        let (pump, rx) = mpsc::unbounded_channel();
        rt.spawn(run_pump(rx, Batcher::default()));
        Self {
            pane,
            predictor: Predictor::new(size),
            server_state: 0,
            acked_input_seq: 0,
            history_lines: 0,
            contiguous_from: 0,
            last_sent_size: None,
            attached: false,
            attach_gen: 0,
            subscribed: false,
            closed: false,
            pump,
        }
    }

    /// Pane id.
    pub fn pane(&self) -> PaneId {
        self.pane
    }

    /// Subscribed on the session (screen updates flowing).
    pub fn is_subscribed(&self) -> bool {
        self.subscribed
    }

    /// Mark subscribed after `ClientSession::subscribe` succeeded.
    pub fn set_subscribed(&mut self, on: bool) {
        self.subscribed = on;
    }

    /// A UI channel is attached.
    pub fn is_attached(&self) -> bool {
        self.attached
    }

    /// Attach generation (changes on every attach/detach).
    pub fn attach_gen(&self) -> u64 {
        self.attach_gen
    }

    /// The predictor (tests, diagnostics).
    pub fn predictor(&self) -> &Predictor {
        &self.predictor
    }

    /// What `attach_pane` returns for a cached view.
    pub fn info(&self) -> SubscribeInfo {
        SubscribeInfo {
            pane: self.pane,
            state: self.server_state,
            size: self.predictor.mirror().size(),
            history_lines: self.history_lines,
            last_input_seq: self.acked_input_seq,
        }
    }

    fn header(&self, kind: RecordKind) -> RecordHeader {
        let size = self.predictor.mirror().size();
        RecordHeader {
            kind,
            flags: 0,
            cols: size.cols,
            rows: size.rows,
            state: self.server_state,
            acked_input_seq: self.acked_input_seq,
            history_start: self.history_lines,
            history_lines: self.history_lines,
        }
    }

    fn send(&mut self, msg: PumpMsg) {
        let _ = self.pump.send(msg);
    }

    fn send_overlay(&mut self, payload: Vec<u8>) {
        if self.attached {
            let rec = encode(&self.header(RecordKind::Predict), &payload);
            self.send(PumpMsg::Records(rec));
        }
    }

    /// Attach a UI channel (replacing any previous one). For a cached view
    /// the first record (a Full painted from the mirror) is queued at once
    /// and the cached `SubscribeInfo` is returned; otherwise the caller must
    /// subscribe and the daemon's Full will be the first record.
    pub fn attach(&mut self, sink: Arc<dyn RecordSink>) -> Option<SubscribeInfo> {
        self.attach_gen += 1;
        self.attached = true;
        self.last_sent_size = None;
        self.send(PumpMsg::SetSink(Some(sink)));
        if self.closed {
            let rec = encode(&RecordHeader::new(RecordKind::PaneClosed), &[]);
            self.send(PumpMsg::Urgent(rec));
            return Some(self.info());
        }
        if !(self.subscribed && self.predictor.has_screen()) {
            return None;
        }
        let (payload, replayed) = self.predictor.full_repaint();
        let mut h = self.header(RecordKind::Screen);
        h.flags = flags::FULL | flags::RESIZED;
        h.history_start = self
            .history_lines
            .saturating_sub(replayed)
            .max(self.contiguous_from);
        self.last_sent_size = Some(self.predictor.mirror().size());
        self.send(PumpMsg::Urgent(encode(&h, &payload)));
        Some(self.info())
    }

    /// Drop the UI channel; the view keeps mirroring. Returns the new attach
    /// generation (for the linger timer).
    pub fn detach(&mut self) -> u64 {
        self.attach_gen += 1;
        self.attached = false;
        self.send(PumpMsg::SetSink(None));
        self.attach_gen
    }

    /// A validated screen update from the session.
    pub fn on_screen(&mut self, u: &ScreenUpdate) {
        let gap = u.kind == UpdateKind::Diff && u.history_start > self.history_lines;
        let payload = self.predictor.apply_server(u);
        if u.kind == UpdateKind::Full || gap {
            self.contiguous_from = u.history_start;
        }
        self.server_state = u.state;
        self.acked_input_seq = self.acked_input_seq.max(u.acked_input_seq);
        self.history_lines = u.history_lines;
        if !self.attached {
            return;
        }
        let mut h = self.header(RecordKind::Screen);
        h.cols = u.size.cols;
        h.rows = u.size.rows;
        h.history_start = u.history_start;
        if u.kind == UpdateKind::Full {
            h.flags |= flags::FULL;
        }
        if self.last_sent_size != Some(u.size) {
            h.flags |= flags::RESIZED;
        }
        if gap {
            h.flags |= flags::HISTORY_GAP;
        }
        self.last_sent_size = Some(u.size);
        self.send(PumpMsg::Records(encode(&h, &payload)));
    }

    /// The daemon applied input up to `seq`.
    pub fn on_input_ack(&mut self, seq: u64) {
        self.acked_input_seq = self.acked_input_seq.max(seq);
        if let Some(overlay) = self.predictor.on_ack(seq) {
            self.send_overlay(overlay);
        }
        if self.attached {
            let rec = encode(&self.header(RecordKind::InputAck), &[]);
            self.send(PumpMsg::Records(rec));
        }
    }

    /// Input `data` was sent with `seq`; draw predictions if `show`.
    pub fn on_input(&mut self, seq: u64, data: &[u8], show: bool) {
        if let Some(overlay) = self.predictor.on_input(seq, data, show) {
            self.send_overlay(overlay);
        }
    }

    /// The session reconnected without resuming (`Connected { resumed:
    /// false }`): the daemon numbers this client's input from 1 again, so
    /// pending guesses (keyed by old seqs) can never be confirmed and the
    /// acked seq must restart at 0.
    pub fn reset_input(&mut self) {
        self.acked_input_seq = 0;
        if let Some(undo) = self.predictor.clear() {
            self.send_overlay(undo);
        }
    }

    /// Highest input seq the daemon acknowledged for this view.
    pub fn acked_input_seq(&self) -> u64 {
        self.acked_input_seq
    }

    /// Prediction was switched off: undo visible guesses.
    pub fn clear_predictions(&mut self) {
        if let Some(undo) = self.predictor.clear() {
            self.send_overlay(undo);
        }
    }

    /// `resize_pane` succeeded. If the mirror is already at `size` the
    /// daemon will not send a resized screen (nothing changed), so tell the
    /// UI the authoritative size with an empty overlay record: the UI never
    /// resizes xterm on its own.
    pub fn after_resize(&mut self, size: TermSize) {
        let size = size.sanitized();
        if self.attached && self.predictor.has_screen() && self.predictor.mirror().size() == size {
            self.last_sent_size = Some(size);
            let rec = encode(&self.header(RecordKind::Predict), &[]);
            self.send(PumpMsg::Records(rec));
        }
    }

    /// The pane is gone: tell the UI (kind 3).
    pub fn close(&mut self) {
        self.closed = true;
        self.subscribed = false;
        if self.attached {
            let rec = encode(&RecordHeader::new(RecordKind::PaneClosed), &[]);
            self.send(PumpMsg::Urgent(rec));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::decode_all;
    use civet_term::{PaneTerminal, TermConfig};
    use std::time::Duration;

    struct Server {
        term: PaneTerminal,
        sent: Option<Arc<civet_term::Snapshot>>,
    }

    impl Server {
        fn new(size: TermSize) -> Self {
            Self {
                term: PaneTerminal::new(TermConfig {
                    size,
                    scrollback_limit: 5000,
                }),
                sent: None,
            }
        }
        fn update(&mut self, acked: u64) -> ScreenUpdate {
            let enc = self.term.encode_update(self.sent.as_deref());
            self.sent = Some(self.term.snapshot());
            enc.into_screen_update(PaneId(7), acked)
        }
    }

    async fn drain(rx: &mut mpsc::UnboundedReceiver<Vec<u8>>) -> Vec<(RecordHeader, Vec<u8>)> {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            for (h, p) in decode_all(&msg).unwrap() {
                out.push((h, p.to_vec()));
            }
        }
        out
    }

    #[tokio::test]
    async fn records_flags_and_cached_reattach() {
        let size = TermSize::new(20, 4);
        let mut srv = Server::new(size);
        let mut v = PaneView::new(PaneId(7), size, &Handle::current());
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        assert!(v.attach(Arc::new(tx)).is_none(), "not cached yet");
        v.set_subscribed(true);

        srv.term.feed(b"hello");
        v.on_screen(&srv.update(0));
        // Scroll a lot so a diff caps the replay and leaves a gap.
        for i in 0..2500 {
            srv.term.feed(format!("line {i}\r\n").as_bytes());
        }
        v.on_screen(&srv.update(3));
        srv.term.resize(TermSize::new(30, 5)).unwrap();
        v.on_screen(&srv.update(3));
        v.on_input_ack(4);

        let recs = drain(&mut rx).await;
        let kinds: Vec<_> = recs.iter().map(|(h, _)| h.kind).collect();
        assert_eq!(
            kinds,
            [
                RecordKind::Screen,
                RecordKind::Screen,
                RecordKind::Screen,
                RecordKind::InputAck
            ]
        );
        assert_eq!(recs[0].0.flags, flags::FULL | flags::RESIZED);
        assert_eq!(recs[1].0.flags, flags::HISTORY_GAP);
        assert!(recs[1].0.history_start > 0);
        assert_eq!(recs[2].0.flags & flags::RESIZED, flags::RESIZED);
        assert_eq!((recs[2].0.cols, recs[2].0.rows), (30, 5));
        assert_eq!(recs[3].0.acked_input_seq, 4);

        // Re-attach (UI reload): instant Full from the mirror, then live
        // records continue on the new channel only.
        let (tx2, mut rx2) = mpsc::unbounded_channel::<Vec<u8>>();
        let info = v.attach(Arc::new(tx2)).expect("cached");
        assert_eq!(info.state, srv.term.state());
        assert_eq!(info.size, TermSize::new(30, 5));
        srv.term.feed(b"more");
        v.on_screen(&srv.update(4));
        let recs2 = drain(&mut rx2).await;
        assert_eq!(recs2[0].0.flags, flags::FULL | flags::RESIZED);
        assert_eq!(recs2[0].0.history_lines, srv.term.history_lines());
        assert!(recs2[0].0.history_start >= recs[1].0.history_start);
        let mut client = PaneTerminal::new(TermConfig {
            size: TermSize::new(30, 5),
            scrollback_limit: 10,
        });
        for (_, p) in &recs2 {
            client.feed(p);
        }
        assert_eq!(client.screen_text(), srv.term.screen_text());
        assert!(drain(&mut rx).await.is_empty(), "old channel gets nothing");

        // resize to the current size still yields a record with the size.
        v.after_resize(TermSize::new(30, 5));
        let r = drain(&mut rx2).await;
        assert_eq!(r.len(), 1);
        assert_eq!(
            (r[0].0.kind, r[0].0.cols, r[0].0.rows),
            (RecordKind::Predict, 30, 5)
        );

        v.close();
        let r = drain(&mut rx2).await;
        assert_eq!(r[0].0.kind, RecordKind::PaneClosed);
    }

    #[tokio::test]
    async fn reset_input_after_a_fresh_session() {
        let size = TermSize::new(20, 4);
        let mut srv = Server::new(size);
        let mut v = PaneView::new(PaneId(7), size, &Handle::current());
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        v.attach(Arc::new(tx));
        v.set_subscribed(true);
        srv.term.feed(b"$ ");
        v.on_screen(&srv.update(0));
        v.on_input_ack(41);
        assert_eq!(v.acked_input_seq(), 41);
        // A guess drawn for seq 42 (shown: RTT is high).
        v.on_input(42, b"x", true);
        assert_eq!(v.predictor().pending(), 1);
        drain(&mut rx).await;

        v.reset_input();
        assert_eq!(v.acked_input_seq(), 0);
        assert_eq!(v.predictor().pending(), 0);
        let r = drain(&mut rx).await;
        assert_eq!(r.len(), 1, "undo of the visible guess");
        assert_eq!(r[0].0.kind, RecordKind::Predict);
        assert_eq!(r[0].0.acked_input_seq, 0);
        // New numbering from 1 is acked normally.
        v.on_input_ack(1);
        assert_eq!(v.acked_input_seq(), 1);
    }
}
