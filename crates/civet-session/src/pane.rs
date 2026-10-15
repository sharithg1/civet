//! One pane: PTY + server-side terminal + reader/writer/waiter threads.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use civet_proto::{
    AgentStatus, AgentStatusSource, ClientId, ECHO_ACK_DELAY_MS, Event, PaneId, PaneInfo, TermSize,
    WorktreeId,
};
use civet_term::{FeedOutcome, PaneTerminal, PromptMark, TermConfig};
use portable_pty::{CommandBuilder, MasterPty, PtySize};
use tokio::sync::{broadcast, watch};

use crate::status::{Change, FeedSignals, Mark, StatusTracker};
use crate::{InputOutcome, SessionError, sys};

/// Read buffer size for PTY output.
const READ_CHUNK: usize = 64 * 1024;
/// After the process exits, how long to wait for the reader to drain the
/// remaining output before reporting the exit.
const EXIT_DRAIN_WAIT: Duration = Duration::from_millis(500);
/// After a non-SIGKILL kill, escalate to SIGKILL if the process group is
/// still alive after this long.
const KILL_ESCALATE_AFTER: Duration = Duration::from_secs(3);

/// Lock a std mutex, ignoring poisoning (a panicked holder leaves plain data
/// that is still usable).
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Everything needed to start a pane (computed by the manager).
#[derive(Debug, Clone)]
pub(crate) struct SpawnParams {
    pub id: PaneId,
    /// Full argv actually executed (shell wrapper included).
    pub argv: Vec<String>,
    /// argv reported in `PaneInfo.command`.
    pub display_command: Vec<String>,
    pub cwd: PathBuf,
    /// Extra environment (applied in order on top of the daemon's env).
    pub env: Vec<(String, String)>,
    pub size: TermSize,
    pub title: String,
    pub agent: Option<String>,
    pub is_agent: bool,
    pub worktree: Option<WorktreeId>,
    pub idle_after: Duration,
    pub scrollback_limit: usize,
    pub status_file: PathBuf,
}

/// Mutable descriptive state of a pane.
#[derive(Debug)]
pub(crate) struct PaneMeta {
    pub worktree: Option<WorktreeId>,
    pub cwd: String,
    pub command: Vec<String>,
    pub title: String,
    pub size: TermSize,
    pub agent: Option<String>,
    pub exit_code: Option<i32>,
    pub exited: bool,
    pub created_at_ms: u64,
}

/// Per-(client, pane) input bookkeeping.
#[derive(Debug, Default)]
pub(crate) struct InputRecord {
    /// Highest seq written to the PTY.
    pub last_seq: u64,
    /// Highest seq written at least `ECHO_ACK_DELAY_MS` ago.
    acked: u64,
    /// Recent writes not yet echo-acked: (seq, written at).
    recent: VecDeque<(u64, Instant)>,
}

impl InputRecord {
    /// Echo-acked seq as of `now`, and when the next pending write becomes
    /// acked (if any).
    pub(crate) fn echo_ack(&mut self, now: Instant) -> (u64, Option<Instant>) {
        let delay = Duration::from_millis(ECHO_ACK_DELAY_MS);
        while let Some(&(seq, t)) = self.recent.front() {
            if now >= t + delay {
                self.acked = seq;
                self.recent.pop_front();
            } else {
                break;
            }
        }
        (self.acked, self.recent.front().map(|&(_, t)| t + delay))
    }
}

/// A running (or exited) pane.
pub(crate) struct Pane {
    pub id: PaneId,
    pub term: Mutex<PaneTerminal>,
    pub meta: Mutex<PaneMeta>,
    pub status: Mutex<StatusTracker>,
    pub inputs: Mutex<HashMap<ClientId, InputRecord>>,
    /// Current terminal state number; also poked (send_modify) on close.
    pub state_tx: watch::Sender<u64>,
    /// Bumped on every applied input (wakes subscriptions for echo acks).
    pub input_tx: watch::Sender<u64>,
    closed: AtomicBool,
    /// False until PaneCreated was emitted; pane events are dropped before
    /// that (PaneCreated carries the current title/status instead). The
    /// mutex orders PaneCreated before every other event of the pane.
    announced: Mutex<bool>,
    writer: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    pid: Option<u32>,
    events: broadcast::Sender<Event>,
    pub status_file: PathBuf,
    hook_sig: Mutex<Option<(SystemTime, u64)>>,
}

