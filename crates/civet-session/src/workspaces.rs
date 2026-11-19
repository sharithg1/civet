//! Floating workspaces ([`civet_proto::WorkspaceInfo`]): places for ad-hoc
//! tasks that belong to the host, not to a repository.
//!
//! A workspace is a directory the daemon owns, `<state>/workspaces/<slug>`,
//! holding one symlink per root (a repository, worktree, folder or file
//! anywhere on the host) next to whatever the task leaves there. Panes run
//! in that directory, so `cd <root name>` works in a shell, and get the
//! roots in [`civet_proto::ENV_WORKSPACE_ROOTS`], which the agent shims turn
//! into `--add-dir` (see [`crate::hooks`]).
//!
//! Workspaces persist in `workspaces.json`, apart from `repos.json`: a
//! daemon that predates them then still reads its repositories after a
//! downgrade.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use civet_proto::{MAX_WORKSPACE_ROOTS, RepoId, WorkspaceInfo, WorkspaceRoot};
use serde::{Deserialize, Serialize};

use crate::SessionError;
use crate::git::{canonical_or_same, path_string, repo_id_for};

pub(crate) const WORKSPACES_FILE: &str = "workspaces.json";
/// Directory under the state dir holding one directory per workspace.
pub(crate) const WORKSPACES_DIR: &str = "workspaces";
/// Longest workspace name, in characters.
const MAX_NAME_CHARS: usize = 64;

/// One root as persisted: where it is and what its link is called.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredRoot {
    /// Canonical absolute path.
    pub path: String,
    /// Link name in the workspace directory.
    pub name: String,
}

/// A workspace as persisted in `workspaces.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredWorkspace {
    pub id: String,
    pub name: String,
    /// Canonical path of its directory.
    pub path: String,
    #[serde(default)]
    pub roots: Vec<StoredRoot>,
}

impl StoredWorkspace {
    pub(crate) fn info(&self) -> WorkspaceInfo {
        WorkspaceInfo {
            id: RepoId(self.id.clone()),
            name: self.name.clone(),
            path: self.path.clone(),
            roots: self
                .roots
                .iter()
                .map(|r| {
                    let meta = std::fs::metadata(&r.path);
                    WorkspaceRoot {
                        path: r.path.clone(),
                        name: r.name.clone(),
                        is_dir: meta.as_ref().is_ok_and(|m| m.is_dir()),
                        missing: meta.is_err(),
                    }
                })
                .collect(),
        }
    }

    /// Value of [`civet_proto::ENV_WORKSPACE_ROOTS`]: the roots that still
    /// exist, one per line.
    pub(crate) fn roots_env(&self) -> String {
        self.roots
            .iter()
            .filter(|r| Path::new(&r.path).exists())
            .map(|r| r.path.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The roots as (link name, path), for [`crate::files::Scope`].
    pub(crate) fn links(&self) -> Vec<(String, PathBuf)> {
        self.roots
            .iter()
            .map(|r| (r.name.clone(), PathBuf::from(&r.path)))
            .collect()
    }
}

/// A display name: trimmed, not empty, no control characters, at most
/// [`MAX_NAME_CHARS`].
pub(crate) fn check_name(name: &str) -> Result<String, SessionError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(SessionError::InvalidArgument(
            "a workspace needs a name".into(),
        ));
    }
    if name.chars().count() > MAX_NAME_CHARS || name.chars().any(char::is_control) {
        return Err(SessionError::InvalidArgument(format!(
            "workspace name {name:?} is too long (over {MAX_NAME_CHARS} characters) or has control characters"
        )));
    }
    Ok(name.to_owned())
}

/// Directory name for a workspace name: lowercase letters and digits, runs
/// of anything else as one `-`.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() {
        "workspace".to_owned()
    } else {
        out.to_owned()
    }
}

/// `base`, or `base-2`, `base-3`... : the first that `taken` does not hold.
fn numbered(base: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_owned();
    }
    (2u32..)
        .map(|n| format!("{base}-{n}"))
        .find(|c| !taken(c))
        .unwrap_or_else(|| base.to_owned())
}

