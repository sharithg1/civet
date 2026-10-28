//! Agent usage for `Request::AgentUsage` (see `civet_proto::usage`).
//!
//! Reads (never writes) the agents' own session logs:
//!
//! * Claude Code: `~/.claude/projects/<dir>/<session>.jsonl` and its
//!   subagents' `<session>/subagents/*.jsonl` (`$CLAUDE_CONFIG_DIR` moves
//!   `~/.claude`). Every assistant line carries `message.usage`; a message
//!   with several content blocks is logged once per block with the same
//!   `message.id`, so a message counts once (its newest line).
//! * Codex: `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` (`$CODEX_HOME`
//!   moves `~/.codex`): `session_meta` (id, cwd), `turn_context` (model) and
//!   `token_count` events with running totals per thread.
//!
//! Each file's parse state is kept between requests together with how far
//! it was read, so a request only reads what was appended since the last
//! one (a file that shrank or was replaced is read again). Files last
//! modified before the window are not opened at all. Usage is kept per hour
//! and per model; costs are estimated with [`super::prices`].

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use civet_proto::{
    AgentSessionUsage, AgentUsage, MAX_USAGE_SESSIONS, RepoId, TokenCounts, UsageAgent, UsageHour,
    WorktreeId, now_ms,
};
use civet_session::SessionManager;
use serde::Deserialize;

use super::prices::{self, PRICES, PRICES_AS_OF};

const HOUR_MS: u64 = 3_600_000;

/// Claude messages remembered per file for de-duplicating their per-block
/// lines (the blocks of one message are logged next to each other).
const RECENT_MESSAGES: usize = 16;

/// Longest line parsed (a line with a huge tool result is skipped whole).
const MAX_LINE_BYTES: usize = 64 * 1024 * 1024;

/// Where the logs are.
#[derive(Debug, Clone)]
pub(crate) struct Roots {
    /// Claude Code's `projects` directory.
    pub claude: Option<PathBuf>,
    /// Codex's `sessions` directory.
    pub codex: Option<PathBuf>,
}

impl Roots {
    /// The daemon user's, honouring `$CLAUDE_CONFIG_DIR` and `$CODEX_HOME`.
    pub(crate) fn from_env() -> Self {
        let home = dirs::home_dir();
        let dir = |var: &str, default: &str| {
            std::env::var_os(var)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .or_else(|| home.as_ref().map(|h| h.join(default)))
        };
        Self {
            claude: dir("CLAUDE_CONFIG_DIR", ".claude").map(|d| d.join("projects")),
            codex: dir("CODEX_HOME", ".codex").map(|d| d.join("sessions")),
        }
    }
}

/// Answers `Request::AgentUsage`, keeping each log's parse state.
pub(crate) struct UsageScanner {
    sessions: SessionManager,
    roots: Roots,
    /// Log path → what it held so far. Taken out while a scan runs (one at
    /// a time).
    logs: tokio::sync::Mutex<HashMap<PathBuf, Log>>,
}

impl std::fmt::Debug for UsageScanner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsageScanner").finish_non_exhaustive()
    }
}

impl UsageScanner {
    pub(crate) fn new(sessions: SessionManager) -> Arc<Self> {
        Arc::new(Self {
            sessions,
            roots: Roots::from_env(),
            logs: tokio::sync::Mutex::new(HashMap::new()),
        })
    }

    /// Answer one `Request::AgentUsage`.
    pub(crate) async fn request(&self, since_ms: u64) -> AgentUsage {
        let worktrees = self.worktrees().await;
        let mut guard = self.logs.lock().await;
        let mut logs = std::mem::take(&mut *guard);
        let roots = self.roots.clone();
        let sessions = match tokio::task::spawn_blocking(move || {
            scan(&roots, &mut logs, since_ms);
            let sessions = collect(&logs, since_ms);
            (logs, sessions)
        })
        .await
        {
            Ok((logs, sessions)) => {
                *guard = logs;
                sessions
            }
            Err(e) => {
                // The parse state is lost; the next request starts over.
                tracing::warn!(error = %e, "agent usage scan panicked");
                Vec::new()
            }
        };
        drop(guard);
        respond(sessions, &worktrees, since_ms)
    }

    /// Every worktree of every project: (path, worktree, project).
    async fn worktrees(&self) -> Vec<(String, WorktreeId, RepoId)> {
        let mut out = Vec::new();
        let Ok(projects) = self.sessions.list_projects().await else {
            return out;
        };
        for p in projects {
            let Ok(wts) = self.sessions.list_worktrees(&p.repo.id).await else {
                continue;
            };
            out.extend(wts.into_iter().map(|w| (w.path, w.id, p.repo.id.clone())));
        }
        out
    }
}