impl std::fmt::Debug for Pane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pane")
            .field("id", &self.id)
            .field("pid", &self.pid)
            .field("closed", &self.is_closed())
            .finish_non_exhaustive()
    }
}

fn pty_size(size: TermSize) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: size.pixel_width,
        pixel_height: size.pixel_height,
    }
}

fn signals(outcome: &FeedOutcome) -> FeedSignals {
    FeedSignals {
        bell: outcome.bell,
        notifications: outcome
            .notifications
            .iter()
            .map(|n| n.body.clone())
            .collect(),
        marks: outcome
            .prompt_marks
            .iter()
            .map(|m| match m {
                PromptMark::PromptStart => Mark::PromptStart,
                PromptMark::CommandStart => Mark::CommandStart,
                PromptMark::CommandExecuted => Mark::CommandExecuted,
                PromptMark::CommandFinished { .. } => Mark::CommandFinished,
            })
            .collect(),
    }
}

impl Pane {
    /// Open the PTY, spawn the process and start the pane's threads.
    /// Blocking (fork/exec); call from `spawn_blocking`.
    pub(crate) fn spawn(
        p: SpawnParams,
        events: broadcast::Sender<Event>,
    ) -> Result<Arc<Pane>, SessionError> {
        let size = p.size;
        let pty = portable_pty::native_pty_system();
        let pair = pty
            .openpty(pty_size(size))
            .map_err(|e| SessionError::Spawn(format!("openpty: {e}")))?;
        let mut cmd = CommandBuilder::from_argv(p.argv.iter().map(Into::into).collect());
        cmd.cwd(&p.cwd);
        for (k, v) in &p.env {
            cmd.env(k, v);
        }
        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| SessionError::Spawn(format!("{}: {e}", p.argv.join(" "))))?;
        // Our copy of the slave must go, or the reader never sees EOF.
        drop(pair.slave);
        let master = pair.master;
        let setup = (|| -> Result<_, SessionError> {
            let reader = master
                .try_clone_reader()
                .map_err(|e| SessionError::Spawn(format!("pty reader: {e}")))?;
            let writer = master
                .take_writer()
                .map_err(|e| SessionError::Spawn(format!("pty writer: {e}")))?;
            Ok((reader, writer))
        })();
        let (reader, writer) = match setup {
            Ok(rw) => rw,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };

        let term = PaneTerminal::new(TermConfig {
            size,
            scrollback_limit: p.scrollback_limit,
        });
        let initial_state = term.state();
        let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>();
        let pane = Arc::new(Pane {
            id: p.id,
            term: Mutex::new(term),
            meta: Mutex::new(PaneMeta {
                worktree: p.worktree.clone(),
                cwd: p.cwd.to_string_lossy().into_owned(),
                command: p.display_command.clone(),
                title: p.title.clone(),
                size,
                agent: p.agent.clone(),
                exit_code: None,
                exited: false,
                created_at_ms: civet_proto::now_ms(),
            }),
            status: Mutex::new(StatusTracker::new(p.id, p.is_agent, p.idle_after)),
            inputs: Mutex::new(HashMap::new()),
            state_tx: watch::Sender::new(initial_state),
            input_tx: watch::Sender::new(0),
            closed: AtomicBool::new(false),
            announced: Mutex::new(false),
            writer: Mutex::new(Some(write_tx)),
            master: Mutex::new(Some(master)),
            pid: child.process_id(),
            events,
            status_file: p.status_file.clone(),
            hook_sig: Mutex::new(None),
        });

        let id = p.id.0;
        let spawn_thread = |name: String, f: Box<dyn FnOnce() + Send>| {
            std::thread::Builder::new()
                .name(name)
                .spawn(f)
                .map_err(|e| SessionError::Spawn(format!("thread: {e}")))
        };

        spawn_thread(
            format!("pty-writer-{id}"),
            Box::new(move || writer_loop(writer, write_rx)),
        )?;

