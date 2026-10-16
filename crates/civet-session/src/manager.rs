//! [`SessionManager`]: repos, worktrees, panes, input, subscriptions.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use civet_proto::{
    AgentStatus, ClientId, Event, InputMessage, KillSignal, LaunchAgentRequest, PaneId, PaneInfo,
    ProjectInfo, ProjectKind, RepoId, RepoInfo, ScrollbackPage, ScrollbackRequest,
    SpawnPaneRequest, TermSize, WorktreeId, WorktreeInfo,
};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::git::{
    Git, branch_dir_name, canonical_lenient, canonical_or_same, parse_worktree_porcelain,
    path_string, repo_id_for,
};
use crate::hooks::{self, HookFiles, ShellKind, basename};
use crate::pane::{Pane, SpawnParams, lock};
use crate::subscription::Subscription;
use crate::watch::WorktreeWatch;
use crate::{ENV_WORKTREE, InputOutcome, KNOWN_AGENTS, SessionConfig, SessionError};

/// Event broadcast capacity.
const EVENT_CAPACITY: usize = 1024;
/// Status ticker period (heuristics).
const TICK: Duration = Duration::from_millis(50);
/// Hook files are polled every this many ticks (250 ms).
const HOOK_POLL_TICKS: u32 = 5;
const REPOS_FILE: &str = "repos.json";
/// Files trashed from the Files tab (see [`crate::fileops`]).
const TRASH_DIR: &str = "trash";

/// A registered repository (or folder project) as persisted in
/// `repos.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredRepo {
    id: String,
    name: String,
    path: String,
    /// Missing in files written before folder projects: a repository.
    #[serde(default)]
    kind: ProjectKind,
}

impl StoredRepo {
    fn is_folder(&self) -> bool {
        self.kind == ProjectKind::Folder
    }

    /// Unsupported for a folder project: `what` needs git.
    fn require_git(&self, what: &str) -> Result<(), SessionError> {
        if self.is_folder() {
            return Err(SessionError::Unsupported(format!(
                "{} is a folder, not a git repository: no {what}",
                self.name
            )));
        }
        Ok(())
    }