/// The reply: sessions matched to worktrees, newest first, plus the prices.
fn respond(
    mut sessions: Vec<AgentSessionUsage>,
    worktrees: &[(String, WorktreeId, RepoId)],
    since_ms: u64,
) -> AgentUsage {
    for s in &mut sessions {
        if let Some((_, wt, repo)) = worktree_of(&s.cwd, worktrees) {
            s.worktree = Some(wt.clone());
            s.repo = Some(repo.clone());
        }
    }
    sessions.sort_by_key(|s| std::cmp::Reverse(s.last_ms));
    sessions.truncate(MAX_USAGE_SESSIONS);
    AgentUsage {
        since_ms,
        scanned_at_ms: now_ms(),
        sessions,
        prices: PRICES.iter().map(|p| p.to_proto()).collect(),
        prices_as_of: PRICES_AS_OF.to_string(),
    }
}

/// The deepest worktree containing `cwd` (worktrees can nest: a
/// `.claude/worktrees/...` checkout inside the main one).
fn worktree_of<'a, T>(
    cwd: &str,
    worktrees: &'a [(String, T, RepoId)],
) -> Option<&'a (String, T, RepoId)> {
    let cwd = cwd.trim_end_matches('/');
    worktrees
        .iter()
        .filter(|(path, ..)| {
            let path = path.trim_end_matches('/');
            !path.is_empty()
                && (cwd == path
                    || cwd
                        .strip_prefix(path)
                        .is_some_and(|rest| rest.starts_with('/')))
        })
        .max_by_key(|(path, ..)| path.len())
}

// ---------------------------------------------------------------------------
// Per-file state
// ---------------------------------------------------------------------------

/// One hour of one log.
#[derive(Debug, Clone, Default, PartialEq)]
struct Hour {
    tokens: TokenCounts,
    /// Estimated cost, pico-dollars.
    cost_pico: u64,
    unpriced: u64,
    first_ms: u64,
    last_ms: u64,
}

/// One request's usage as parsed.
#[derive(Debug, Clone, PartialEq)]
struct Entry {
    at_ms: u64,
    model: String,
    tokens: TokenCounts,
    cost_pico: u64,
    /// No price for the model.
    unpriced: bool,
}

/// What one log file held so far.
#[derive(Debug)]
struct Log {
    agent: UsageAgent,
    /// The session it belongs to (a subagent log: its parent's).
    session_id: String,
    /// A subagent's log (its cwd does not name the session's).
    sidechain: bool,
    cwd: Option<String>,
    /// Bytes parsed (up to the end of the last complete line).
    offset: u64,
    /// File identity and state when last read.
    ino: u64,
    size: u64,
    mtime_ns: i128,
    hours: BTreeMap<u64, Hour>,
    /// Tokens per model.
    models: HashMap<String, u64>,
    /// Claude: the newest messages counted, to replace when logged again.
    recent: VecDeque<(String, Entry)>,
    /// Codex: the current model and the thread's running total.
    codex_model: Option<String>,
    codex_total: Option<CodexTokens>,
}

impl Log {
    fn new(agent: UsageAgent, session_id: String, sidechain: bool) -> Self {
        Self {
            agent,
            session_id,
            sidechain,
            cwd: None,
            offset: 0,
            ino: 0,
            size: 0,
            mtime_ns: 0,
            hours: BTreeMap::new(),
            models: HashMap::new(),
            recent: VecDeque::new(),
            codex_model: None,
            codex_total: None,
        }
    }

    /// Parse one line (without its newline).
    fn feed(&mut self, line: &[u8]) {
        match self.agent {
            UsageAgent::Claude => self.feed_claude(line),
            UsageAgent::Codex => self.feed_codex(line),
        }
    }

