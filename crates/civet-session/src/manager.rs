//! [`SessionManager`]: repos, worktrees, panes, input, subscriptions.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use civet_proto::{
    AgentStatus, ClientId, Event, InputMessage, KillSignal, LaunchAgentRequest, PaneId, PaneInfo,
    RepoId, RepoInfo, ScrollbackPage, ScrollbackRequest, SpawnPaneRequest, TermSize, WorktreeId,
    WorktreeInfo,
};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::git::{
    Git, branch_dir_name, canonical_or_same, parse_worktree_porcelain, path_string, repo_id_for,
};
use crate::hooks::{self, HookFiles, basename, sh_quote};
use crate::pane::{Pane, SpawnParams, lock};
use crate::subscription::Subscription;
use crate::{ENV_WORKTREE, InputOutcome, KNOWN_AGENTS, SessionConfig, SessionError};

/// Event broadcast capacity.
const EVENT_CAPACITY: usize = 1024;
/// Status ticker period (heuristics).
const TICK: Duration = Duration::from_millis(50);
/// Hook files are polled every this many ticks (250 ms).
const HOOK_POLL_TICKS: u32 = 5;
const REPOS_FILE: &str = "repos.json";

/// A registered repository as persisted in `repos.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredRepo {
    id: String,
    name: String,
    path: String,
}

struct Inner {
    config: SessionConfig,
    shell: String,
    git: Git,
    hook_files: HookFiles,
    events: broadcast::Sender<Event>,
    panes: Mutex<BTreeMap<PaneId, Arc<Pane>>>,
    next_pane: AtomicU64,
    repos: tokio::sync::Mutex<Vec<StoredRepo>>,
    shutdown: AtomicBool,
}

/// Owns repos/worktrees, panes, input sequencing and subscriptions.
/// Clone = same manager (Arc inside). Send + Sync.
#[derive(Clone)]
pub struct SessionManager {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for SessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionManager")
            .field("state_dir", &self.inner.config.state_dir)
            .field("shell", &self.inner.shell)
            .field("panes", &lock(&self.inner.panes).len())
            .finish_non_exhaustive()
    }
}

fn default_shell(configured: Option<&str>) -> String {
    if let Some(s) = configured.filter(|s| !s.is_empty()) {
        return s.to_owned();
    }
    if let Some(s) = std::env::var("SHELL").ok().filter(|s| !s.is_empty()) {
        return s;
    }
    if Path::new("/bin/bash").exists() {
        "/bin/bash".to_owned()
    } else {
        "/bin/sh".to_owned()
    }
}

/// `~` / `~/x` → home-relative path.
fn expand_home(p: &str) -> PathBuf {
    if p == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(p));
    }
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(p)
}

fn load_repos(path: &Path) -> Vec<StoredRepo> {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!("cannot read {}: {e}", path.display());
            return Vec::new();
        }
    };
    match serde_json::from_slice(&data) {
        Ok(v) => v,
        Err(e) => {
            let backup = path.with_extension("json.corrupt");
            tracing::warn!(
                "corrupt {} ({e}); moved to {}",
                path.display(),
                backup.display()
            );
            let _ = std::fs::rename(path, &backup);
            Vec::new()
        }
    }
}

/// First word of a command string (for `program` given as a whole string).
fn first_word(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or("")
}

fn is_command_string(cmd: &civet_proto::CommandSpec) -> bool {
    cmd.args.is_empty() && cmd.program.trim().contains(char::is_whitespace)
}

impl SessionManager {
    /// Loads `<state_dir>/repos.json` (missing = empty), creates
    /// `<state_dir>/status/`, writes `<state_dir>/hooks/`, starts the status
    /// ticker thread.
    pub fn new(config: SessionConfig) -> Result<Self, SessionError> {
        std::fs::create_dir_all(config.state_dir.join("status"))?;
        let hook_files = hooks::install(&config.state_dir)?;
        let repos = load_repos(&config.state_dir.join(REPOS_FILE));
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let inner = Arc::new(Inner {
            shell: default_shell(config.shell.as_deref()),
            git: Git::new(config.git.clone()),
            hook_files,
            events,
            panes: Mutex::new(BTreeMap::new()),
            next_pane: AtomicU64::new(1),
            repos: tokio::sync::Mutex::new(repos),
            shutdown: AtomicBool::new(false),
            config,
        });
        let weak = Arc::downgrade(&inner);
        std::thread::Builder::new()
            .name("civet-status-ticker".into())
            .spawn(move || ticker(weak))?;
        Ok(Self { inner })
    }

