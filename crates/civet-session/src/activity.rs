//! Agent activity timeline per worktree (see [`civet_proto::activity`]).
//!
//! A recorder thread follows the daemon's events. When an agent pane in a
//! worktree turns `Running` it opens a turn and snapshots the worktree; when
//! the pane leaves `Running` (or closes) it snapshots again and lists the
//! files that differ. Plain shells (OSC 133 running / idle per command) are
//! not recorded.
//!
//! A snapshot is a git tree of the working tree as `git add -A` would stage
//! it, written through a copy of the index (`GIT_INDEX_FILE`), so the user's
//! index and refs are never touched. Its objects are unreachable: `git gc`
//! prunes them after its grace period (two weeks by default), after which
//! that turn's diffs are gone but its file list stays.
//!
//! Timelines live in `<state>/activity/<hash>.json`, one file per worktree,
//! at most [`MAX_ACTIVITY_TURNS`] turns each; files untouched for
//! [`MAX_TIMELINE_AGE`] and all but the newest [`MAX_TIMELINES`] are deleted
//! at startup. A turn still open when the daemon stopped is closed as
//! `Unknown` when its timeline is loaded again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use civet_proto::{
    AgentActivity, AgentStatus, AgentTurn, Event, GitFileChange, MAX_ACTIVITY_TURNS,
    MAX_TURN_FILES, PaneId, WorktreeId,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;

use crate::SessionError;
use crate::git::{Git, parse_name_status_z, parse_numstat_z};
use crate::hooks::write_atomic;
use crate::pane::lock;

/// Under the state directory.
pub(crate) const ACTIVITY_DIR: &str = "activity";
/// Timeline files kept at most (newest by mtime).
const MAX_TIMELINES: usize = 200;
/// Timelines not written for this long are deleted at startup.
const MAX_TIMELINE_AGE: Duration = Duration::from_secs(30 * 24 * 3600);
/// The recorder checks this often whether the manager is gone.
const ALIVE_CHECK: Duration = Duration::from_secs(1);

/// What the recorder needs to know about a pane when its agent starts.
#[derive(Debug, Clone)]
pub(crate) struct PaneFacts {
    pub worktree: Option<WorktreeId>,
    pub is_agent: bool,
    /// Agent hint, else title.
    pub label: String,
}

/// One worktree's timeline as stored on disk.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Timeline {
    worktree: String,
    next_id: u64,
    turns: Vec<AgentTurn>,
}

/// Timelines by worktree, loaded lazily, written through on every change.
#[derive(Debug)]
pub(crate) struct ActivityStore {
    dir: PathBuf,
    timelines: HashMap<String, Timeline>,
    /// When this daemon run started: the end of turns an earlier run left open.
    started_ms: u64,
}