    /// The only "worktree" of a folder project: its root.
    fn folder_root(&self) -> WorktreeInfo {
        let path = path_string(&canonical_lenient(Path::new(&self.path)));
        WorktreeInfo {
            id: WorktreeId(path.clone()),
            repo: RepoId(self.id.clone()),
            path,
            branch: None,
            head: None,
            is_main: true,
            locked: false,
        }
    }
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

/// `path` (`~` expanded) if it is a directory, else InvalidArgument.
fn project_dir(path: &str) -> Result<PathBuf, SessionError> {
    let dir = expand_home(path.trim());
    if !dir.is_dir() {
        return Err(SessionError::InvalidArgument(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    Ok(dir)
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
        let hook_files = hooks::install(&config.state_dir, config.cli_program.as_deref())?;
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
        let weak = Arc::downgrade(&inner);
        std::thread::Builder::new()
            .name("civet-worktree-watch".into())
            .spawn(move || worktree_watcher(weak))?;
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
        let default_branch = if !r.is_folder() && path.is_dir() {
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

    /// Registered repositories (folder projects left out: see
    /// [`Self::list_projects`]).
    pub async fn list_repos(&self) -> Result<Vec<RepoInfo>, SessionError> {
        let repos = self.inner.repos.lock().await.clone();
        let mut out = Vec::with_capacity(repos.len());
        for r in repos.iter().filter(|r| !r.is_folder()) {
            out.push(self.repo_info(r).await);
        }
        Ok(out)
    }

    /// Registered projects: repositories and folders, in the order added.
    pub async fn list_projects(&self) -> Result<Vec<ProjectInfo>, SessionError> {
        let repos = self.inner.repos.lock().await.clone();
        let mut out = Vec::with_capacity(repos.len());
        for r in &repos {
            out.push(ProjectInfo {
                repo: self.repo_info(r).await,
                kind: r.kind,
            });
        }
        Ok(out)
    }

    /// Register the repository containing `path`. RepoId = hex of the first
    /// 8 bytes of SHA-256(canonical common git dir). Persists repos.json and
    /// emits ReposChanged. AlreadyExists if registered; InvalidArgument if
    /// `path` is in no git repository.
    pub async fn add_repo(&self, path: &str) -> Result<RepoInfo, SessionError> {
        let dir = project_dir(path)?;
        let stored = match self.git_repo_at(&dir).await? {
            Some(stored) => stored,
            None => {
                return Err(SessionError::InvalidArgument(format!(
                    "{} is not a git repository",
                    dir.display()
                )));
            }
        };
        let stored = self.register(stored).await?;
        Ok(self.repo_info(&stored).await)
    }

    /// Like [`Self::add_repo`], but a directory in no git repository is
    /// registered as a [`ProjectKind::Folder`] (RepoId: the same hash of its
    /// canonical path).
    pub async fn add_project(&self, path: &str) -> Result<ProjectInfo, SessionError> {
        let dir = project_dir(path)?;
        let stored = match self.git_repo_at(&dir).await? {
            Some(stored) => stored,
            None => {
                let root = canonical_or_same(&dir);
                StoredRepo {
                    id: repo_id_for(&root),
                    name: root
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path_string(&root)),
                    path: path_string(&root),
                    kind: ProjectKind::Folder,
                }
            }
        };
        let stored = self.register(stored).await?;
        Ok(ProjectInfo {
            repo: self.repo_info(&stored).await,
            kind: stored.kind,
        })
    }

    /// The repository containing `dir`, or None if git says there is none.
    async fn git_repo_at(&self, dir: &Path) -> Result<Option<StoredRepo>, SessionError> {
        let git = &self.inner.git;
        let common = match git
            .run(
                dir,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )
            .await
        {
            Ok(c) => c,
            Err(SessionError::Git(msg)) => {
                tracing::debug!("{} is not in a git repository: {msg}", dir.display());
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        let common = canonical_or_same(Path::new(common.trim()));
        let list = git.run(dir, &["worktree", "list", "--porcelain"]).await?;
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
        Ok(Some(StoredRepo {
            id: repo_id_for(&common),
            name,
            path: path_string(&main_path),
            kind: ProjectKind::Git,
        }))
    }

    /// Persist a new project and emit ReposChanged (AlreadyExists if its id
    /// is registered).
    async fn register(&self, stored: StoredRepo) -> Result<StoredRepo, SessionError> {
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
        tracing::info!(repo = %stored.id, path = %stored.path, kind = ?stored.kind, "repo added");
        self.emit(Event::ReposChanged);
        Ok(stored)
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

    /// `prunable`: include worktrees whose directory is gone (git reports
    /// them as "prunable"). Why: clients never see those (selecting one
    /// could only fail), but removal must still find them by id.
    async fn worktrees_of(
        &self,
        r: &StoredRepo,
        prunable: bool,
    ) -> Result<Vec<WorktreeInfo>, SessionError> {
        if r.is_folder() {
            return Ok(vec![r.folder_root()]);
        }
        let out = self
            .inner
            .git
            .run(Path::new(&r.path), &["worktree", "list", "--porcelain"])
            .await?;
        Ok(parse_worktree_porcelain(&out)
            .into_iter()
            .enumerate()
            .filter(|(_, w)| !w.bare && (prunable || !w.prunable))
            .map(|(i, w)| {
                let path = path_string(&canonical_lenient(Path::new(&w.path)));
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

    /// `git worktree list --porcelain`, without bare entries and without
    /// "prunable" ones whose directory was deleted (removable by id until
    /// git prunes them; `create_worktree` does).
    pub async fn list_worktrees(&self, repo: &RepoId) -> Result<Vec<WorktreeInfo>, SessionError> {
        let r = self.find_repo(repo).await?;
        self.worktrees_of(&r, false).await
    }

    /// The repository's recent pull requests (`gh pr list`, see
    /// [`crate::github`]).
    pub async fn list_pull_requests(
        &self,
        repo: &RepoId,
    ) -> Result<Vec<civet_proto::PullRequestInfo>, SessionError> {
        let r = self.find_repo(repo).await?;
        r.require_git("pull requests")?;
        crate::github::list_pull_requests(Path::new(&r.path)).await
    }

    /// The worktree's uncommitted changes (see [`crate::changes::status`]).
    pub async fn git_status(
        &self,
        worktree: &WorktreeId,
    ) -> Result<civet_proto::GitStatusInfo, SessionError> {
        let (r, w) = self.find_worktree(worktree).await?;
        r.require_git("changes")?;
        crate::changes::status(&self.inner.git, Path::new(&w.path)).await
    }

    /// One file's diff in the worktree (see [`crate::changes::diff`]).
    pub async fn git_diff(
        &self,
        worktree: &WorktreeId,
        path: &str,
        old_path: Option<&str>,
        staged: bool,
        context_lines: u32,
    ) -> Result<civet_proto::GitDiffInfo, SessionError> {
        let (r, w) = self.find_worktree(worktree).await?;
        r.require_git("diffs")?;
        crate::changes::diff(
            &self.inner.git,
            Path::new(&w.path),
            path,
            old_path,
            staged,
            context_lines,
        )
        .await
    }

    /// The worktree's branch and upstream (see [`crate::git_actions::branch`]).
    pub async fn git_branch(
        &self,
        worktree: &WorktreeId,
    ) -> Result<civet_proto::GitBranchInfo, SessionError> {
        let (r, w) = self.find_worktree(worktree).await?;
        r.require_git("branch")?;
        crate::git_actions::branch(&self.inner.git, Path::new(&w.path)).await
    }

    /// Run a git write operation in the worktree (see
    /// [`crate::git_actions::run`]). Returns the snapshot of a
    /// [`civet_proto::GitAction::Discard`], None for the others.
    pub async fn git_action(
        &self,
        worktree: &WorktreeId,
        action: civet_proto::GitAction,
    ) -> Result<Option<civet_proto::GitDiscardSnapshot>, SessionError> {
        let (r, w) = self.find_worktree(worktree).await?;
        r.require_git("git actions")?;
        let out = crate::git_actions::run(&self.inner.git, Path::new(&w.path), action).await?;
        Ok(match out {
            crate::git_actions::ActionOutcome::Done => None,
            crate::git_actions::ActionOutcome::Discarded(s) => Some(s),
        })
    }

    /// `git worktree add [-b branch] -- <path> [base]`. Default path:
    /// `<parent of main worktree>/<repo name>-worktrees/<branch, '/' -> '-'>`.
    /// With `base` (must resolve to a commit: `git rev-parse --verify
    /// --end-of-options <base>^{commit}`, else InvalidArgument): create
    /// `branch` from it (the new branch does not track `base`); an existing
    /// `branch` is checked out only if it points at `base` and no worktree
    /// has it, else AlreadyExists. Without: check out `branch` if it exists,
    /// else create it from HEAD. Stale registrations of deleted worktree
    /// directories are pruned first. A newly created branch gets
    /// `branch.<b>.remote=origin` / `branch.<b>.merge=refs/heads/<b>` when
    /// an `origin` remote exists (no repo-wide config). Emits
    /// WorktreesChanged.
    pub async fn create_worktree(
        &self,
        repo: &RepoId,
        branch: &str,
        base: Option<&str>,
        path: Option<&str>,
    ) -> Result<WorktreeInfo, SessionError> {
        let r = self.find_repo(repo).await?;
        r.require_git("worktrees")?;
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
        // Why: git records the path it is given; registering the canonical
        // one keeps the worktree's id equal to what `git worktree list`
        // reports, even after a symlinked parent changes or disappears.
        let target = canonical_lenient(&target);
        let target_s = path_string(&target);
        // Why: a worktree whose directory was deleted without `git worktree
        // remove` still holds its path and branch, and `worktree add` then
        // refuses both ("missing but already registered", "already used by
        // worktree"). Prune only drops registrations whose directory is gone
        // and that are not locked.
        if let Err(e) = git.run(&main, &["worktree", "prune"]).await {
            tracing::debug!("git worktree prune: {e}");
        }
        let exists = git.branch_exists(&main, branch).await?;
        // `base` is user input: it must name a commit, never an option.
        let base = match base.map(str::trim) {
            Some(b) => Some((b.to_owned(), git.verify_commit(&main, b).await?)),
            None => None,
        };
        let mut args: Vec<&str> = vec!["worktree", "add"];
        let created_branch = match &base {
            Some((_, sha)) if exists => {
                // Why: removing a worktree keeps its branch, so re-creating
                // the same name right after would always fail. A branch that
                // still points at the base and is not checked out anywhere
                // holds no work of its own: check it out instead.
                if !self.can_adopt_branch(&r, branch, sha).await? {
                    return Err(SessionError::AlreadyExists(format!("branch {branch}")));
                }
                args.extend(["--", &target_s, branch]);
                false
            }
            Some((b, _)) => {
                args.extend(["--no-track", "-b", branch, "--", &target_s, b]);
                true
            }
            None if exists => {
                args.extend(["--", &target_s, branch]);
                false
            }
            None => {
                args.extend(["-b", branch, "--", &target_s]);
                true
            }
        };
        git.run(&main, &args).await?;
        if created_branch {
            // Let a plain `git push` / `git status` in the new worktree use
            // origin/<branch>: per-branch upstream config only (never
            // repo-wide push.* settings, which would change `git push` in
            // the user's other checkouts). Best effort; nothing without an
            // origin remote.
            if let Err(e) = git.set_branch_upstream(&main, branch, "origin").await {
                tracing::debug!(branch, "branch upstream config: {e}");
            }
        }
        let canonical = path_string(&canonical_or_same(&target));
        let wt = self
            .worktrees_of(&r, false)
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

    /// True if local `branch` points at `sha` and no worktree has it checked
    /// out.
    async fn can_adopt_branch(
        &self,
        r: &StoredRepo,
        branch: &str,
        sha: &str,
    ) -> Result<bool, SessionError> {
        let main = Path::new(&r.path);
        let tip = self
            .inner
            .git
            .verify_commit(main, &format!("refs/heads/{branch}"))
            .await?;
        if tip != sha {
            return Ok(false);
        }
        let list = self.worktrees_of(r, true).await?;
        Ok(!list.iter().any(|w| w.branch.as_deref() == Some(branch)))
    }

    async fn find_worktree(
        &self,
        worktree: &WorktreeId,
    ) -> Result<(StoredRepo, WorktreeInfo), SessionError> {
        let wanted = path_string(&canonical_lenient(Path::new(&worktree.0)));
        let repos = self.inner.repos.lock().await.clone();
        for r in repos {
            let Ok(list) = self.worktrees_of(&r, true).await else {
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

    /// A worktree's directory, for the file requests below.
    async fn worktree_root(&self, worktree: &WorktreeId) -> Result<PathBuf, SessionError> {
        let (_, w) = self.find_worktree(worktree).await?;
        Ok(PathBuf::from(w.path))
    }

    /// A worktree's directory, and the git to ask about ignored files (None
    /// in a folder project, where nothing is ignored).
    async fn listing_root(
        &self,
        worktree: &WorktreeId,
    ) -> Result<(PathBuf, Option<&Git>), SessionError> {
        let (r, w) = self.find_worktree(worktree).await?;
        let git = (!r.is_folder()).then_some(&self.inner.git);
        Ok((PathBuf::from(w.path), git))
    }

    /// The folder project whose root is `worktree`, if any (no git runs).
    async fn folder_of(&self, worktree: &WorktreeId) -> Option<StoredRepo> {
        let wanted = path_string(&canonical_lenient(Path::new(&worktree.0)));
        self.inner
            .repos
            .lock()
            .await
            .iter()
            .find(|r| r.is_folder() && r.folder_root().path == wanted)
            .cloned()
    }

    /// One directory level of a worktree (`dir` relative to its root),
    /// gitignored entries left out unless `include_ignored`. Paths that
    /// leave the worktree are InvalidArgument (see [`crate::files`]).
    pub async fn list_files(
        &self,
        worktree: &WorktreeId,
        dir: &str,
        include_ignored: bool,
    ) -> Result<civet_proto::DirListing, SessionError> {
        let (root, git) = self.listing_root(worktree).await?;
        crate::files::list_dir(git, &root, dir, include_ignored).await
    }

    /// The start of a worktree file for a read-only preview (same path
    /// rules as [`Self::list_files`]).
    pub async fn read_file(
        &self,
        worktree: &WorktreeId,
        path: &str,
        max_bytes: u64,
    ) -> Result<civet_proto::FilePreview, SessionError> {
        let root = self.worktree_root(worktree).await?;
        crate::files::read_file(&root, path, max_bytes).await
    }

    /// Up to `len` bytes of a worktree file from `offset`, for downloads
    /// (same path rules as [`Self::list_files`]).
    pub async fn read_file_chunk(
        &self,
        worktree: &WorktreeId,
        path: &str,
        offset: u64,
        len: u64,
    ) -> Result<civet_proto::FileChunk, SessionError> {
        let root = self.worktree_root(worktree).await?;
        crate::files::read_chunk(&root, path, offset, len).await
    }

    /// Create, move, copy, write, trash or restore worktree files (see
    /// [`crate::fileops`]). Trash goes to `<state>/trash`.
    pub async fn file_op(
        &self,
        worktree: &WorktreeId,
        op: civet_proto::FileOp,
    ) -> Result<civet_proto::FileOpResult, SessionError> {
        let root = self.worktree_root(worktree).await?;
        let trash = self.inner.config.state_dir.join(TRASH_DIR);
        crate::fileops::apply(&root, trash, op).await
    }

    /// Every tracked and untracked, not ignored file of a worktree (every
    /// file of a folder project).
    pub async fn list_all_files(
        &self,
        worktree: &WorktreeId,
    ) -> Result<civet_proto::FileList, SessionError> {
        let (root, git) = self.listing_root(worktree).await?;
        crate::files::list_all(git, &root).await
    }

    /// `git worktree remove [--force]`. Refuses the main worktree. With
    /// `force`, a locked worktree is removed too (`-f -f`). A worktree whose
    /// directory is already gone is unregistered with `git worktree prune`
    /// when `remove` fails (older git refuses a missing directory). Fails
    /// unless git no longer lists the worktree afterwards. Emits
    /// WorktreesChanged whenever git was asked to change something.
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
        let git = &self.inner.git;
        let main = Path::new(&r.path);
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
            if w.locked {
                // Why: git wants `-f` twice for a locked worktree; "Remove
                // anyway" in the UI means exactly that.
                args.push("--force");
            }
        }
        args.push(&w.path);
        if let Err(e) = git.run(main, &args).await {
            if Path::new(&w.path).exists() {
                return Err(e);
            }
            // Why: the directory was deleted behind git's back (or an earlier
            // remove died half way): only the registration is left, and
            // prune clears it. A locked one stays unless the user forces it.
            if w.locked {
                if !force {
                    return Err(e);
                }
                let _ = git.run(main, &["worktree", "unlock", &w.path]).await;
            }
            tracing::info!(path = %w.path, "worktree directory missing, pruning: {e}");
            git.run(main, &["worktree", "prune"]).await?;
        }
        // Why: tell clients to re-list either way; they may have hidden the
        // row optimistically and must see it again if git kept it.
        self.emit(Event::WorktreesChanged {
            repo: RepoId(r.id.clone()),
        });
        if self
            .worktrees_of(&r, true)
            .await?
            .iter()
            .any(|x| x.id == w.id)
        {
            return Err(SessionError::Git(format!(
                "{} is still registered after git worktree remove",
                w.path
            )));
        }
        tracing::info!(repo = %r.id, path = %w.path, "worktree removed");
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
    ///   command string: `[$SHELL, -lc, program]`. csh/tcsh get `-c` and
    ///   `exec $argv:q` instead (they reject `-lc`).
    /// * env: daemon env + TERM, COLORTERM, CIVET_PANE_ID,
    ///   CIVET_STATUS_FILE, CIVET_SHIM_DIR, PATH (shim dir first),
    ///   TERM_PROGRAM=civet, shell integration (zsh `ZDOTDIR`, bash
    ///   `PROMPT_COMMAND`, fish `-C`) keeping the shim dir first after the
    ///   user's rc files, CIVET_WORKTREE, then `req.env`. `-lc` commands are
    ///   prefixed with `PATH="$CIVET_SHIM_DIR:$PATH"; `.
    /// * Claude Code / Codex get their hooks from the PATH shims
    ///   (`<state>/hooks/bin`); an explicit-path `claude` program (or the
    ///   `claude` agent hint on another launcher) gets `--settings` appended
    ///   to its argv.
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
        if let Some(w) = &req.worktree {
            if let Some(r) = self.folder_of(w).await {
                return Err(SessionError::Unsupported(format!(
                    "{} is a folder project: it has no sessions",
                    r.name
                )));
            }
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
        let kind = ShellKind::of(&shell);
        let setup = hooks::shell_setup(&shell, &self.inner.hook_files, |k| std::env::var(k).ok());
        // Runs after the login files, so the agent shims stay first on PATH
        // even if they prepend their own directories.
        let prefix = kind.path_prefix();
        let agent = req.agent.clone().filter(|a| !a.trim().is_empty());
        let (argv, display, program_base) = match &req.command {
            None => {
                let mut argv = vec![shell.clone(), "-l".to_owned()];
                argv.extend(setup.interactive_args.iter().cloned());
                (
                    argv,
                    vec![shell.clone(), "-l".to_owned()],
                    basename(&shell).to_owned(),
                )
            }
            Some(cmd) if cmd.program.trim().is_empty() => {
                return Err(SessionError::InvalidArgument("empty command".into()));
            }
            Some(cmd) if is_command_string(cmd) => {
                // `claude`, `FOO=1 claude`, `cd x && claude` all reach the
                // PATH shim, which adds the hook settings.
                let base = basename(first_word(&cmd.program)).to_owned();
                let line = cmd.program.trim().to_owned();
                (
                    vec![
                        shell.clone(),
                        kind.command_flag().to_owned(),
                        format!("{prefix}{line}"),
                    ],
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
                    kind.command_flag().to_owned(),
                    format!("{prefix}{}", kind.exec_args()),
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
        env.extend(setup.env);
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
        r.require_git("worktrees or sessions")?;
        let main = PathBuf::from(&r.path);
        let git = &self.inner.git;
        git.check_branch_name(&main, &req.branch).await?;
        // An existing branch is refused (or adopted) by create_worktree.
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

    /// Running panes' root processes (pane, pid, worktree), for attributing
    /// listening ports to panes.
    pub fn pane_processes(&self) -> Vec<(PaneId, u32, Option<WorktreeId>)> {
        lock(&self.inner.panes)
            .values()
            .filter(|p| !p.is_exited())
            .filter_map(|p| Some((p.id, p.pid()?, p.worktree())))
            .collect()
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

    /// After [`SessionError::Busy`] from `write_input`: resolves once the
    /// pane's PTY input queue has room again (or the pane is gone); then
    /// retry the same message. Awaiting this in the connection's input path
    /// is the backpressure (the client's sends block on flow control).
    pub async fn wait_input_capacity(&self, pane: PaneId) {
        if let Ok(p) = self.pane(pane) {
            p.wait_input_capacity().await;
        }
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

/// Emits WorktreesChanged for repos whose worktrees changed outside Civet
/// (see [`crate::watch`]), every `worktree_poll`. Exits when the manager is
/// dropped or shut down.
fn worktree_watcher(weak: Weak<Inner>) {
    let mut watch = WorktreeWatch::default();
    loop {
        let Some(inner) = weak.upgrade() else { return };
        if inner.shutdown.load(Ordering::SeqCst) {
            return;
        }
        let period = inner.config.worktree_poll;
        // Why: blocking_lock is fine on this plain thread; a repo being
        // added right now is simply picked up on the next round.
        let repos: Vec<(String, PathBuf)> = inner
            .repos
            .blocking_lock()
            .iter()
            .filter(|r| !r.is_folder())
            .map(|r| (r.id.clone(), PathBuf::from(&r.path)))
            .collect();
        for repo in watch.changed(&repos) {
            tracing::debug!(%repo, "worktrees changed on disk");
            let _ = inner
                .events
                .send(Event::WorktreesChanged { repo: RepoId(repo) });
        }
        drop(inner);
        // Sleep in short steps so a dropped manager's thread ends promptly.
        let until = Instant::now() + period;
        while Instant::now() < until {
            std::thread::sleep(TICK.min(period));
            if weak.strong_count() == 0 {
                return;
            }
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