    /// All daemon events (pane lifecycle, titles, status, bells, repo
    /// changes). Lagging receivers lose events (broadcast); capacity 1024.
    pub fn subscribe_events(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    /// The shell used for panes.
    pub fn shell(&self) -> &str {
        &self.inner.shell
    }

    fn emit(&self, e: Event) {
        let _ = self.inner.events.send(e);
    }

    // ----- repos & worktrees -------------------------------------------

    async fn save_repos(&self, repos: &[StoredRepo]) -> Result<(), SessionError> {
        let path = self.inner.config.state_dir.join(REPOS_FILE);
        let data = serde_json::to_vec_pretty(repos).map_err(std::io::Error::other)?;
        tokio::task::spawn_blocking(move || hooks::write_atomic(&path, &data))
            .await
            .map_err(std::io::Error::other)??;
        Ok(())
    }

    async fn repo_info(&self, r: &StoredRepo) -> RepoInfo {
        let path = Path::new(&r.path);
        let default_branch = if path.is_dir() {
            self.inner.git.default_branch(path).await
        } else {
            None
        };
        RepoInfo {
            id: RepoId(r.id.clone()),
            name: r.name.clone(),
            path: r.path.clone(),
            default_branch,
        }
    }

    async fn find_repo(&self, repo: &RepoId) -> Result<StoredRepo, SessionError> {
        self.inner
            .repos
            .lock()
            .await
            .iter()
            .find(|r| r.id == repo.0)
            .cloned()
            .ok_or_else(|| SessionError::NotFound(format!("repo {repo}")))
    }

    /// Registered repositories.
    pub async fn list_repos(&self) -> Result<Vec<RepoInfo>, SessionError> {
        let repos = self.inner.repos.lock().await.clone();
        let mut out = Vec::with_capacity(repos.len());
        for r in &repos {
            out.push(self.repo_info(r).await);
        }
        Ok(out)
    }

    /// Register the repository containing `path`. RepoId = hex of the first
    /// 8 bytes of SHA-256(canonical common git dir). Persists repos.json and
    /// emits ReposChanged. AlreadyExists if registered.
    pub async fn add_repo(&self, path: &str) -> Result<RepoInfo, SessionError> {
        let dir = expand_home(path.trim());
        if !dir.is_dir() {
            return Err(SessionError::InvalidArgument(format!(
                "{} is not a directory",
                dir.display()
            )));
        }
        let git = &self.inner.git;
        let common = git
            .run(
                &dir,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )
            .await
            .map_err(|e| match e {
                SessionError::Git(msg) => SessionError::InvalidArgument(format!(
                    "{} is not a git repository: {msg}",
                    dir.display()
                )),
                other => other,
            })?;
        let common = canonical_or_same(Path::new(common.trim()));
        let list = git.run(&dir, &["worktree", "list", "--porcelain"]).await?;
        let main = parse_worktree_porcelain(&list)
            .into_iter()
            .next()
            .ok_or_else(|| SessionError::Git("git worktree list returned nothing".into()))?;
        let main_path = canonical_or_same(Path::new(&main.path));
        let raw_name = main_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".to_owned());
        let name = if main.bare {
            raw_name
                .strip_suffix(".git")
                .unwrap_or(&raw_name)
                .to_owned()
        } else {
            raw_name
        };
        let stored = StoredRepo {
            id: repo_id_for(&common),
            name,
            path: path_string(&main_path),
        };
        {
            let mut repos = self.inner.repos.lock().await;
            if repos.iter().any(|r| r.id == stored.id) {
                return Err(SessionError::AlreadyExists(format!(
                    "repo {} ({})",
                    stored.name, stored.path
                )));
            }
            repos.push(stored.clone());
            self.save_repos(&repos).await?;
        }
        tracing::info!(repo = %stored.id, path = %stored.path, "repo added");
        self.emit(Event::ReposChanged);
        Ok(self.repo_info(&stored).await)
    }

    /// Unregister a repository (files and worktrees are untouched).
    pub async fn remove_repo(&self, repo: &RepoId) -> Result<(), SessionError> {
        {
            let mut repos = self.inner.repos.lock().await;
            let before = repos.len();
            repos.retain(|r| r.id != repo.0);
            if repos.len() == before {
                return Err(SessionError::NotFound(format!("repo {repo}")));
            }
            self.save_repos(&repos).await?;
        }
        self.emit(Event::ReposChanged);
        Ok(())
    }

    async fn worktrees_of(&self, r: &StoredRepo) -> Result<Vec<WorktreeInfo>, SessionError> {
        let out = self
            .inner
            .git
            .run(Path::new(&r.path), &["worktree", "list", "--porcelain"])
            .await?;
        Ok(parse_worktree_porcelain(&out)
            .into_iter()
            .enumerate()
            .filter(|(_, w)| !w.bare)
            .map(|(i, w)| {
                let path = path_string(&canonical_or_same(Path::new(&w.path)));
                WorktreeInfo {
                    id: WorktreeId(path.clone()),
                    repo: RepoId(r.id.clone()),
                    path,
                    branch: w.branch,
                    head: w.head,
                    is_main: i == 0,
                    locked: w.locked,
                }
            })
            .collect())
    }

    /// `git worktree list --porcelain` (bare entries omitted).
    pub async fn list_worktrees(&self, repo: &RepoId) -> Result<Vec<WorktreeInfo>, SessionError> {
        let r = self.find_repo(repo).await?;
        self.worktrees_of(&r).await
    }

    /// `git worktree add [-b branch] <path> [base]`. Default path:
    /// `<parent of main worktree>/<repo name>-worktrees/<branch, '/' -> '-'>`.
    /// With `base`: create `branch` from it (AlreadyExists if the branch
    /// exists; the new branch does not track `base`). Without: check out
    /// `branch` if it exists, else create it from HEAD. Emits
    /// WorktreesChanged.
    pub async fn create_worktree(
        &self,
        repo: &RepoId,
        branch: &str,
        base: Option<&str>,
        path: Option<&str>,
    ) -> Result<WorktreeInfo, SessionError> {
        let r = self.find_repo(repo).await?;
        let git = &self.inner.git;
        let main = PathBuf::from(&r.path);
        git.check_branch_name(&main, branch).await?;
        let target = match path.map(str::trim).filter(|p| !p.is_empty()) {
            Some(p) => {
                let p = expand_home(p);
                if p.is_absolute() {
                    p
                } else {
                    main.parent().unwrap_or(&main).join(p)
                }
            }
            None => main
                .parent()
                .unwrap_or(&main)
                .join(format!("{}-worktrees", r.name))
                .join(branch_dir_name(branch)),
        };
        if target.exists() {
            let empty_dir =
                target.is_dir() && std::fs::read_dir(&target).is_ok_and(|mut d| d.next().is_none());
            if !empty_dir {
                return Err(SessionError::AlreadyExists(format!(
                    "path {}",
                    target.display()
                )));
            }
        }
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let target_s = path_string(&target);
        let exists = git.branch_exists(&main, branch).await?;
        let mut args: Vec<&str> = vec!["worktree", "add"];
        let created_branch = match base {
            Some(b) => {
                if exists {
                    return Err(SessionError::AlreadyExists(format!("branch {branch}")));
                }
                args.extend(["--no-track", "-b", branch, &target_s, b]);
                true
            }
            None if exists => {
                args.extend([&target_s, branch]);
                false
            }
            None => {
                args.extend(["-b", branch, &target_s]);
                true
            }
        };
        git.run(&main, &args).await?;
        if created_branch {
            // Like Orca: with no upstream, let a plain `git push` publish the
            // branch (git >= 2.37). Best effort.
            if let Err(e) = git
                .run(
                    &target,
                    &["config", "--local", "push.autoSetupRemote", "true"],
                )
                .await
            {
                tracing::debug!("push.autoSetupRemote: {e}");
            }
        }
        let canonical = path_string(&canonical_or_same(&target));
        let wt = self
            .worktrees_of(&r)
            .await?
            .into_iter()
            .find(|w| w.path == canonical)
            .ok_or_else(|| {
                SessionError::Git(format!(
                    "worktree {canonical} missing after git worktree add"
                ))
            })?;
        tracing::info!(repo = %r.id, path = %wt.path, branch, "worktree created");
        self.emit(Event::WorktreesChanged {
            repo: RepoId(r.id.clone()),
        });
        Ok(wt)
    }

    async fn find_worktree(
        &self,
        worktree: &WorktreeId,
    ) -> Result<(StoredRepo, WorktreeInfo), SessionError> {
        let wanted = path_string(&canonical_or_same(Path::new(&worktree.0)));
        let repos = self.inner.repos.lock().await.clone();
        for r in repos {
            let Ok(list) = self.worktrees_of(&r).await else {
                continue;
            };
            if let Some(w) = list
                .into_iter()
                .find(|w| w.id == *worktree || w.path == wanted)
            {
                return Ok((r, w));
            }
        }
        Err(SessionError::NotFound(format!("worktree {worktree}")))
    }

    /// `git worktree remove [--force]`. Refuses the main worktree.
    pub async fn remove_worktree(
        &self,
        worktree: &WorktreeId,
        force: bool,
    ) -> Result<(), SessionError> {
        let (r, w) = self.find_worktree(worktree).await?;
        if w.is_main {
            return Err(SessionError::InvalidArgument(format!(
                "{} is the main worktree of {}",
                w.path, r.name
            )));
        }
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(&w.path);
        self.inner.git.run(Path::new(&r.path), &args).await?;
        tracing::info!(repo = %r.id, path = %w.path, "worktree removed");
        self.emit(Event::WorktreesChanged {
            repo: RepoId(r.id.clone()),
        });
        Ok(())
    }

    /// Base rev for a new agent branch: the default branch (local if it
    /// exists, else `origin/<name>`), else HEAD.
    async fn default_base(&self, main: &Path) -> String {
        let git = &self.inner.git;
        if let Some(name) = git.default_branch(main).await {
            if git.branch_exists(main, &name).await.unwrap_or(false) {
                return name;
            }
            let remote = format!("refs/remotes/origin/{name}");
            if git.ref_exists(main, &remote).await.unwrap_or(false) {
                return format!("origin/{name}");
            }
        }
        "HEAD".to_owned()
    }

    // ----- panes ---------------------------------------------------------

    fn pane(&self, pane: PaneId) -> Result<Arc<Pane>, SessionError> {
        lock(&self.inner.panes)
            .get(&pane)
            .cloned()
            .ok_or_else(|| SessionError::NotFound(pane.to_string()))
    }

    /// Spawn a PTY and start its reader/writer/waiter threads and status
    /// detection. Emits PaneCreated.
    ///
    /// * cwd: `req.cwd`, else the worktree path, else `$HOME` (must exist).
    /// * argv: no command → `[$SHELL, -l]`; with command →
    ///   `[$SHELL, -lc, exec "$0" "$@", program, args...]`. A command with no
    ///   args whose `program` contains whitespace is treated as a shell
    ///   command string: `[$SHELL, -lc, program]`.
    /// * env: daemon env + TERM, COLORTERM, CIVET_PANE_ID,
    ///   CIVET_STATUS_FILE, CIVET_WORKTREE, then `req.env`.
    /// * Claude Code gets `--settings <state>/hooks/claude-settings.json`.
    pub async fn spawn_pane(&self, req: SpawnPaneRequest) -> Result<PaneInfo, SessionError> {
        if self.inner.shutdown.load(Ordering::SeqCst) {
            return Err(SessionError::Spawn(
                "session manager is shutting down".into(),
            ));
        }
        if req.size.cols == 0 || req.size.rows == 0 {
            return Err(SessionError::InvalidArgument(format!(
                "invalid size {}x{}",
                req.size.cols, req.size.rows
            )));
        }
        let cwd = match (&req.cwd, &req.worktree) {
            (Some(c), _) => expand_home(c),
            (None, Some(w)) => PathBuf::from(&w.0),
            (None, None) => dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
        };
        if !cwd.is_dir() {
            return Err(SessionError::InvalidArgument(format!(
                "working directory {} does not exist",
                cwd.display()
            )));
        }
        let shell = self.inner.shell.clone();
        let agent = req.agent.clone().filter(|a| !a.trim().is_empty());
        let (argv, display, program_base) = match &req.command {
            None => (
                vec![shell.clone(), "-l".to_owned()],
                vec![shell.clone(), "-l".to_owned()],
                basename(&shell).to_owned(),
            ),
            Some(cmd) if cmd.program.trim().is_empty() => {
                return Err(SessionError::InvalidArgument("empty command".into()));
            }
            Some(cmd) if is_command_string(cmd) => {
                let base = basename(first_word(&cmd.program)).to_owned();
                let mut line = cmd.program.trim().to_owned();
                let is_claude = base == "claude" || agent.as_deref().is_some_and(|a| a == "claude");
                if is_claude && !line.contains("--settings") {
                    line.push_str(" --settings ");
                    line.push_str(&sh_quote(
                        &self.inner.hook_files.claude_settings.to_string_lossy(),
                    ));
                }
                (
                    vec![shell.clone(), "-lc".to_owned(), line.clone()],
                    vec![line],
                    base,
                )
            }
            Some(cmd) => {
                let args = hooks::claude_args(
                    &cmd.program,
                    &cmd.args,
                    agent.as_deref(),
                    &self.inner.hook_files,
                );
                let mut argv = vec![
                    shell.clone(),
                    "-lc".to_owned(),
                    "exec \"$0\" \"$@\"".to_owned(),
                    cmd.program.clone(),
                ];
                argv.extend(args.iter().cloned());
                let mut display = vec![cmd.program.clone()];
                display.extend(args);
                (argv, display, basename(&cmd.program).to_owned())
            }
        };
        let is_agent = agent.is_some()
            || (req.command.is_some() && KNOWN_AGENTS.contains(&program_base.as_str()));

        let id = PaneId(self.inner.next_pane.fetch_add(1, Ordering::SeqCst));
        let status_file = self
            .inner
            .config
            .state_dir
            .join("status")
            .join(format!("{}.status", id.0));
        tokio::fs::write(&status_file, b"").await?;

        let mut env: Vec<(String, String)> = vec![
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
            (civet_proto::ENV_PANE_ID.into(), id.0.to_string()),
            (
                civet_proto::ENV_STATUS_FILE.into(),
                status_file.to_string_lossy().into_owned(),
            ),
        ];
        if let Some(w) = &req.worktree {
            env.push((ENV_WORKTREE.into(), w.0.clone()));
        }
        env.extend(req.env.iter().cloned());

        let params = SpawnParams {
            id,
            argv,
            display_command: display,
            cwd,
            env,
            size: req.size,
            title: req
                .title
                .clone()
                .filter(|t| !t.is_empty())
                .unwrap_or(program_base),
            agent,
            is_agent,
            worktree: req.worktree.clone(),
            idle_after: self.inner.config.idle_after,
            scrollback_limit: self.inner.config.scrollback_limit,
            status_file: status_file.clone(),
        };
        let events = self.inner.events.clone();
        let spawned = tokio::task::spawn_blocking(move || Pane::spawn(params, events))
            .await
            .map_err(|e| SessionError::Spawn(format!("spawn task: {e}")))?;
        let pane = match spawned {
            Ok(p) => p,
            Err(e) => {
                let _ = std::fs::remove_file(&status_file);
                return Err(e);
            }
        };
        lock(&self.inner.panes).insert(id, pane.clone());
        Ok(pane.announce())
    }

    /// Create a worktree on a NEW branch (AlreadyExists if it exists) from
    /// `base` (default: the repo's default branch, else HEAD), then spawn
    /// `command` in it with `title.unwrap_or(branch)`. If the spawn fails the
    /// worktree is left in place and a Spawn error is returned.
    pub async fn launch_agent(
        &self,
        req: LaunchAgentRequest,
    ) -> Result<(WorktreeInfo, PaneInfo), SessionError> {
        let r = self.find_repo(&req.repo).await?;
        let main = PathBuf::from(&r.path);
        let git = &self.inner.git;
        git.check_branch_name(&main, &req.branch).await?;
        if git.branch_exists(&main, &req.branch).await? {
            return Err(SessionError::AlreadyExists(format!(
                "branch {}",
                req.branch
            )));
        }
        let base = match req.base.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
            Some(b) => b.to_owned(),
            None => self.default_base(&main).await,
        };
        let wt = self
            .create_worktree(
                &req.repo,
                &req.branch,
                Some(&base),
                req.worktree_path.as_deref(),
            )
            .await?;
        let spawn = SpawnPaneRequest {
            worktree: Some(wt.id.clone()),
            cwd: None,
            command: Some(req.command),
            env: req.env,
            size: req.size,
            title: Some(req.title.unwrap_or_else(|| req.branch.clone())),
            agent: req.agent,
        };
        let pane = self.spawn_pane(spawn).await.map_err(|e| match e {
            SessionError::Spawn(_) => e,
            other => SessionError::Spawn(other.to_string()),
        })?;
        Ok((wt, pane))
    }