/// Create the directory of a new workspace called `name` under
/// `<state_dir>/workspaces` (a name whose directory is taken, e.g. left
/// behind by a removed workspace, gets a numbered one) and return it with
/// no roots yet.
pub(crate) fn create(state_dir: &Path, name: &str) -> Result<StoredWorkspace, SessionError> {
    let parent = state_dir.join(WORKSPACES_DIR);
    std::fs::create_dir_all(&parent)?;
    let dir_name = numbered(&slug(name), |c| {
        std::fs::symlink_metadata(parent.join(c)).is_ok()
    });
    let dir = parent.join(dir_name);
    std::fs::create_dir(&dir)?;
    let dir = canonical_or_same(&dir);
    Ok(StoredWorkspace {
        id: repo_id_for(&dir),
        name: name.to_owned(),
        path: path_string(&dir),
        roots: Vec::new(),
    })
}

/// The canonical paths of `roots` (each absolute or `~/...`, existing, and
/// not inside the workspace directory `dir`), without repeats.
pub(crate) fn resolve_roots(roots: &[String], dir: &Path) -> Result<Vec<PathBuf>, SessionError> {
    let invalid = |m: String| Err(SessionError::InvalidArgument(m));
    let mut out: Vec<PathBuf> = Vec::new();
    for raw in roots {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        if raw.chars().any(char::is_control) {
            return invalid(format!("root {raw:?} has control characters"));
        }
        let path = crate::manager::expand_home(raw);
        if !path.is_absolute() {
            return invalid(format!("root {raw:?} must be an absolute path or ~/..."));
        }
        let Ok(path) = std::fs::canonicalize(&path) else {
            return invalid(format!("{} does not exist", path.display()));
        };
        if path.starts_with(dir) {
            return invalid(format!("{} is inside the workspace itself", path.display()));
        }
        if !out.contains(&path) {
            out.push(path);
        }
    }
    if out.len() > MAX_WORKSPACE_ROOTS {
        return invalid(format!(
            "a workspace has at most {MAX_WORKSPACE_ROOTS} roots ({} given)",
            out.len()
        ));
    }
    Ok(out)
}

/// A link name for `path`: its last component (`git` for `.git`, which
/// file requests refuse).
fn base_name(path: &Path) -> String {
    match path.file_name().map(|n| n.to_string_lossy().into_owned()) {
        Some(n) if n == ".git" => "git".to_owned(),
        Some(n) => n,
        None => "root".to_owned(),
    }
}

fn remove_link(dir: &Path, name: &str) {
    let link = dir.join(name);
    // Only a link: something else the task put there under that name stays.
    if std::fs::symlink_metadata(&link).is_ok_and(|m| m.is_symlink()) {
        if let Err(e) = std::fs::remove_file(&link) {
            tracing::warn!("removing {}: {e}", link.display());
        }
    }
}

/// Make `paths` (from [`resolve_roots`]) the workspace's roots: links of
/// dropped roots are removed, kept roots keep their link name, new ones get
/// a name nothing in the directory has.
pub(crate) fn set_roots(ws: &mut StoredWorkspace, paths: Vec<PathBuf>) -> Result<(), SessionError> {
    let dir = PathBuf::from(&ws.path);
    let wanted: Vec<String> = paths.iter().map(|p| path_string(p)).collect();
    for old in ws.roots.iter().filter(|r| !wanted.contains(&r.path)) {
        remove_link(&dir, &old.name);
    }
    let mut used: HashSet<String> = ws
        .roots
        .iter()
        .filter(|r| wanted.contains(&r.path))
        .map(|r| r.name.clone())
        .collect();
    let mut roots = Vec::with_capacity(paths.len());
    for (path, stored) in paths.iter().zip(wanted) {
        if let Some(kept) = ws.roots.iter().find(|r| r.path == stored) {
            roots.push(kept.clone());
            continue;
        }
        let name = numbered(&base_name(path), |c| {
            used.contains(c) || std::fs::symlink_metadata(dir.join(c)).is_ok()
        });
        std::os::unix::fs::symlink(path, dir.join(&name))?;
        used.insert(name.clone());
        roots.push(StoredRoot { path: stored, name });
    }
    ws.roots = roots;
    ensure_links(ws);
    Ok(())
}

