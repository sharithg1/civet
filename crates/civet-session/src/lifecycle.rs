//! Worktree lifecycle: where each worktree's branch stands against the
//! default branch and its upstream, which worktrees look finished with
//! (merged, upstream deleted, PR merged or closed), and "Update from main"
//! (fetch, then rebase onto or merge the default branch).
//!
//! Everything but the update is read only (`--no-optional-locks`) and runs
//! in the main worktree against the commits `git worktree list` reported,
//! so a worktree whose directory is gone still gets its counts.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use civet_proto::{
    PullRequestInfo, PullRequestState, RepoLifecycle, StaleReason, UpdateOutcome, UpdateStrategy,
    WorktreeInfo, WorktreeLifecycle, WorktreeUpdate,
};

use crate::SessionError;
use crate::git::{Git, parse_status_v2};
use crate::git_actions::network;

const RO: &str = "--no-optional-locks";

/// Worktrees examined at once (each runs a few git commands).
const PARALLEL: usize = 8;

/// The ref `ahead` / `behind` are measured against: `origin/<default>` if
/// that remote-tracking branch exists, else the local `<default>`.
async fn base_ref(git: &Git, main: &Path, default: &str) -> Option<String> {
    let remote = format!("refs/remotes/origin/{default}");
    if git.ref_exists(main, &remote).await.unwrap_or(false) {
        return Some(format!("origin/{default}"));
    }
    git.branch_exists(main, default)
        .await
        .unwrap_or(false)
        .then(|| default.to_owned())
}

/// Branches seen with a live upstream, per repository (main worktree path),
/// for the daemon's lifetime. Why: git alone can't tell an upstream deleted
/// after a merge from one never pushed (`create_worktree` configures it up
/// front), so a gone upstream only counts once it was seen to exist.
#[derive(Default)]
pub(crate) struct PushedBranches(Mutex<HashMap<PathBuf, HashSet<String>>>);

impl PushedBranches {
    /// Record `upstreams`' live ones; returns every branch of `main` seen
    /// pushed so far.
    fn note(&self, main: &Path, upstreams: &HashMap<String, Upstream>) -> HashSet<String> {
        let mut all = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let seen = all.entry(main.to_path_buf()).or_default();
        seen.extend(
            upstreams
                .iter()
                .filter(|(_, u)| u.name.is_some() && !u.gone)
                .map(|(b, _)| b.clone()),
        );
        seen.clone()
    }
}

/// The upstream of a local branch, from `git for-each-ref`.
#[derive(Debug, Default, PartialEq, Eq)]
struct Upstream {
    name: Option<String>,
    ahead: u64,
    behind: u64,
    gone: bool,
}

/// `git for-each-ref` format of [`parse_upstreams`]' input.
const UPSTREAM_FORMAT: &str =
    "--format=%(refname:short)%00%(upstream:short)%00%(upstream:track,nobracket)";

/// Parse `git for-each-ref` [`UPSTREAM_FORMAT`] `refs/heads`: branch →
/// upstream. The track field is empty, `gone`, or
/// `ahead N`, `behind N`, `ahead N, behind M`.
fn parse_upstreams(text: &str) -> HashMap<String, Upstream> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let mut f = line.split('\0');
        let (Some(branch), Some(upstream), track) = (f.next(), f.next(), f.next().unwrap_or(""))
        else {
            continue;
        };
        let mut u = Upstream {
            name: (!upstream.is_empty()).then(|| upstream.to_owned()),
            ..Default::default()
        };
        for part in track.split(',').map(str::trim) {
            match part.split_once(' ') {
                Some(("ahead", n)) => u.ahead = n.parse().unwrap_or(0),
                Some(("behind", n)) => u.behind = n.parse().unwrap_or(0),
                _ if part == "gone" => u.gone = true,
                _ => {}
            }
        }
        out.insert(branch.to_owned(), u);
    }
    out
}