    /// All panes (including exited ones), by id.
    pub fn list_panes(&self) -> Vec<PaneInfo> {
        let panes: Vec<_> = lock(&self.inner.panes).values().cloned().collect();
        panes.iter().map(|p| p.info()).collect()
    }

    /// One pane's info.
    pub fn pane_info(&self, pane: PaneId) -> Result<PaneInfo, SessionError> {
        Ok(self.pane(pane)?.info())
    }

    /// PTY resize (TIOCSWINSZ) + terminal resize. Emits PaneResized. Last
    /// caller wins.
    pub fn resize_pane(&self, pane: PaneId, size: TermSize) -> Result<(), SessionError> {
        self.pane(pane)?.resize(size)
    }

    /// Signal the process group, close the PTY, drop the pane. Emits
    /// PaneClosed. Subscriptions for it end (`next()` returns None). If the
    /// process survives a non-SIGKILL signal for 3 s it gets SIGKILL.
    pub fn kill_pane(&self, pane: PaneId, signal: KillSignal) -> Result<(), SessionError> {
        let p = lock(&self.inner.panes)
            .remove(&pane)
            .ok_or_else(|| SessionError::NotFound(pane.to_string()))?;
        p.kill(signal);
        tracing::info!(pane = pane.0, ?signal, "pane closed");
        self.emit(Event::PaneClosed { pane });
        Ok(())
    }