    fn add(&mut self, e: &Entry, sign: i8) {
        let h = self.hours.entry(e.at_ms / HOUR_MS * HOUR_MS).or_default();
        let m = self.models.entry(e.model.clone()).or_default();
        let total = e.tokens.total();
        if sign > 0 {
            h.tokens.add(&e.tokens);
            h.cost_pico += e.cost_pico;
            if e.unpriced {
                h.unpriced += total;
            }
            h.first_ms = if h.first_ms == 0 {
                e.at_ms
            } else {
                h.first_ms.min(e.at_ms)
            };
            h.last_ms = h.last_ms.max(e.at_ms);
            *m += total;
        } else {
            let t = &mut h.tokens;
            t.input = t.input.saturating_sub(e.tokens.input);
            t.output = t.output.saturating_sub(e.tokens.output);
            t.cache_write = t.cache_write.saturating_sub(e.tokens.cache_write);
            t.cache_read = t.cache_read.saturating_sub(e.tokens.cache_read);
            h.cost_pico = h.cost_pico.saturating_sub(e.cost_pico);
            if e.unpriced {
                h.unpriced = h.unpriced.saturating_sub(total);
            }
            *m = m.saturating_sub(total);
        }
    }

    fn feed_claude(&mut self, line: &[u8]) {
        // Most lines (user turns, tool results, snapshots) have no usage:
        // skip them without parsing.
        if memfind(line, b"\"usage\"").is_none() {
            return;
        }
        let Ok(l) = serde_json::from_slice::<ClaudeLine>(line) else {
            return;
        };
        if self.cwd.is_none() {
            self.cwd = l.cwd.filter(|c| !c.is_empty());
        }
        if l.kind.as_deref() != Some("assistant") {
            return;
        }
        let Some(msg) = l.message else { return };
        let (Some(u), Some(model)) = (msg.usage, msg.model) else {
            return;
        };
        let Some(at_ms) = l.timestamp.as_deref().and_then(parse_time_ms) else {
            return;
        };
        if model.starts_with('<') {
            // `<synthetic>`: Claude Code's own messages, nothing billed.
            return;
        }
        let (write_1h, write_5m) = match u.cache_creation {
            Some(c) if c.ephemeral_1h_input_tokens + c.ephemeral_5m_input_tokens > 0 => (
                c.ephemeral_1h_input_tokens,
                u.cache_creation_input_tokens
                    .saturating_sub(c.ephemeral_1h_input_tokens),
            ),
            _ => (0, u.cache_creation_input_tokens),
        };
        let req = prices::Request {
            input: u.input_tokens,
            output: u.output_tokens,
            write_5m,
            write_1h,
            cache_read: u.cache_read_input_tokens,
            fast: u.speed.as_deref() == Some("fast"),
        };
        let entry = entry(at_ms, model, &req);
        let id = msg
            .id
            .or(l.request_id)
            .unwrap_or_else(|| format!("@{at_ms}"));
        if let Some(i) = self.recent.iter().position(|(k, _)| *k == id) {
            if let Some((_, old)) = self.recent.remove(i) {
                self.add(&old, -1);
            }
        } else if self.recent.len() >= RECENT_MESSAGES {
            self.recent.pop_front();
        }
        self.add(&entry, 1);
        self.recent.push_back((id, entry));
    }

    fn feed_codex(&mut self, line: &[u8]) {
        let kind = if memfind(line, b"\"token_count\"").is_some() {
            0
        } else if memfind(line, b"\"turn_context\"").is_some() {
            1
        } else if memfind(line, b"\"session_meta\"").is_some() {
            2
        } else {
            return;
        };
        let Ok(l) = serde_json::from_slice::<CodexLine>(line) else {
            return;
        };
        let Some(p) = l.payload else { return };
        match (kind, l.kind.as_deref()) {
            (2, Some("session_meta")) => {
                if let Some(id) = p.id.filter(|s| !s.is_empty()) {
                    self.session_id = id;
                }
                if p.cwd.is_some() {
                    self.cwd = p.cwd;
                }
            }
            (1, Some("turn_context")) => {
                if p.model.is_some() {
                    self.codex_model = p.model;
                }
                if self.cwd.is_none() {
                    self.cwd = p.cwd;
                }
            }
            (0, Some("event_msg")) if p.kind.as_deref() == Some("token_count") => {
                let Some(info) = p.info else { return };
                let Some(at_ms) = l.timestamp.as_deref().and_then(parse_time_ms) else {
                    return;
                };
                // The first count of a file is the last request's own (a
                // resumed thread's running total includes the earlier
                // file's); after that, the growth of the running total
                // (the same count is often logged twice).
                let used = match (info.total_token_usage, self.codex_total) {
                    (Some(total), Some(prev)) => {
                        if total.total_tokens <= prev.total_tokens {
                            return;
                        }
                        self.codex_total = Some(total);
                        total.minus(&prev)
                    }
                    (total, _) => {
                        self.codex_total = total;
                        match info.last_token_usage {
                            Some(last) => last,
                            None => return,
                        }
                    }
                };
                let cached = used.cached_input_tokens.min(used.input_tokens);
                let req = prices::Request {
                    input: used.input_tokens - cached,
                    output: used.output_tokens,
                    write_5m: 0,
                    write_1h: 0,
                    cache_read: cached,
                    fast: false,
                };
                let model = self
                    .codex_model
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string());
                let e = entry(at_ms, model, &req);
                self.add(&e, 1);
            }
            _ => {}
        }
    }
}