impl ActivityStore {
    /// Store in `dir` (created), pruning old timeline files.
    pub(crate) fn new(dir: PathBuf) -> Self {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!("cannot create {}: {e}", dir.display());
        }
        prune(&dir, MAX_TIMELINES, MAX_TIMELINE_AGE);
        Self {
            dir,
            timelines: HashMap::new(),
            started_ms: civet_proto::now_ms(),
        }
    }

    fn path(&self, worktree: &str) -> PathBuf {
        let digest = Sha256::digest(worktree.as_bytes());
        self.dir.join(format!("{}.json", hex::encode(&digest[..8])))
    }

    fn timeline(&mut self, worktree: &str) -> &mut Timeline {
        if !self.timelines.contains_key(worktree) {
            let t = self.load(worktree);
            self.timelines.insert(worktree.to_owned(), t);
        }
        self.timelines.get_mut(worktree).expect("just inserted")
    }

    fn load(&self, worktree: &str) -> Timeline {
        let path = self.path(worktree);
        let empty = || Timeline {
            worktree: worktree.to_owned(),
            ..Default::default()
        };
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return empty(),
            Err(e) => {
                tracing::warn!("cannot read {}: {e}", path.display());
                return empty();
            }
        };
        let mut t = match serde_json::from_slice::<Timeline>(&data) {
            // Why: a hash collision must not mix two worktrees' turns.
            Ok(t) if t.worktree == worktree => t,
            Ok(_) => return empty(),
            Err(e) => {
                tracing::warn!("corrupt {} ({e}); starting over", path.display());
                return empty();
            }
        };
        // Written by an earlier daemon run: its panes are gone.
        for turn in &mut t.turns {
            if turn.status == AgentStatus::Running {
                turn.status = AgentStatus::Unknown;
                turn.ended_ms
                    .get_or_insert(self.started_ms.max(turn.started_ms));
            }
        }
        t
    }

    fn save(&self, worktree: &str) {
        let Some(t) = self.timelines.get(worktree) else {
            return;
        };
        let path = self.path(worktree);
        let written = serde_json::to_vec(t)
            .map_err(std::io::Error::other)
            .and_then(|data| write_atomic(&path, &data));
        if let Err(e) = written {
            tracing::warn!("cannot write {}: {e}", path.display());
        }
    }

    /// The worktree's turns, oldest first.
    pub(crate) fn activity(&mut self, worktree: &str) -> AgentActivity {
        AgentActivity {
            turns: self.timeline(worktree).turns.clone(),
        }
    }

    /// Open a turn (dropping the oldest beyond the limit).
    fn begin(&mut self, worktree: &str, turn: AgentTurn) {
        let t = self.timeline(worktree);
        let id = t.next_id;
        t.next_id += 1;
        t.turns.push(AgentTurn { id, ..turn });
        let excess = t.turns.len().saturating_sub(MAX_ACTIVITY_TURNS);
        t.turns.drain(..excess);
        self.save(worktree);
    }

    /// The pane's open turn in the worktree.
    fn open_turn(&mut self, worktree: &str, pane: PaneId) -> Option<&mut AgentTurn> {
        self.timeline(worktree)
            .turns
            .iter_mut()
            .rev()
            .find(|t| t.pane == pane && t.status == AgentStatus::Running)
    }

    /// Change the pane's open turn (if any) and save.
    fn update(&mut self, worktree: &str, pane: PaneId, f: impl FnOnce(&mut AgentTurn)) {
        if let Some(turn) = self.open_turn(worktree, pane) {
            f(turn);
            self.save(worktree);
        }
    }
}