    /// Override a pane's agent status (source Client).
    pub fn set_agent_status(&self, pane: PaneId, status: AgentStatus) -> Result<(), SessionError> {
        self.pane(pane)?.set_status_from_client(status);
        Ok(())
    }

    /// Serve a scrollback page.
    pub fn fetch_scrollback(
        &self,
        req: &ScrollbackRequest,
    ) -> Result<ScrollbackPage, SessionError> {
        let pane = self.pane(req.pane)?;
        let term = lock(&pane.term);
        Ok(term.scrollback_page(req))
    }

    /// Visible screen rows as plain text (debugging, tests, CLI).
    pub fn screen_text(&self, pane: PaneId) -> Result<Vec<String>, SessionError> {
        let pane = self.pane(pane)?;
        let term = lock(&pane.term);
        Ok(term.screen_text())
    }

    // ----- input ---------------------------------------------------------

    /// Apply `msg` iff `msg.seq > last_input_seq(client, pane)`, then record
    /// seq and write time (echo ack). Gaps are allowed (logged). Never
    /// blocks on the PTY. Error if the pane exited.
    pub fn write_input(
        &self,
        client: ClientId,
        msg: &InputMessage,
    ) -> Result<InputOutcome, SessionError> {
        self.pane(msg.pane)?.write_input(client, msg.seq, &msg.data)
    }