fn entry(at_ms: u64, model: String, r: &prices::Request) -> Entry {
    let tokens = TokenCounts {
        input: r.input,
        output: r.output,
        cache_write: r.write_5m + r.write_1h,
        cache_read: r.cache_read,
    };
    let price = prices::price_of(&model);
    Entry {
        at_ms,
        cost_pico: price.map_or(0, |p| p.cost_pico(r)),
        unpriced: price.is_none() && tokens.total() > 0,
        model,
        tokens,
    }
}

#[derive(Deserialize)]
struct ClaudeLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    message: Option<ClaudeMessage>,
}

#[derive(Deserialize)]
struct ClaudeMessage {
    id: Option<String>,
    model: Option<String>,
    usage: Option<ClaudeUsage>,
}

#[derive(Deserialize)]
struct ClaudeUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    cache_creation: Option<ClaudeCacheCreation>,
    speed: Option<String>,
}

#[derive(Deserialize)]
struct ClaudeCacheCreation {
    #[serde(default)]
    ephemeral_5m_input_tokens: u64,
    #[serde(default)]
    ephemeral_1h_input_tokens: u64,
}

#[derive(Deserialize)]
struct CodexLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    payload: Option<CodexPayload>,
}

#[derive(Deserialize)]
struct CodexPayload {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<String>,
    cwd: Option<String>,
    model: Option<String>,
    info: Option<CodexInfo>,
}

#[derive(Deserialize)]
struct CodexInfo {
    total_token_usage: Option<CodexTokens>,
    last_token_usage: Option<CodexTokens>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
struct CodexTokens {
    /// Includes the cached ones.
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cached_input_tokens: u64,
    /// Includes reasoning.
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
}

impl CodexTokens {
    fn minus(&self, o: &CodexTokens) -> CodexTokens {
        CodexTokens {
            input_tokens: self.input_tokens.saturating_sub(o.input_tokens),
            cached_input_tokens: self
                .cached_input_tokens
                .saturating_sub(o.cached_input_tokens),
            output_tokens: self.output_tokens.saturating_sub(o.output_tokens),
            total_tokens: self.total_tokens.saturating_sub(o.total_tokens),
        }
    }
}

fn memfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// `2026-10-28T03:13:15.682Z` (or with a `+hh:mm` offset) → ms since epoch.
fn parse_time_ms(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> { s.get(r)?.parse().ok() };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut i = 19;
    let mut ms = 0i64;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        let frac = &s[start..i];
        let digits: String = frac.chars().chain("000".chars()).take(3).collect();
        ms = digits.parse().ok()?;
    }
    let offset_min = match b.get(i) {
        None | Some(b'Z' | b'z') => 0,
        Some(sign @ (b'+' | b'-')) => {
            let oh = num(i + 1..i + 3)?;
            let om = num(i + 4..i + 6)?;
            let m = oh * 60 + om;
            if *sign == b'+' { m } else { -m }
        }
        _ => return None,
    };
    // Days from civil (Howard Hinnant).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + mi * 60 + sec - offset_min * 60;
    u64::try_from(secs * 1000 + ms).ok()
}

// ---------------------------------------------------------------------------
// Scanning
// ---------------------------------------------------------------------------

/// A log file found on disk.
struct Found {
    path: PathBuf,
    agent: UsageAgent,
    session_id: String,
    sidechain: bool,
}