/// What [`stale_reasons`] decides from.
#[derive(Debug, Default)]
struct Facts<'a> {
    /// Main worktree, detached, unborn or on the default branch: never stale.
    exempt: bool,
    ahead: u64,
    merged: bool,
    upstream_gone: bool,
    /// The branch has a PR, or its upstream was seen to exist.
    was_pushed: bool,
    /// The branch's newest PR, if HEAD has nothing the PR did not.
    pull_request: Option<&'a PullRequestInfo>,
}

fn stale_reasons(f: &Facts) -> Vec<StaleReason> {
    if f.exempt {
        return Vec::new();
    }
    let mut out = Vec::new();
    if f.merged {
        out.push(StaleReason::Merged);
    }
    // Why `was_pushed`: see [`PushedBranches`]. Why `ahead`: a branch
    // with no commits of its own lost nothing with its remote.
    if f.upstream_gone && f.was_pushed && f.ahead > 0 {
        out.push(StaleReason::UpstreamGone);
    }
    match f.pull_request.map(|p| p.state) {
        Some(PullRequestState::Merged) => out.push(StaleReason::PullRequestMerged),
        Some(PullRequestState::Closed) => out.push(StaleReason::PullRequestClosed),
        _ => {}
    }
    out
}

/// `git rev-list --left-right --count a...b` → (only in a, only in b).
async fn left_right(git: &Git, dir: &Path, a: &str, b: &str) -> Result<(u64, u64), SessionError> {
    let range = format!("{a}...{b}");
    let out = git
        .run(dir, &[RO, "rev-list", "--left-right", "--count", &range])
        .await?;
    let mut it = out
        .split_whitespace()
        .map(|n| n.parse::<u64>().unwrap_or(0));
    Ok((it.next().unwrap_or(0), it.next().unwrap_or(0)))
}

/// `head` (already in `base`'s history) was merged in, rather than `base`
/// having just moved on from it: walking `base`'s first parents back to
/// `head`'s history does not end at `head` itself.
async fn merged_into(git: &Git, dir: &Path, head: &str, base: &str) -> Result<bool, SessionError> {
    let not_head = format!("^{head}");
    let out = git
        .run(
            dir,
            &[
                RO,
                "rev-list",
                "--first-parent",
                "--parents",
                base,
                &not_head,
            ],
        )
        .await?;
    // Oldest last: "<commit> <first parent> [<other parents>...]".
    Ok(match out.lines().last() {
        None => false, // base is head
        Some(line) => line.split_whitespace().nth(1) != Some(head),
    })
}

/// Paths with uncommitted changes in the worktree at `dir`, untracked files
/// included (an untracked directory counts once).
async fn dirty_count(git: &Git, dir: &Path) -> Result<u64, SessionError> {
    let out = git
        .run(
            dir,
            &[
                RO,
                "status",
                "--porcelain=v2",
                "-z",
                "--untracked-files=normal",
            ],
        )
        .await?;
    let s = parse_status_v2(&out);
    let mut paths: Vec<&str> = [&s.staged, &s.unstaged, &s.untracked, &s.conflicted]
        .into_iter()
        .flatten()
        .map(|c| c.path.as_str())
        .collect();
    paths.sort_unstable();
    paths.dedup();
    Ok(paths.len() as u64)
}

/// Shared by every worktree's check.
struct Context {
    git: Git,
    main: PathBuf,
    default: Option<String>,
    base: Option<String>,
    upstreams: HashMap<String, Upstream>,
    pushed: HashSet<String>,
    /// Branch → its newest PR and that PR's head commit.
    pull_requests: HashMap<String, (PullRequestInfo, Option<String>)>,
    review: bool,
}