    /// Highest applied input seq from `client` for `pane` (0 if none).
    pub fn last_input_seq(&self, client: ClientId, pane: PaneId) -> u64 {
        self.pane(pane)
            .map(|p| p.last_input_seq(client))
            .unwrap_or(0)
    }

    /// Forget all per-client input seqs (fresh, non-resumed session).
    pub fn reset_client(&self, client: ClientId) {
        let panes: Vec<_> = lock(&self.inner.panes).values().cloned().collect();
        for p in panes {
            p.forget_client(client);
        }
    }

    // ----- screen subscriptions -----------------------------------------

    /// New per-(client, pane) subscription. Its first `next()` yields a Full
    /// update (or a Diff after `reset_baseline` with a held state).
    pub fn subscribe(&self, client: ClientId, pane: PaneId) -> Result<Subscription, SessionError> {
        Ok(Subscription::new(client, self.pane(pane)?))
    }

    /// Kill all panes (SIGHUP), stop threads.
    pub async fn shutdown(&self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        let panes: Vec<_> = std::mem::take(&mut *lock(&self.inner.panes))
            .into_values()
            .collect();
        for p in panes {
            p.kill(KillSignal::Hangup);
            self.emit(Event::PaneClosed { pane: p.id });
        }
    }
}

/// Periodic status heuristics and hook-file polling. Exits when the manager
/// is dropped or shut down.
fn ticker(weak: Weak<Inner>) {
    let mut n: u32 = 0;
    loop {
        std::thread::sleep(TICK);
        let Some(inner) = weak.upgrade() else { return };
        if inner.shutdown.load(Ordering::SeqCst) {
            return;
        }
        let panes: Vec<_> = lock(&inner.panes).values().cloned().collect();
        drop(inner);
        n = n.wrapping_add(1);
        let now = Instant::now();
        for p in panes {
            if n % HOOK_POLL_TICKS == 0 {
                p.poll_hook_file();
            }
            p.tick(now);
        }
    }
}