/// Find the logs, then read what is new in each.
fn scan(roots: &Roots, logs: &mut HashMap<PathBuf, Log>, since_ms: u64) {
    let mut found = Vec::new();
    if let Some(root) = &roots.claude {
        find_claude(root, &mut found);
    }
    if let Some(root) = &roots.codex {
        find_codex(root, 0, &mut found);
    }
    let seen: HashSet<&PathBuf> = found.iter().map(|f| &f.path).collect();
    logs.retain(|p, _| seen.contains(p));
    for f in &found {
        let Ok(meta) = std::fs::metadata(&f.path) else {
            continue;
        };
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos() as i128);
        let ino = meta.ino();
        let size = meta.len();
        if let Some(log) = logs.get(&f.path)
            && log.ino == ino
            && log.size == size
            && log.mtime_ns == mtime_ns
        {
            continue;
        }
        if !logs.contains_key(&f.path) && (mtime_ns / 1_000_000) < since_ms as i128 {
            // Nothing in it is new enough.
            continue;
        }
        let log = logs
            .entry(f.path.clone())
            .or_insert_with(|| Log::new(f.agent, f.session_id.clone(), f.sidechain));
        if log.ino != ino || size < log.offset {
            // Replaced or truncated: read it again.
            *log = Log::new(f.agent, f.session_id.clone(), f.sidechain);
        }
        log.ino = ino;
        log.size = size;
        log.mtime_ns = mtime_ns;
        if let Err(e) = read_from(&f.path, log) {
            tracing::debug!(path = %f.path.display(), error = %e, "reading agent log");
        }
    }
}

/// Feed `log` the complete lines after its offset.
fn read_from(path: &Path, log: &mut Log) -> std::io::Result<()> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(log.offset))?;
    let mut r = BufReader::with_capacity(1 << 20, file);
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = r.read_until(b'\n', &mut line)?;
        if n == 0 || line.last() != Some(&b'\n') {
            // End of file, or a line still being written: read it next time.
            return Ok(());
        }
        log.offset += n as u64;
        if n <= MAX_LINE_BYTES {
            log.feed(&line[..n - 1]);
        }
    }
}

/// `<root>/<project>/<session>.jsonl` and
/// `<root>/<project>/<session>/subagents/*.jsonl`.
fn find_claude(root: &Path, out: &mut Vec<Found>) {
    for project in read_dirs(root) {
        let Ok(entries) = std::fs::read_dir(&project) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_file() {
                if let Some(id) = jsonl_stem(&path) {
                    out.push(Found {
                        session_id: id,
                        path,
                        agent: UsageAgent::Claude,
                        sidechain: false,
                    });
                }
            } else if ft.is_dir() {
                let session = e.file_name().to_string_lossy().into_owned();
                let Ok(subs) = std::fs::read_dir(path.join("subagents")) else {
                    continue;
                };
                for s in subs.flatten() {
                    let p = s.path();
                    if jsonl_stem(&p).is_some() && s.file_type().is_ok_and(|t| t.is_file()) {
                        out.push(Found {
                            path: p,
                            agent: UsageAgent::Claude,
                            session_id: session.clone(),
                            sidechain: true,
                        });
                    }
                }
            }
        }
    }
}

/// Every `*.jsonl` under `dir` (YYYY/MM/DD), named after the file until
/// its `session_meta` says otherwise.
fn find_codex(dir: &Path, depth: usize, out: &mut Vec<Found>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() && depth < 4 {
            find_codex(&path, depth + 1, out);
        } else if ft.is_file()
            && let Some(stem) = jsonl_stem(&path)
        {
            out.push(Found {
                path,
                agent: UsageAgent::Codex,
                session_id: stem,
                sidechain: false,
            });
        }
    }
}

fn read_dirs(dir: &Path) -> impl Iterator<Item = PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
}

fn jsonl_stem(p: &Path) -> Option<String> {
    (p.extension()? == "jsonl").then(|| p.file_stem().map(|s| s.to_string_lossy().into_owned()))?
}