        let (done_tx, done_rx) = mpsc::channel::<()>();
        let rp = pane.clone();
        spawn_thread(
            format!("pty-reader-{id}"),
            Box::new(move || {
                reader_loop(&rp, reader);
                let _ = done_tx.send(());
            }),
        )?;

        let wp = pane.clone();
        spawn_thread(
            format!("pty-waiter-{id}"),
            Box::new(move || {
                let status = child.wait();
                let _ = done_rx.recv_timeout(EXIT_DRAIN_WAIT);
                let code = match &status {
                    Ok(s) if s.signal().is_none() => Some(s.exit_code() as i32),
                    Ok(_) => None,
                    Err(e) => {
                        tracing::warn!(pane = id, "wait failed: {e}");
                        None
                    }
                };
                let signal = status.ok().and_then(|s| s.signal().map(str::to_owned));
                wp.on_exit(code, signal);
            }),
        )?;

        tracing::info!(pane = id, pid = ?pane.pid, cols = size.cols, rows = size.rows, "pane spawned");
        Ok(pane)
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub(crate) fn is_exited(&self) -> bool {
        lock(&self.meta).exited
    }

    fn emit(&self, event: Event) {
        let announced = lock(&self.announced);
        if *announced {
            // No receivers is fine.
            let _ = self.events.send(event);
        }
    }

    /// Emit PaneCreated (with the current info) and enable pane events.
    /// Returns the info that was announced.
    pub(crate) fn announce(&self) -> PaneInfo {
        let mut announced = lock(&self.announced);
        let info = self.info();
        let _ = self.events.send(Event::PaneCreated(info.clone()));
        *announced = true;
        info
    }

    fn emit_status(&self, changes: Vec<Change>) {
        if changes.is_empty() {
            return;
        }
        let events = lock(&self.status).apply_all(changes);
        for e in events {
            tracing::debug!(pane = self.id.0, status = %e.status, source = ?e.source, "agent status");
            self.emit(Event::AgentStatus(e));
        }
    }

    /// Queue bytes for the PTY (input or terminal replies).
    fn send_to_pty(&self, data: Vec<u8>) -> bool {
        match lock(&self.writer).as_ref() {
            Some(tx) => tx.send(data).is_ok(),
            None => false,
        }
    }

    /// Handle one chunk of PTY output.
    fn on_output(&self, bytes: &[u8]) {
        let now = Instant::now();
        let (outcome, state) = {
            let mut term = lock(&self.term);
            let outcome = term.feed(bytes);
            (outcome, term.state())
        };
        if !outcome.pty_replies.is_empty() {
            self.send_to_pty(outcome.pty_replies.clone());
        }
        if outcome.changed {
            self.state_tx.send_replace(state);
        }
        if let Some(title) = &outcome.title {
            lock(&self.meta).title = title.clone();
            if !self.is_closed() {
                self.emit(Event::PaneTitle {
                    pane: self.id,
                    title: title.clone(),
                });
            }
        }
        if outcome.bell && !self.is_closed() {
            self.emit(Event::Bell { pane: self.id });
        }
        if let Some(text) = &outcome.clipboard
            && !self.is_closed()
        {
            // Not part of the screen state: a resume or full repaint never
            // re-sends it. civetd forwards it to subscribers only.
            tracing::debug!(pane = self.id.0, len = text.len(), "OSC 52 clipboard write");
            self.emit(Event::Clipboard {
                pane: self.id,
                text: text.clone(),
            });
        }
        let sig = signals(&outcome);
        let changes = lock(&self.status).on_output(now, bytes.len(), &sig);
        if !self.is_closed() {
            self.emit_status(changes);
        }
    }