/// Delete timeline files older than `max_age`, then all but the newest
/// `keep`.
fn prune(dir: &Path, keep: usize, max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    let mut files: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .map(|e| {
            let mtime = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (mtime, e.path())
        })
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    for (i, (mtime, path)) in files.iter().enumerate() {
        let old = now.duration_since(*mtime).unwrap_or_default() > max_age;
        if i >= keep || old {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Snapshot the worktree at `dir` as a tree id (see the module docs).
/// `tmp_index` is scratch space, removed afterwards.
pub(crate) async fn snapshot(
    git: &Git,
    dir: &Path,
    tmp_index: &Path,
) -> Result<String, SessionError> {
    let index = git.run(dir, &["rev-parse", "--git-path", "index"]).await?;
    let index = dir.join(index.trim());
    let _ = std::fs::remove_file(tmp_index);
    // Why copy: git then only re-hashes files whose stat data changed.
    if index.is_file() {
        std::fs::copy(&index, tmp_index)?;
    }
    let env = [("GIT_INDEX_FILE", tmp_index)];
    let tree = async {
        git.run_env(dir, &["add", "-A"], &env).await?;
        git.run_env(dir, &["write-tree"], &env).await
    }
    .await;
    let _ = std::fs::remove_file(tmp_index);
    Ok(tree?.trim().to_owned())
}

/// Files that differ between two trees, with line counts, sorted by path;
/// true if more than [`MAX_TURN_FILES`] (the rest left out).
pub(crate) async fn changed_files(
    git: &Git,
    dir: &Path,
    from: &str,
    to: &str,
) -> Result<(Vec<GitFileChange>, bool), SessionError> {
    if from == to {
        return Ok((Vec::new(), false));
    }
    let names_args = ["diff", "--name-status", "-z", "-M", from, to];
    let counts_args = ["diff", "--numstat", "-z", "-M", from, to];
    let (names, counts) = tokio::join!(git.run(dir, &names_args), git.run(dir, &counts_args));
    let mut files = parse_name_status_z(&names?);
    let counts = parse_numstat_z(&counts?);
    for f in &mut files {
        if let Some(&(a, d)) = counts.get(&f.path) {
            f.additions = a;
            f.deletions = d;
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let truncated = files.len() > MAX_TURN_FILES;
    files.truncate(MAX_TURN_FILES);
    Ok((files, truncated))
}

/// Turns agent status events into timeline entries.
pub(crate) struct Recorder {
    store: Arc<Mutex<ActivityStore>>,
    git: Git,
    tmp_index: PathBuf,
    /// Panes with an open turn → its worktree.
    open: HashMap<PaneId, WorktreeId>,
}

impl Recorder {
    pub(crate) fn new(store: Arc<Mutex<ActivityStore>>, git: Git, tmp_index: PathBuf) -> Self {
        Self {
            store,
            git,
            tmp_index,
            open: HashMap::new(),
        }
    }

    /// Snapshot, logging (not failing) when it cannot be taken: a folder
    /// project, a deleted worktree.
    async fn snapshot(&self, worktree: &WorktreeId) -> Option<String> {
        match snapshot(&self.git, Path::new(&worktree.0), &self.tmp_index).await {
            Ok(tree) => Some(tree),
            Err(e) => {
                tracing::debug!(worktree = %worktree.0, "no activity snapshot: {e}");
                None
            }
        }
    }

    /// Handle one daemon event. `pane` looks up a live pane.
    pub(crate) async fn on_event(
        &mut self,
        event: &Event,
        pane: &dyn Fn(PaneId) -> Option<PaneFacts>,
    ) {
        match event {
            Event::AgentStatus(e) if e.status == AgentStatus::Running => {
                if let Some(wt) = self.open.get(&e.pane) {
                    // The prompt hook, after output already made it Running.
                    if let Some(prompt) = &e.message {
                        lock(&self.store).update(&wt.0, e.pane, |t| {
                            t.prompt.get_or_insert_with(|| prompt.clone());
                        });
                    }
                    return;
                }
                let Some(facts) = pane(e.pane).filter(|f| f.is_agent) else {
                    return;
                };
                let Some(wt) = facts.worktree else { return };
                let from_tree = self.snapshot(&wt).await;
                let turn = AgentTurn {
                    id: 0,
                    pane: e.pane,
                    agent: facts.label,
                    prompt: e.message.clone(),
                    started_ms: e.at_ms,
                    ended_ms: None,
                    status: AgentStatus::Running,
                    message: None,
                    files: Vec::new(),
                    files_truncated: false,
                    from_tree,
                    to_tree: None,
                };
                lock(&self.store).begin(&wt.0, turn);
                self.open.insert(e.pane, wt);
            }
            Event::AgentStatus(e) => {
                self.end(e.pane, e.at_ms, e.status, e.message.clone()).await;
            }
            Event::PaneClosed { pane } => {
                self.end(*pane, civet_proto::now_ms(), AgentStatus::Unknown, None)
                    .await;
            }
            _ => {}
        }
    }

    /// Close the pane's open turn, if any.
    async fn end(
        &mut self,
        pane: PaneId,
        at_ms: u64,
        status: AgentStatus,
        message: Option<String>,
    ) {
        let Some(wt) = self.open.remove(&pane) else {
            return;
        };
        let from_tree = lock(&self.store)
            .open_turn(&wt.0, pane)
            .and_then(|t| t.from_tree.clone());
        let to_tree = self.snapshot(&wt).await;
        let (files, files_truncated) = match (&from_tree, &to_tree) {
            (Some(from), Some(to)) => changed_files(&self.git, Path::new(&wt.0), from, to)
                .await
                .unwrap_or_else(|e| {
                    tracing::debug!(worktree = %wt.0, "turn files: {e}");
                    (Vec::new(), false)
                }),
            _ => (Vec::new(), false),
        };
        lock(&self.store).update(&wt.0, pane, |t| {
            t.ended_ms = Some(at_ms);
            t.status = status;
            t.message = message;
            t.files = files;
            t.files_truncated = files_truncated;
            t.to_tree = to_tree;
        });
    }
}

/// The recorder thread's body: handles `events` until `alive` turns false
/// (checked every [`ALIVE_CHECK`] while idle) or the channel closes.
pub(crate) fn run(
    mut recorder: Recorder,
    mut events: broadcast::Receiver<Event>,
    pane: impl Fn(PaneId) -> Option<PaneFacts>,
    alive: impl Fn() -> bool,
) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::warn!("activity recorder not started: {e}");
            return;
        }
    };
    rt.block_on(async move {
        loop {
            match tokio::time::timeout(ALIVE_CHECK, events.recv()).await {
                Ok(Ok(event)) => recorder.on_event(&event, &pane).await,
                Ok(Err(broadcast::error::RecvError::Lagged(n))) => {
                    tracing::warn!("activity recorder missed {n} events");
                }
                Ok(Err(broadcast::error::RecvError::Closed)) => return,
                Err(_) if !alive() => return,
                Err(_) => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use civet_proto::{AgentStatusEvent, AgentStatusSource, GitChangeKind};
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            // Why: run from a git hook, these would point at the outer repo.
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    }

    fn repo() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap().join("repo");
        std::fs::create_dir(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.join("old.rs"), "fn main() {}\n".repeat(20)).unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-qm", "init"]);
        (tmp, dir)
    }

    fn status(pane: u64, status: AgentStatus, message: Option<&str>, at_ms: u64) -> Event {
        Event::AgentStatus(AgentStatusEvent {
            pane: PaneId(pane),
            status,
            previous: AgentStatus::Unknown,
            source: AgentStatusSource::Hook,
            message: message.map(str::to_owned),
            at_ms,
        })
    }

    fn agent_in(dir: &Path) -> impl Fn(PaneId) -> Option<PaneFacts> {
        let wt = WorktreeId(dir.to_string_lossy().into_owned());
        move |p: PaneId| {
            Some(PaneFacts {
                worktree: Some(wt.clone()),
                // Pane 2 is a plain shell.
                is_agent: p != PaneId(2),
                label: "claude".into(),
            })
        }
    }

    fn recorder(tmp: &Path) -> (Recorder, Arc<Mutex<ActivityStore>>) {
        let store = Arc::new(Mutex::new(ActivityStore::new(tmp.join("activity"))));
        let rec = Recorder::new(
            store.clone(),
            Git::new("git".into()),
            tmp.join("activity/index.tmp"),
        );
        (rec, store)
    }

    #[tokio::test]
    async fn snapshot_leaves_the_index_alone() {
        let (_tmp, dir) = repo();
        let g = Git::new("git".into());
        let tmp_index = dir.parent().unwrap().join("idx");
        let clean = snapshot(&g, &dir, &tmp_index).await.unwrap();
        assert_eq!(clean, git(&dir, &["rev-parse", "HEAD^{tree}"]).trim());
        std::fs::write(dir.join("new.txt"), "x\n").unwrap();
        let dirty = snapshot(&g, &dir, &tmp_index).await.unwrap();
        assert_ne!(dirty, clean);
        assert!(!tmp_index.exists());
        // Nothing staged, the untracked file still untracked.
        assert_eq!(git(&dir, &["status", "--porcelain"]), "?? new.txt\n");
        let (files, truncated) = changed_files(&g, &dir, &clean, &dirty).await.unwrap();
        assert!(!truncated);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "new.txt");
        assert_eq!(files[0].kind, GitChangeKind::Added);
        assert_eq!((files[0].additions, files[0].deletions), (Some(1), Some(0)));
        // Not a git worktree: no snapshot.
        let plain = dir.parent().unwrap().join("plain");
        std::fs::create_dir(&plain).unwrap();
        assert!(snapshot(&g, &plain, &tmp_index).await.is_err());
    }

    #[tokio::test]
    async fn turns_record_files_including_commits() {
        let (tmp, dir) = repo();
        let (mut rec, store) = recorder(tmp.path());
        let panes = agent_in(&dir);
        let wt = dir.to_string_lossy().into_owned();

        rec.on_event(&status(1, AgentStatus::Running, None, 1000), &panes)
            .await;
        // The prompt hook arrives after output made the pane Running.
        rec.on_event(
            &status(1, AgentStatus::Running, Some("Fix it"), 1001),
            &panes,
        )
        .await;
        let open = store.lock().unwrap().activity(&wt);
        assert_eq!(open.turns.len(), 1);
        assert_eq!(open.turns[0].status, AgentStatus::Running);
        assert_eq!(open.turns[0].prompt.as_deref(), Some("Fix it"));

        // The agent edits, renames and commits part of it.
        std::fs::write(dir.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        git(&dir, &["mv", "old.rs", "new.rs"]);
        git(&dir, &["commit", "-qam", "agent"]);
        std::fs::write(dir.join("notes.md"), "hi\n").unwrap();
        rec.on_event(
            &status(1, AgentStatus::NeedsInput, Some("Allow Bash?"), 5000),
            &panes,
        )
        .await;

        let turns = store.lock().unwrap().activity(&wt).turns;
        let t = &turns[0];
        assert_eq!(
            (t.status, t.ended_ms, t.message.as_deref()),
            (AgentStatus::NeedsInput, Some(5000), Some("Allow Bash?"))
        );
        let paths: Vec<_> = t
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.old_path.as_deref(), f.kind))
            .collect();
        assert_eq!(
            paths,
            vec![
                ("a.txt", None, GitChangeKind::Modified),
                ("new.rs", Some("old.rs"), GitChangeKind::Renamed),
                ("notes.md", None, GitChangeKind::Added),
            ]
        );
        assert_eq!(
            (t.files[0].additions, t.files[0].deletions),
            (Some(2), Some(1))
        );

        // The turn's diff of one file, from its snapshots.
        let g = Git::new("git".into());
        let (from, to) = (t.from_tree.clone().unwrap(), t.to_tree.clone().unwrap());
        let d = crate::changes::diff_trees(&g, &dir, &from, &to, "a.txt", None, 3)
            .await
            .unwrap();
        assert!(d.diff.contains("+TWO"), "{}", d.diff);
        assert!(
            crate::changes::diff_trees(&g, &dir, &from, "nope", "a.txt", None, 3)
                .await
                .is_err()
        );

        // A shell's command, and a status without an open turn: nothing.
        rec.on_event(&status(2, AgentStatus::Running, None, 6000), &panes)
            .await;
        rec.on_event(&status(1, AgentStatus::Done, None, 6500), &panes)
            .await;
        assert_eq!(store.lock().unwrap().activity(&wt).turns.len(), 1);

        // Next turn, no edits; the pane is closed while it runs.
        rec.on_event(&status(1, AgentStatus::Running, None, 7000), &panes)
            .await;
        rec.on_event(&Event::PaneClosed { pane: PaneId(1) }, &panes)
            .await;
        let turns = store.lock().unwrap().activity(&wt).turns;
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[1].id, 1);
        assert_eq!(turns[1].status, AgentStatus::Unknown);
        assert!(turns[1].files.is_empty());
        assert!(turns[1].ended_ms.is_some());
    }

    #[tokio::test]
    async fn timelines_persist_and_stay_bounded() {
        let (tmp, dir) = repo();
        let wt = dir.to_string_lossy().into_owned();
        {
            let (mut rec, store) = recorder(tmp.path());
            let panes = agent_in(&dir);
            for i in 0..MAX_ACTIVITY_TURNS as u64 + 5 {
                rec.on_event(&status(1, AgentStatus::Running, None, i * 10), &panes)
                    .await;
                rec.on_event(&status(1, AgentStatus::Done, None, i * 10 + 5), &panes)
                    .await;
            }
            // Still running when the daemon "stops".
            rec.on_event(&status(3, AgentStatus::Running, None, 99_999), &panes)
                .await;
            let turns = store.lock().unwrap().activity(&wt).turns;
            assert_eq!(turns.len(), MAX_ACTIVITY_TURNS);
            assert_eq!(turns[0].id, 6);
        }
        // A new daemon run reads it back; the open turn is closed.
        let (_rec, store) = recorder(tmp.path());
        let turns = store.lock().unwrap().activity(&wt).turns;
        assert_eq!(turns.len(), MAX_ACTIVITY_TURNS);
        let last = turns.last().unwrap();
        assert_eq!((last.id, last.status), (105, AgentStatus::Unknown));
        assert!(last.ended_ms.is_some_and(|ms| ms >= last.started_ms));
        assert_eq!(turns[0].status, AgentStatus::Done);
        // Another worktree has nothing.
        assert!(
            store
                .lock()
                .unwrap()
                .activity("/elsewhere")
                .turns
                .is_empty()
        );
    }

    #[test]
    fn prune_keeps_the_newest() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..5 {
            let p = tmp.path().join(format!("{i}.json"));
            std::fs::write(&p, "{}").unwrap();
            let t = SystemTime::now() - Duration::from_secs(100 * (5 - i));
            std::fs::File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(t)
                .unwrap();
        }
        std::fs::write(tmp.path().join("index.tmp"), "").unwrap();
        prune(tmp.path(), 3, Duration::from_secs(450));
        let mut left: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["2.json", "3.json", "4.json", "index.tmp"]);
        // Age: 2.json is 300 s old.
        prune(tmp.path(), 3, Duration::from_secs(250));
        assert!(!tmp.path().join("2.json").exists());
    }
}