/// The logs' sessions with usage in the window (worktrees still unset).
fn collect(logs: &HashMap<PathBuf, Log>, since_ms: u64) -> Vec<AgentSessionUsage> {
    let since_hour = since_ms / HOUR_MS * HOUR_MS;
    struct Acc {
        cwd: Option<String>,
        cwd_from_sidechain: bool,
        hours: BTreeMap<u64, Hour>,
        models: HashMap<String, u64>,
    }
    let mut by_session: HashMap<(UsageAgent, &str), Acc> = HashMap::new();
    for log in logs.values() {
        let in_window: Vec<(&u64, &Hour)> = log.hours.range(since_hour..).collect();
        if in_window.iter().all(|(_, h)| h.tokens.total() == 0) {
            continue;
        }
        let acc = by_session
            .entry((log.agent, log.session_id.as_str()))
            .or_insert_with(|| Acc {
                cwd: None,
                cwd_from_sidechain: true,
                hours: BTreeMap::new(),
                models: HashMap::new(),
            });
        // The main log's cwd names the session; a subagent's only stands in.
        if log.cwd.is_some() && (acc.cwd.is_none() || (acc.cwd_from_sidechain && !log.sidechain)) {
            acc.cwd = log.cwd.clone();
            acc.cwd_from_sidechain = log.sidechain;
        }
        for (k, h) in in_window {
            let a = acc.hours.entry(*k).or_default();
            a.tokens.add(&h.tokens);
            a.cost_pico += h.cost_pico;
            a.unpriced += h.unpriced;
            if h.first_ms != 0 {
                a.first_ms = if a.first_ms == 0 {
                    h.first_ms
                } else {
                    a.first_ms.min(h.first_ms)
                };
            }
            a.last_ms = a.last_ms.max(h.last_ms);
        }
        for (m, n) in &log.models {
            *acc.models.entry(m.clone()).or_default() += n;
        }
    }
    by_session
        .into_iter()
        .map(|((agent, id), acc)| {
            let mut tokens = TokenCounts::default();
            let (mut cost_pico, mut unpriced) = (0u64, 0u64);
            let hours = acc
                .hours
                .iter()
                .filter(|(_, h)| h.tokens.total() > 0)
                .map(|(k, h)| {
                    tokens.add(&h.tokens);
                    cost_pico += h.cost_pico;
                    unpriced += h.unpriced;
                    UsageHour {
                        hour_ms: *k,
                        tokens: h.tokens,
                        cost_micros: h.cost_pico / 1_000_000,
                    }
                })
                .collect();
            let mut models: Vec<(String, u64)> =
                acc.models.into_iter().filter(|(_, n)| *n > 0).collect();
            models.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            AgentSessionUsage {
                agent,
                session_id: id.to_string(),
                cwd: acc.cwd.unwrap_or_default(),
                worktree: None,
                repo: None,
                models: models.into_iter().map(|(m, _)| m).collect(),
                first_ms: acc
                    .hours
                    .values()
                    .map(|h| h.first_ms)
                    .filter(|&t| t != 0)
                    .min()
                    .unwrap_or(0),
                last_ms: acc.hours.values().map(|h| h.last_ms).max().unwrap_or(0),
                tokens,
                cost_micros: cost_pico / 1_000_000,
                unpriced_tokens: unpriced,
                hours,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    const T0: &str = "2026-10-28T03:13:15.682Z";
    const T0_MS: u64 = 1_793_157_195_682;

    fn claude_line(id: &str, ts: &str, model: &str, input: u64, output: u64) -> String {
        serde_json::json!({
            "type": "assistant",
            "timestamp": ts,
            "cwd": "/w/feat/crates",
            "sessionId": "s1",
            "requestId": "req",
            "message": {
                "id": id,
                "model": model,
                "content": [{"type": "text", "text": "the \"usage\" word"}],
                "usage": {
                    "input_tokens": input,
                    "output_tokens": output,
                    "cache_creation_input_tokens": 1000,
                    "cache_read_input_tokens": 1_000_000,
                    "cache_creation": {
                        "ephemeral_1h_input_tokens": 1000,
                        "ephemeral_5m_input_tokens": 0
                    }
                }
            }
        })
        .to_string()
    }

    fn codex_count(ts: &str, total: (u64, u64, u64), last: (u64, u64, u64)) -> String {
        let t = |(i, c, o): (u64, u64, u64)| {
            serde_json::json!({
                "input_tokens": i, "cached_input_tokens": c, "output_tokens": o,
                "reasoning_output_tokens": 0, "total_tokens": i + o
            })
        };
        serde_json::json!({
            "timestamp": ts,
            "type": "event_msg",
            "payload": {"type": "token_count", "info": {
                "total_token_usage": t(total), "last_token_usage": t(last)
            }}
        })
        .to_string()
    }

    fn write(path: &Path, lines: &[String]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    fn roots(dir: &Path) -> Roots {
        Roots {
            claude: Some(dir.join("claude")),
            codex: Some(dir.join("codex")),
        }
    }

    #[test]
    fn parses_timestamps() {
        assert_eq!(parse_time_ms(T0), Some(T0_MS));
        assert_eq!(parse_time_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time_ms("2026-10-28T03:13:15Z"), Some(T0_MS - 682));
        assert_eq!(parse_time_ms("2026-10-28T05:13:15.682+02:00"), Some(T0_MS));
        assert_eq!(
            parse_time_ms("2000-02-29T00:00:00.5Z"),
            Some(951_782_400_500)
        );
        assert_eq!(parse_time_ms("yesterday"), None);
    }

    #[test]
    fn claude_counts_each_message_once() {
        let mut log = Log::new(UsageAgent::Claude, "s1".into(), false);
        // Two content blocks of one message, then another message.
        for l in [
            claude_line("m1", T0, "claude-opus-5-5", 2, 100),
            claude_line("m1", T0, "claude-opus-5-5", 2, 211),
            r#"{"type":"user","message":{"content":"no usage here"}}"#.to_string(),
            claude_line("m2", T0, "claude-opus-5-5", 3, 50),
        ] {
            log.feed(l.as_bytes());
        }
        assert_eq!(log.cwd.as_deref(), Some("/w/feat/crates"));
        let h = &log.hours[&(T0_MS / HOUR_MS * HOUR_MS)];
        assert_eq!(
            h.tokens,
            TokenCounts {
                input: 5,
                output: 261,
                cache_write: 2000,
                cache_read: 2_000_000,
            }
        );
        // Opus 5.5: $4 in, $20 out, $8 1h writes, $0.20 reads per MTok.
        let want = 5 * 4_000_000 + 261 * 20_000_000 + 2000 * 8_000_000 + 2_000_000 * 200_000;
        assert_eq!(h.cost_pico, want);
        assert_eq!(h.unpriced, 0);
        assert_eq!((h.first_ms, h.last_ms), (T0_MS, T0_MS));
    }

    #[test]
    fn claude_skips_synthetic_and_flags_unpriced() {
        let mut log = Log::new(UsageAgent::Claude, "s1".into(), false);
        log.feed(claude_line("m1", T0, "<synthetic>", 0, 0).as_bytes());
        assert!(log.hours.is_empty());
        log.feed(claude_line("m2", T0, "claude-future-9", 10, 10).as_bytes());
        let h = log.hours.values().next().unwrap();
        assert_eq!(h.cost_pico, 0);
        assert_eq!(h.unpriced, h.tokens.total());
    }

    #[test]
    fn codex_counts_growth_of_the_running_total() {
        let mut log = Log::new(UsageAgent::Codex, "rollout-x".into(), false);
        for l in [
            serde_json::json!({"timestamp": T0, "type": "session_meta",
                "payload": {"id": "thread-1", "cwd": "/w/feat"}})
            .to_string(),
            serde_json::json!({"timestamp": T0, "type": "turn_context",
                "payload": {"model": "gpt-5-codex", "cwd": "/w/feat"}})
            .to_string(),
            // A resumed thread: the running total includes an older file's.
            codex_count(T0, (900_000, 0, 9000), (1000, 600, 10)),
            codex_count(T0, (902_000, 1000, 9020), (2000, 1000, 20)),
            // Logged twice.
            codex_count(T0, (902_000, 1000, 9020), (2000, 1000, 20)),
        ] {
            log.feed(l.as_bytes());
        }
        assert_eq!(log.session_id, "thread-1");
        assert_eq!(log.cwd.as_deref(), Some("/w/feat"));
        let h = log.hours.values().next().unwrap();
        assert_eq!(
            h.tokens,
            TokenCounts {
                input: 400 + 1000,
                output: 30,
                cache_write: 0,
                cache_read: 600 + 1000,
            }
        );
        assert_eq!(
            h.cost_pico,
            1400 * 1_250_000 + 30 * 10_000_000 + 1600 * 125_000
        );
        assert_eq!(log.models["gpt-5-codex"], h.tokens.total());
    }

    #[test]
    fn scans_incrementally_and_rolls_up_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let r = roots(dir.path());
        let main = dir.path().join("claude/-w-feat/s1.jsonl");
        let sub = dir
            .path()
            .join("claude/-w-feat/s1/subagents/agent-a1.jsonl");
        let codex = dir.path().join("codex/2026/10/28/rollout-1.jsonl");
        write(&main, &[claude_line("m1", T0, "claude-opus-5-5", 10, 10)]);
        write(&sub, &[claude_line("m9", T0, "claude-haiku-5-5", 7, 1)]);
        write(&codex, &[codex_count(T0, (100, 0, 10), (100, 0, 10))]);
        let mut logs = HashMap::new();
        scan(&r, &mut logs, 0);
        assert_eq!(logs.len(), 3);
        let s = collect(&logs, 0);
        assert_eq!(s.len(), 2);
        let claude = s.iter().find(|s| s.agent == UsageAgent::Claude).unwrap();
        assert_eq!(claude.session_id, "s1");
        assert_eq!(claude.cwd, "/w/feat/crates");
        assert_eq!(claude.models, ["claude-opus-5-5", "claude-haiku-5-5"]);
        assert_eq!(claude.tokens.input, 17);
        let codex_s = s.iter().find(|s| s.agent == UsageAgent::Codex).unwrap();
        assert_eq!(codex_s.session_id, "rollout-1");
        assert_eq!(codex_s.models, ["unknown"]);
        assert_eq!(codex_s.unpriced_tokens, 110);

        // Appended: a complete line and one still being written.
        let before = logs[&main].offset;
        let next = claude_line("m2", "2026-10-28T05:00:00Z", "claude-opus-5-5", 5, 5);
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&main)
                .unwrap();
            writeln!(f, "{next}").unwrap();
            write!(f, "{{\"type\":\"assistant\",\"usage\"").unwrap();
        }
        scan(&r, &mut logs, 0);
        assert_eq!(logs[&main].offset, before + next.len() as u64 + 1);
        let s = collect(&logs, 0);
        let claude = s.iter().find(|s| s.agent == UsageAgent::Claude).unwrap();
        assert_eq!(claude.tokens.input, 22);
        assert_eq!(claude.hours.len(), 2);
        assert_eq!(
            claude.last_ms,
            parse_time_ms("2026-10-28T05:00:00Z").unwrap()
        );
        assert_eq!(claude.first_ms, T0_MS);

        // The window leaves the first hour out.
        let s = collect(&logs, T0_MS + HOUR_MS);
        let claude = s.iter().find(|s| s.agent == UsageAgent::Claude).unwrap();
        assert_eq!(claude.tokens.input, 5);
        assert!(!s.iter().any(|s| s.agent == UsageAgent::Codex));

        // Rewritten from scratch (shorter): read again.
        std::fs::remove_file(&main).unwrap();
        write(&main, &[claude_line("m3", T0, "claude-opus-5-5", 1, 1)]);
        scan(&r, &mut logs, 0);
        let s = collect(&logs, 0);
        let claude = s.iter().find(|s| s.agent == UsageAgent::Claude).unwrap();
        assert_eq!(claude.tokens.input, 1 + 7);

        // Deleted logs are forgotten.
        std::fs::remove_file(&codex).unwrap();
        scan(&r, &mut logs, 0);
        assert_eq!(logs.len(), 2);
    }

    #[test]
    fn old_files_are_not_opened() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("claude/-w/old.jsonl");
        write(&main, &[claude_line("m1", T0, "claude-opus-5-5", 10, 10)]);
        let mut logs = HashMap::new();
        scan(&roots(dir.path()), &mut logs, now_ms() + HOUR_MS);
        assert!(logs.is_empty());
    }

    #[test]
    fn matches_the_deepest_worktree() {
        let wts = vec![
            ("/r".to_string(), WorktreeId::from("/r"), RepoId("r".into())),
            (
                "/r/.claude/worktrees/x".to_string(),
                WorktreeId::from("/r/.claude/worktrees/x"),
                RepoId("r".into()),
            ),
            (
                "/r2".to_string(),
                WorktreeId::from("/r2"),
                RepoId("r2".into()),
            ),
        ];
        let of = |cwd: &str| worktree_of(cwd, &wts).map(|w| w.0.as_str());
        assert_eq!(of("/r"), Some("/r"));
        assert_eq!(of("/r/src/"), Some("/r"));
        assert_eq!(
            of("/r/.claude/worktrees/x/src"),
            Some("/r/.claude/worktrees/x")
        );
        assert_eq!(of("/r2/a"), Some("/r2"));
        assert_eq!(of("/r22"), None);
        assert_eq!(of("/elsewhere"), None);

        let s = AgentSessionUsage {
            agent: UsageAgent::Claude,
            session_id: "s".into(),
            cwd: "/r2/a".into(),
            worktree: None,
            repo: None,
            models: vec![],
            first_ms: 1,
            last_ms: 2,
            tokens: TokenCounts::default(),
            cost_micros: 0,
            unpriced_tokens: 0,
            hours: vec![],
        };
        let r = respond(vec![s], &wts, 0);
        assert_eq!(r.sessions[0].worktree, Some(WorktreeId::from("/r2")));
        assert_eq!(r.sessions[0].repo, Some(RepoId("r2".into())));
        assert_eq!(r.prices_as_of, PRICES_AS_OF);
        assert!(!r.prices.is_empty());
    }
}
