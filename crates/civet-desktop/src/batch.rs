//! Per-pane channel batching (docs/INTERFACES.md §6.2): records are appended
//! to a buffer that is flushed when [`FLUSH_DELAY`] has passed since the
//! first unflushed record, or when it exceeds [`FLUSH_BYTES`], whichever
//! comes first. Frame-rate batching without visible latency for echoes.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;

/// Flush at most this long after the first unflushed record.
pub const FLUSH_DELAY: Duration = Duration::from_millis(4);
/// Flush as soon as the buffer exceeds this many bytes.
pub const FLUSH_BYTES: usize = 256 * 1024;

/// Where a pane's channel messages go (a Tauri `Channel` in the app, an mpsc
/// in tests).
pub trait RecordSink: Send + Sync + 'static {
    /// Deliver one message (one or more records). False if the receiver is
    /// gone.
    fn send(&self, message: Vec<u8>) -> bool;
}

impl fmt::Debug for dyn RecordSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RecordSink")
    }
}

impl RecordSink for mpsc::UnboundedSender<Vec<u8>> {
    fn send(&self, message: Vec<u8>) -> bool {
        mpsc::UnboundedSender::send(self, message).is_ok()
    }
}

/// Pure batching policy (no I/O, explicit clock) so it can be unit-tested.
#[derive(Debug)]
pub struct Batcher {
    buf: Vec<u8>,
    first_at: Option<Instant>,
    delay: Duration,
    max_bytes: usize,
}

impl Default for Batcher {
    fn default() -> Self {
        Self::new(FLUSH_DELAY, FLUSH_BYTES)
    }
}

impl Batcher {
    /// Custom limits.
    pub fn new(delay: Duration, max_bytes: usize) -> Self {
        Self {
            buf: Vec::new(),
            first_at: None,
            delay,
            max_bytes,
        }
    }

    /// Append encoded records. Returns a batch to send right away if the
    /// buffer now exceeds the size limit.
    pub fn push(&mut self, records: &[u8], now: Instant) -> Option<Vec<u8>> {
        if records.is_empty() {
            return None;
        }
        if self.first_at.is_none() {
            self.first_at = Some(now);
        }
        self.buf.extend_from_slice(records);
        if self.buf.len() > self.max_bytes {
            self.take()
        } else {
            None
        }
    }

    /// When the pending batch must be flushed, if any.
    pub fn deadline(&self) -> Option<Instant> {
        self.first_at.map(|t| t + self.delay)
    }

    /// The pending batch if its deadline has passed.
    pub fn take_due(&mut self, now: Instant) -> Option<Vec<u8>> {
        match self.deadline() {
            Some(d) if now >= d => self.take(),
            _ => None,
        }
    }

    /// The pending batch regardless of the deadline.
    pub fn take(&mut self) -> Option<Vec<u8>> {
        self.first_at = None;
        if self.buf.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.buf))
        }
    }

    /// Drop anything pending.
    pub fn clear(&mut self) {
        self.buf.clear();
        self.first_at = None;
    }

    /// Bytes pending.
    pub fn pending(&self) -> usize {
        self.buf.len()
    }
}

/// Messages for a pane's pump task.
pub enum PumpMsg {
    /// Encoded records, batched.
    Records(Vec<u8>),
    /// Encoded records that must go out now (after anything pending), e.g.
    /// the first Full after an attach.
    Urgent(Vec<u8>),
    /// Replace the destination; pending bytes for the old one are dropped.
    SetSink(Option<Arc<dyn RecordSink>>),
}

impl fmt::Debug for PumpMsg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Records(b) => write!(f, "Records({} bytes)", b.len()),
            Self::Urgent(b) => write!(f, "Urgent({} bytes)", b.len()),
            Self::SetSink(s) => write!(f, "SetSink({})", s.is_some()),
        }
    }
}