/// Recreate the directory and any root link that went missing (best
/// effort: a name now taken by something else is left alone).
pub(crate) fn ensure_links(ws: &StoredWorkspace) {
    let dir = Path::new(&ws.path);
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::warn!("workspace directory {}: {e}", dir.display());
        return;
    }
    for r in &ws.roots {
        let link = dir.join(&r.name);
        if std::fs::symlink_metadata(&link).is_err() {
            if let Err(e) = std::os::unix::fs::symlink(&r.path, &link) {
                tracing::warn!("linking {}: {e}", link.display());
            }
        }
    }
}

/// Remove a workspace's links, and its directory if nothing else is left
/// in it. The roots themselves are never touched.
pub(crate) fn remove(ws: &StoredWorkspace) {
    let dir = Path::new(&ws.path);
    for r in &ws.roots {
        remove_link(dir, &r.name);
    }
    // Fails if the task left files there: they stay.
    if let Err(e) = std::fs::remove_dir(dir) {
        tracing::info!("workspace directory {} kept: {e}", dir.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_and_numbering() {
        assert_eq!(slug("Release Notes!"), "release-notes");
        assert_eq!(slug("  --a__b  "), "a-b");
        assert_eq!(slug("***"), "workspace");
        assert_eq!(numbered("a", |_| false), "a");
        assert_eq!(numbered("a", |c| c == "a" || c == "a-2"), "a-3");
        assert!(check_name("  ").is_err());
        assert!(check_name("a\nb").is_err());
        assert_eq!(check_name(" x ").unwrap(), "x");
    }

    #[test]
    fn roots_are_linked_renumbered_and_unlinked() {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let (a, b) = (base.join("a/app"), base.join("b/app"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let file = base.join("todo.md");
        std::fs::write(&file, "x").unwrap();
        let s = |p: &Path| p.display().to_string();

        let mut ws = create(&base.join("state"), "My Task").unwrap();
        assert!(ws.path.ends_with("workspaces/my-task"));
        let dir = PathBuf::from(&ws.path);
        // The same root twice counts once; a missing one is refused.
        let paths = resolve_roots(&[s(&a), s(&b), s(&file), s(&a)], &dir).unwrap();
        assert_eq!(paths.len(), 3);
        assert!(resolve_roots(&[s(&base.join("nope"))], &dir).is_err());
        assert!(resolve_roots(&["rel/path".into()], &dir).is_err());
        assert!(resolve_roots(&[s(&dir)], &dir).is_err());

        set_roots(&mut ws, paths).unwrap();
        let names: Vec<_> = ws.roots.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["app", "app-2", "todo.md"]);
        assert_eq!(std::fs::read_link(dir.join("app-2")).unwrap(), b);
        let info = ws.info();
        assert!(info.roots[0].is_dir && !info.roots[2].is_dir && !info.roots[2].missing);
        assert_eq!(
            ws.roots_env(),
            format!("{}\n{}\n{}", s(&a), s(&b), s(&file))
        );

        // Dropping the first keeps the second's name; a file of the task's
        // own under a wanted name is not replaced.
        std::fs::write(dir.join("notes"), "mine").unwrap();
        let notes = base.join("notes");
        std::fs::create_dir(&notes).unwrap();
        let paths = resolve_roots(&[s(&b), s(&notes)], &dir).unwrap();
        set_roots(&mut ws, paths).unwrap();
        let names: Vec<_> = ws.roots.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["app-2", "notes-2"]);
        assert!(std::fs::symlink_metadata(dir.join("app")).is_err());
        assert!(std::fs::symlink_metadata(dir.join("todo.md")).is_err());

        // A deleted link comes back; a root that is gone is reported.
        std::fs::remove_file(dir.join("app-2")).unwrap();
        ensure_links(&ws);
        assert!(dir.join("app-2").is_dir());
        std::fs::remove_dir(&notes).unwrap();
        assert!(ws.info().roots[1].missing);
        assert_eq!(ws.roots_env(), s(&b));

        // Removal leaves the directory while the task's file is in it, and
        // a new workspace of that name gets its own directory.
        remove(&ws);
        assert!(dir.join("notes").is_file());
        assert!(std::fs::symlink_metadata(dir.join("app-2")).is_err());
        assert!(b.is_dir());
        let again = create(&base.join("state"), "My Task").unwrap();
        assert!(again.path.ends_with("workspaces/my-task-2"));
        assert_ne!(again.id, ws.id);
        remove(&again);
        assert!(!Path::new(&again.path).exists());
    }
}