async fn examine(cx: &Context, w: &WorktreeInfo) -> Result<WorktreeLifecycle, SessionError> {
    let git = &cx.git;
    let main = cx.main.as_path();
    let up = w.branch.as_ref().and_then(|b| cx.upstreams.get(b));
    let mut out = WorktreeLifecycle {
        worktree: w.id.clone(),
        branch: w.branch.clone(),
        ahead: 0,
        behind: 0,
        upstream: up.and_then(|u| u.name.clone()),
        upstream_ahead: up.map_or(0, |u| u.ahead),
        upstream_behind: up.map_or(0, |u| u.behind),
        upstream_gone: up.is_some_and(|u| u.gone),
        unpushed: 0,
        dirty: None,
        pull_request: None,
        stale: Vec::new(),
    };
    if cx.review && Path::new(&w.path).is_dir() {
        out.dirty = Some(dirty_count(git, Path::new(&w.path)).await?);
    }
    let Some(head) = w.head.as_deref() else {
        return Ok(out);
    };
    let mut merged = false;
    if let Some(base) = &cx.base {
        (out.ahead, out.behind) = left_right(git, main, head, base).await?;
        if out.ahead == 0 {
            merged = merged_into(git, main, head, base).await?;
        }
    }
    // Commits only this branch has: on no remote-tracking branch, not in
    // the base (a repository without remotes has only the base).
    let mut args = vec![RO, "rev-list", "--count", head, "--not", "--remotes"];
    args.extend(cx.base.as_deref());
    out.unpushed = git.run(main, &args).await?.trim().parse().unwrap_or(0);

    let pr = w.branch.as_ref().and_then(|b| cx.pull_requests.get(b));
    // Why: a branch name can be reused after its PR was merged; the old PR
    // only counts while HEAD has nothing it did not.
    let pr_covers_head = match pr {
        Some((_, Some(oid))) if oid == head => true,
        Some((_, Some(oid))) => {
            git.succeeds(main, &[RO, "merge-base", "--is-ancestor", head, oid])
                .await?
        }
        _ => false,
    };
    out.pull_request = pr.map(|(p, _)| p.clone());
    let on_default = w.branch.is_some() && w.branch == cx.default;
    out.stale = stale_reasons(&Facts {
        exempt: w.is_main || w.branch.is_none() || on_default,
        ahead: out.ahead,
        merged,
        upstream_gone: out.upstream_gone,
        was_pushed: pr.is_some() || w.branch.as_ref().is_some_and(|b| cx.pushed.contains(b)),
        pull_request: pr.filter(|_| pr_covers_head).map(|(p, _)| p),
    });
    Ok(out)
}

/// Lifecycle of every worktree in `worktrees` (all of one repository whose
/// main worktree is `main`). With `fetch`, `git fetch --prune` first (a
/// failure is reported, not fatal); with `review`, also uncommitted changes
/// and pull requests.
pub(crate) async fn repo_lifecycle(
    git: &Git,
    main: &Path,
    worktrees: &[WorktreeInfo],
    fetch: bool,
    review: bool,
    pushed: &PushedBranches,
) -> Result<RepoLifecycle, SessionError> {
    let mut fetch_error = None;
    if fetch && !git.run(main, &["remote"]).await?.trim().is_empty() {
        // Before the fetch prunes them: an upstream deleted since the last
        // look was still live then.
        let before = git
            .run(main, &[RO, "for-each-ref", UPSTREAM_FORMAT, "refs/heads"])
            .await?;
        pushed.note(main, &parse_upstreams(&before));
        if let Err(e) = network(git, main, &["fetch", "--prune", "--quiet", "--all"]).await {
            fetch_error = Some(e.to_string());
        }
    }
    let default = git.default_branch(main).await;
    let base = match &default {
        Some(d) => base_ref(git, main, d).await,
        None => None,
    };
    let refs = git
        .run(main, &[RO, "for-each-ref", UPSTREAM_FORMAT, "refs/heads"])
        .await?;
    let upstreams = parse_upstreams(&refs);
    let mut pull_requests = HashMap::new();
    let mut pull_requests_error = None;
    if review {
        match crate::github::list_pull_requests_with_heads(main).await {
            Ok(list) => {
                for (pr, oid) in list {
                    // Newest first: keep the first of each branch.
                    pull_requests.entry(pr.branch.clone()).or_insert((pr, oid));
                }
            }
            Err(e) => pull_requests_error = Some(e.to_string()),
        }
    }
    let cx = Arc::new(Context {
        git: git.clone(),
        main: main.to_path_buf(),
        default,
        base: base.clone(),
        pushed: pushed.note(main, &upstreams),
        upstreams,
        pull_requests,
        review,
    });
    let limit = Arc::new(tokio::sync::Semaphore::new(PARALLEL));
    let mut tasks = tokio::task::JoinSet::new();
    for (i, w) in worktrees.iter().cloned().enumerate() {
        let (cx, limit) = (cx.clone(), limit.clone());
        tasks.spawn(async move {
            let _permit = limit.acquire_owned().await;
            (i, examine(&cx, &w).await)
        });
    }
    let mut results: Vec<Option<WorktreeLifecycle>> = vec![None; worktrees.len()];
    while let Some(joined) = tasks.join_next().await {
        let (i, r) = joined.map_err(|e| SessionError::Git(format!("lifecycle task: {e}")))?;
        results[i] = Some(r?);
    }
    Ok(RepoLifecycle {
        base,
        worktrees: results.into_iter().flatten().collect(),
        fetch_error,
        pull_requests_error,
    })
}