/// Run a pump: batch records and deliver them to the current sink. Ends
/// when every sender is dropped (after flushing).
pub async fn run_pump(mut rx: mpsc::UnboundedReceiver<PumpMsg>, mut batcher: Batcher) {
    let mut sink: Option<Arc<dyn RecordSink>> = None;
    let deliver = |sink: &Option<Arc<dyn RecordSink>>, b: Vec<u8>| {
        if let Some(s) = sink {
            // A dead receiver (reloaded webview) is fine: the next attach
            // replaces the sink.
            let _ = s.send(b);
        }
    };
    loop {
        let deadline = batcher.deadline();
        tokio::select! {
            msg = rx.recv() => match msg {
                None => {
                    if let Some(b) = batcher.take() {
                        deliver(&sink, b);
                    }
                    return;
                }
                Some(PumpMsg::Records(b)) => {
                    if sink.is_none() {
                        continue;
                    }
                    if let Some(b) = batcher.push(&b, Instant::now()) {
                        deliver(&sink, b);
                    }
                }
                Some(PumpMsg::Urgent(b)) => {
                    if sink.is_none() {
                        continue;
                    }
                    batcher.push(&b, Instant::now());
                    if let Some(b) = batcher.take() {
                        deliver(&sink, b);
                    }
                }
                Some(PumpMsg::SetSink(s)) => {
                    batcher.clear();
                    sink = s;
                }
            },
            () = sleep_until_opt(deadline), if deadline.is_some() => {
                if let Some(b) = batcher.take_due(Instant::now()) {
                    deliver(&sink, b);
                }
            }
        }
    }
}

async fn sleep_until_opt(t: Option<Instant>) {
    match t {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flushes_after_delay_from_first_record() {
        let t0 = Instant::now();
        let mut b = Batcher::default();
        assert!(b.push(b"aa", t0).is_none());
        assert!(b.push(b"bb", t0 + Duration::from_millis(3)).is_none());
        assert_eq!(b.deadline(), Some(t0 + FLUSH_DELAY));
        assert!(b.take_due(t0 + Duration::from_millis(3)).is_none());
        assert_eq!(b.take_due(t0 + FLUSH_DELAY).unwrap(), b"aabb");
        assert_eq!(b.deadline(), None);
        assert!(b.take_due(t0 + Duration::from_secs(1)).is_none());
    }

    #[test]
    fn flushes_immediately_over_size_limit() {
        let t0 = Instant::now();
        let mut b = Batcher::default();
        let half = vec![1u8; FLUSH_BYTES / 2];
        assert!(b.push(&half, t0).is_none());
        assert!(b.push(&half, t0).is_none(), "exactly the limit stays");
        let out = b.push(b"x", t0).expect("over the limit");
        assert_eq!(out.len(), FLUSH_BYTES + 1);
        assert_eq!(b.pending(), 0);
        assert_eq!(b.deadline(), None);
    }

    #[test]
    fn empty_pushes_do_not_arm_the_timer() {
        let mut b = Batcher::default();
        assert!(b.push(b"", Instant::now()).is_none());
        assert_eq!(b.deadline(), None);
        assert!(b.take().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn pump_batches_within_4ms_and_switches_sinks() {
        let (tx, rx) = mpsc::unbounded_channel();
        let pump = tokio::spawn(run_pump(rx, Batcher::default()));
        let (out_tx, mut out) = mpsc::unbounded_channel::<Vec<u8>>();
        tx.send(PumpMsg::Records(b"dropped: no sink".to_vec()))
            .unwrap();
        tx.send(PumpMsg::SetSink(Some(Arc::new(out_tx)))).unwrap();
        tx.send(PumpMsg::Records(b"a".to_vec())).unwrap();
        tokio::time::sleep(Duration::from_millis(1)).await;
        tx.send(PumpMsg::Records(b"b".to_vec())).unwrap();
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert!(out.try_recv().is_err(), "not yet due");
        tokio::time::sleep(Duration::from_millis(3)).await;
        assert_eq!(out.recv().await.unwrap(), b"ab");

        // Urgent flushes at once, including what is pending.
        tx.send(PumpMsg::Records(b"c".to_vec())).unwrap();
        tx.send(PumpMsg::Urgent(b"D".to_vec())).unwrap();
        tokio::task::yield_now().await;
        assert_eq!(out.recv().await.unwrap(), b"cD");

        // Replacing the sink drops pending bytes for the old one.
        let (out2_tx, mut out2) = mpsc::unbounded_channel::<Vec<u8>>();
        tx.send(PumpMsg::Records(b"old".to_vec())).unwrap();
        tx.send(PumpMsg::SetSink(Some(Arc::new(out2_tx)))).unwrap();
        tx.send(PumpMsg::Records(b"new".to_vec())).unwrap();
        drop(tx);
        pump.await.unwrap();
        assert_eq!(out2.recv().await.unwrap(), b"new");
        assert!(out.try_recv().is_err());
    }
}