    fn on_exit(&self, code: Option<i32>, signal: Option<String>) {
        {
            let mut meta = lock(&self.meta);
            meta.exited = true;
            meta.exit_code = code;
        }
        // Writes after exit are refused; let the writer thread finish.
        lock(&self.writer).take();
        tracing::info!(pane = self.id.0, ?code, ?signal, "pane exited");
        if self.is_closed() {
            return;
        }
        self.emit(Event::PaneExited {
            pane: self.id,
            exit_code: code,
        });
        let message = match (code, signal) {
            (Some(c), _) => format!("exit {c}"),
            (None, Some(s)) => format!("killed by {s}"),
            (None, None) => "exited".to_owned(),
        };
        self.emit_status(vec![Change {
            status: AgentStatus::Done,
            source: AgentStatusSource::ProcessExit,
            message: Some(message),
        }]);
        // Wake subscribers (state may not have changed).
        self.state_tx.send_modify(|_| {});
    }

    /// Apply input from `client` iff `seq` is new.
    pub(crate) fn write_input(
        &self,
        client: ClientId,
        seq: u64,
        data: &[u8],
    ) -> Result<InputOutcome, SessionError> {
        if self.is_closed() || self.is_exited() {
            return Err(SessionError::InvalidArgument(format!(
                "{} has exited",
                self.id
            )));
        }
        let now = Instant::now();
        {
            let mut inputs = lock(&self.inputs);
            let rec = inputs.entry(client).or_default();
            if seq <= rec.last_seq {
                return Ok(InputOutcome::Duplicate);
            }
            if seq != rec.last_seq + 1 {
                tracing::debug!(pane = self.id.0, last = rec.last_seq, seq, "input seq gap");
            }
            if !self.send_to_pty(data.to_vec()) {
                return Err(SessionError::InvalidArgument(format!(
                    "{} has exited",
                    self.id
                )));
            }
            rec.last_seq = seq;
            rec.recent.push_back((seq, now));
        }
        tracing::trace!(pane = self.id.0, seq, len = data.len(), "input");
        lock(&self.status).on_input(now);
        self.input_tx.send_modify(|e| *e = e.wrapping_add(1));
        Ok(InputOutcome::Applied)
    }

    pub(crate) fn last_input_seq(&self, client: ClientId) -> u64 {
        lock(&self.inputs).get(&client).map_or(0, |r| r.last_seq)
    }

    /// (echo-acked seq, next deadline) for `client`.
    pub(crate) fn echo_ack(&self, client: ClientId, now: Instant) -> (u64, Option<Instant>) {
        lock(&self.inputs)
            .get_mut(&client)
            .map_or((0, None), |r| r.echo_ack(now))
    }

    pub(crate) fn forget_client(&self, client: ClientId) {
        lock(&self.inputs).remove(&client);
    }

    pub(crate) fn resize(&self, size: TermSize) -> Result<(), SessionError> {
        if size.cols == 0 || size.rows == 0 {
            return Err(SessionError::InvalidArgument(format!(
                "invalid size {}x{}",
                size.cols, size.rows
            )));
        }
        if let Some(master) = lock(&self.master).as_ref() {
            if let Err(e) = master.resize(pty_size(size)) {
                // The PTY may already be gone (exited pane); the emulator
                // still resizes so the screen stays consistent.
                tracing::debug!(pane = self.id.0, "pty resize failed: {e}");
            }
        }
        let (changed, state) = {
            let mut term = lock(&self.term);
            let changed = term
                .resize(size)
                .map_err(|e| SessionError::InvalidArgument(e.to_string()))?;
            (changed, term.state())
        };
        lock(&self.meta).size = size;
        if changed {
            self.state_tx.send_replace(state);
        }
        self.emit(Event::PaneResized {
            pane: self.id,
            size,
        });
        Ok(())
    }

    /// Signal the process group, close our end of the PTY, wake subscribers.
    pub(crate) fn kill(self: &Arc<Self>, signal: civet_proto::KillSignal) {
        self.closed.store(true, Ordering::SeqCst);
        let sig = sys::signal_number(signal);
        let fg = lock(&self.master)
            .as_ref()
            .and_then(|m| m.process_group_leader());
        if !self.is_exited() {
            if let Some(pid) = self.pid {
                if let Err(e) = sys::killpg(pid as i32, sig) {
                    tracing::debug!(pane = self.id.0, "killpg({pid}) failed: {e}");
                }
            }
            if let Some(fg) = fg.filter(|&fg| Some(fg as u32) != self.pid) {
                let _ = sys::killpg(fg, sig);
            }
        }
        lock(&self.writer).take();
        lock(&self.master).take();
        self.state_tx.send_modify(|_| {});
        let _ = std::fs::remove_file(&self.status_file);

        if sig != libc::SIGKILL && !self.is_exited() {
            let pane = self.clone();
            let _ = std::thread::Builder::new()
                .name(format!("pty-reaper-{}", self.id.0))
                .spawn(move || {
                    std::thread::sleep(KILL_ESCALATE_AFTER);
                    if !pane.is_exited() {
                        if let Some(pid) = pane.pid {
                            tracing::info!(pane = pane.id.0, "escalating to SIGKILL");
                            let _ = sys::killpg(pid as i32, libc::SIGKILL);
                        }
                    }
                });
        }
    }