// ----- update from the default branch ----------------------------------------------

/// `git rev-parse --git-path <name>` exists in the worktree at `dir`.
async fn git_path_exists(git: &Git, dir: &Path, name: &str) -> Result<bool, SessionError> {
    let p = git.run(dir, &["rev-parse", "--git-path", name]).await?;
    Ok(dir.join(p.trim()).exists())
}

/// Unmerged paths, sorted.
async fn conflicted_paths(git: &Git, dir: &Path) -> Result<Vec<String>, SessionError> {
    let out = git
        .run(dir, &["diff", "--name-only", "-z", "--diff-filter=U"])
        .await?;
    let mut paths: Vec<String> = out
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Fetch the default branch and rebase the branch checked out at `dir` onto
/// it, or merge it in (see [`civet_proto::Request::UpdateWorktree`]). `main`
/// is the repository's main worktree.
pub(crate) async fn update(
    git: &Git,
    main: &Path,
    dir: &Path,
    strategy: UpdateStrategy,
) -> Result<WorktreeUpdate, SessionError> {
    let refuse = |m: String| Err(SessionError::InvalidArgument(m));
    let branch = git
        .output(dir, &["symbolic-ref", "-q", "--short", "HEAD"])
        .await?;
    let branch = branch.stdout.trim();
    if branch.is_empty() {
        return refuse("HEAD is detached; check out a branch to update it".into());
    }
    let Some(default) = git.default_branch(main).await else {
        return refuse("this repository has no default branch to update from".into());
    };
    if branch == default {
        return refuse(format!("{branch} is the default branch; use Pull instead"));
    }
    for (name, what) in [
        ("MERGE_HEAD", "a merge"),
        ("rebase-merge", "a rebase"),
        ("rebase-apply", "a rebase"),
    ] {
        if git_path_exists(git, dir, name).await? {
            return refuse(format!(
                "{what} is already in progress here; finish or abort it first"
            ));
        }
    }
    let changed = git
        .run(dir, &["status", "--porcelain", "--untracked-files=no"])
        .await?;
    if !changed.trim().is_empty() {
        return refuse("there are uncommitted changes; commit or stash them first".into());
    }
    let remote = format!("refs/remotes/origin/{default}");
    if git.ref_exists(main, &remote).await? {
        // Fetches only the default branch; git updates origin/<default>.
        network(git, dir, &["fetch", "--quiet", "origin", &default])
            .await
            .map_err(|e| SessionError::Git(format!("could not fetch origin/{default}: {e}")))?;
    }
    let Some(base) = base_ref(git, main, &default).await else {
        return refuse(format!("{default} does not exist here"));
    };
    let done = |outcome, conflicts| {
        Ok(WorktreeUpdate {
            base: base.clone(),
            outcome,
            conflicts,
        })
    };
    if git
        .succeeds(dir, &["merge-base", "--is-ancestor", &base, "HEAD"])
        .await?
    {
        return done(UpdateOutcome::UpToDate, Vec::new());
    }
    // core.editor=true: never wait for an editor (merge message, rebase
    // todo); the daemon has no terminal.
    let args: &[&str] = match strategy {
        UpdateStrategy::Rebase => &["-c", "core.editor=true", "rebase", "--no-autostash", &base],
        UpdateStrategy::Merge => &["-c", "core.editor=true", "merge", "--no-edit", &base],
    };
    let out = git.output(dir, args).await?;
    if out.success {
        return done(UpdateOutcome::Updated, Vec::new());
    }
    let conflicts = conflicted_paths(git, dir).await?;
    match strategy {
        UpdateStrategy::Rebase => {
            // Why abort: a rebase stopped half way leaves the branch detached
            // under whatever agent works here; nothing changed is the safer
            // state, and the conflicts are reported so a merge can be chosen.
            let rebasing = git_path_exists(git, dir, "rebase-merge").await?
                || git_path_exists(git, dir, "rebase-apply").await?;
            if rebasing {
                git.run(dir, &["rebase", "--abort"]).await?;
            }
            if !conflicts.is_empty() {
                return done(UpdateOutcome::RebaseConflicts, conflicts);
            }
        }
        // The merge's conflicts stay for the Changes tab to resolve.
        UpdateStrategy::Merge if !conflicts.is_empty() => {
            return done(UpdateOutcome::MergeConflicts, conflicts);
        }
        UpdateStrategy::Merge => {}
    }
    Err(SessionError::Git(
        [out.stdout.trim(), out.stderr.trim()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_upstreams() {
        let text = "main\0origin/main\0\nfeat/a\0origin/feat/a\0ahead 2, behind 1\nb\0origin/b\0gone\nlocal\0\0\nc\0origin/c\0behind 4\n";
        let m = parse_upstreams(text);
        assert_eq!(m.len(), 5);
        assert_eq!(m["main"].name.as_deref(), Some("origin/main"));
        assert_eq!((m["feat/a"].ahead, m["feat/a"].behind), (2, 1));
        assert!(m["b"].gone && m["b"].name.is_some());
        assert_eq!(m["local"], Upstream::default());
        assert_eq!((m["c"].ahead, m["c"].behind), (0, 4));
    }

    #[test]
    fn stale_reason_rules() {
        let pr = |state| PullRequestInfo {
            number: 1,
            branch: "b".into(),
            title: "t".into(),
            url: "u".into(),
            state,
        };
        use StaleReason::*;
        assert_eq!(stale_reasons(&Facts::default()), vec![]);
        let merged = Facts {
            merged: true,
            ..Default::default()
        };
        assert_eq!(stale_reasons(&merged), vec![Merged]);
        // Never pushed and no commits yet: a fresh worktree.
        let fresh = Facts {
            upstream_gone: true,
            ..Default::default()
        };
        assert_eq!(stale_reasons(&fresh), vec![]);
        // Commits, never pushed: the upstream civet configured is missing.
        let unpushed = Facts {
            upstream_gone: true,
            ahead: 3,
            ..Default::default()
        };
        assert_eq!(stale_reasons(&unpushed), vec![]);
        let gone = Facts {
            was_pushed: true,
            ..unpushed
        };
        assert_eq!(stale_reasons(&gone), vec![UpstreamGone]);
        let p = pr(PullRequestState::Merged);
        let squashed = Facts {
            upstream_gone: true,
            was_pushed: true,
            ahead: 3,
            pull_request: Some(&p),
            ..Default::default()
        };
        assert_eq!(
            stale_reasons(&squashed),
            vec![UpstreamGone, PullRequestMerged]
        );
        let p = pr(PullRequestState::Closed);
        let closed = Facts {
            pull_request: Some(&p),
            ..Default::default()
        };
        assert_eq!(stale_reasons(&closed), vec![PullRequestClosed]);
        for state in [PullRequestState::Open, PullRequestState::Draft] {
            let p = pr(state);
            let open = Facts {
                pull_request: Some(&p),
                ..Default::default()
            };
            assert_eq!(stale_reasons(&open), vec![]);
        }
        let exempt = Facts {
            exempt: true,
            merged: true,
            ..squashed
        };
        assert_eq!(stale_reasons(&exempt), vec![]);
    }
}