    pub(crate) fn info(&self) -> PaneInfo {
        let state = *self.state_tx.borrow();
        let status = lock(&self.status).status;
        let m = lock(&self.meta);
        PaneInfo {
            id: self.id,
            worktree: m.worktree.clone(),
            cwd: m.cwd.clone(),
            command: m.command.clone(),
            title: m.title.clone(),
            size: m.size,
            agent: m.agent.clone(),
            status,
            exit_code: m.exit_code,
            exited: m.exited,
            created_at_ms: m.created_at_ms,
            state,
        }
    }

    /// Set status from a client request.
    pub(crate) fn set_status_from_client(&self, status: AgentStatus) {
        self.emit_status(vec![Change {
            status,
            source: AgentStatusSource::Client,
            message: None,
        }]);
    }

    /// Re-read the hook file if its mtime or length changed.
    pub(crate) fn poll_hook_file(&self) {
        if self.is_closed() {
            return;
        }
        let Ok(md) = std::fs::metadata(&self.status_file) else {
            return;
        };
        let sig = (md.modified().unwrap_or(SystemTime::UNIX_EPOCH), md.len());
        {
            let mut cur = lock(&self.hook_sig);
            if *cur == Some(sig) {
                return;
            }
            *cur = Some(sig);
        }
        if md.len() == 0 {
            return;
        }
        let Ok(text) = std::fs::read_to_string(&self.status_file) else {
            return;
        };
        if let Some((status, message)) = civet_proto::parse_status_file(&text) {
            self.emit_status(vec![Change {
                status,
                source: AgentStatusSource::Hook,
                message,
            }]);
        } else {
            tracing::debug!(pane = self.id.0, "unrecognised hook file contents");
        }
    }

    /// Time-based heuristics.
    pub(crate) fn tick(&self, now: Instant) {
        if self.is_closed() {
            return;
        }
        let change = lock(&self.status).on_tick(now);
        if let Some(c) = change {
            self.emit_status(vec![c]);
        }
    }
}

fn writer_loop(mut writer: Box<dyn Write + Send>, rx: mpsc::Receiver<Vec<u8>>) {
    for data in rx {
        if let Err(e) = writer.write_all(&data).and_then(|()| writer.flush()) {
            tracing::debug!("pty write failed: {e}");
            break;
        }
    }
}

fn reader_loop(pane: &Pane, mut reader: Box<dyn Read + Send>) {
    let mut buf = vec![0u8; READ_CHUNK];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => pane.on_output(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            // EIO once the slave side is closed (Linux); normal end.
            Err(_) => break,
        }
    }
    tracing::debug!(pane = pane.id.0, "pty reader finished");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_ack_window() {
        let t0 = Instant::now();
        let mut r = InputRecord::default();
        r.recent.push_back((1, t0));
        r.recent.push_back((2, t0 + Duration::from_millis(30)));
        let (a, next) = r.echo_ack(t0 + Duration::from_millis(10));
        assert_eq!(a, 0);
        assert_eq!(next, Some(t0 + Duration::from_millis(50)));
        let (a, next) = r.echo_ack(t0 + Duration::from_millis(60));
        assert_eq!(a, 1);
        assert_eq!(next, Some(t0 + Duration::from_millis(80)));
        let (a, next) = r.echo_ack(t0 + Duration::from_millis(80));
        assert_eq!(a, 2);
        assert_eq!(next, None);
    }
}
