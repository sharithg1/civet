//! Notices worktree changes made outside Civet (`git worktree add/remove`
//! in a terminal, `rm -rf` of a worktree, `git checkout` in one) so clients
//! re-list without waiting for a reconnect.
//!
//! Why polling of file metadata rather than `git worktree list`: it costs a
//! few `stat`s per repo and spawns no process, so it can run every couple of
//! seconds on a host with many repos; git itself only runs when a client
//! re-lists after [`civet_proto::Event::WorktreesChanged`].

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The git directory shared by all worktrees of the repo whose main worktree
/// (or bare repository) is `main`: `<main>/.git`, the target of a `.git`
/// file (`gitdir: ...`), or `main` itself for a bare repo. None if `main`
/// does not look like a repository (any more).
pub(crate) fn common_dir(main: &Path) -> Option<PathBuf> {
    let dotgit = main.join(".git");
    if dotgit.is_dir() {
        return Some(dotgit);
    }
    if dotgit.is_file() {
        let text = std::fs::read_to_string(&dotgit).ok()?;
        let target = text.trim().strip_prefix("gitdir:")?.trim();
        let p = main.join(target);
        // A linked worktree's gitdir is `<common>/worktrees/<name>`; the
        // main worktree's (`--separate-git-dir`) is the common dir itself.
        let common = std::fs::read_to_string(p.join("commondir"))
            .ok()
            .map(|c| p.join(c.trim()))
            .unwrap_or(p);
        return Some(common);
    }
    main.join("HEAD").is_file().then(|| main.to_path_buf())
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// A value that changes whenever `git worktree list` could: a worktree is
/// registered or unregistered, its directory appears or disappears, it is
/// locked or unlocked, or a checkout moves its HEAD (branch switch).
pub(crate) fn fingerprint(main: &Path) -> u64 {
    let mut h = DefaultHasher::new();
    let Some(common) = common_dir(main) else {
        return 0;
    };
    mtime(&common.join("HEAD")).hash(&mut h);
    if let Ok(entries) = std::fs::read_dir(common.join("worktrees")) {
        let mut names: Vec<_> = entries.flatten().map(|e| e.file_name()).collect();
        names.sort();
        for name in names {
            let dir = common.join("worktrees").join(&name);
            name.hash(&mut h);
            mtime(&dir.join("HEAD")).hash(&mut h);
            dir.join("locked").exists().hash(&mut h);
            // `gitdir` holds `<worktree>/.git`: its existence is what git
            // reports as "prunable".
            let gitdir = std::fs::read_to_string(dir.join("gitdir")).unwrap_or_default();
            gitdir.hash(&mut h);
            Path::new(gitdir.trim()).exists().hash(&mut h);
        }
    }
    h.finish()
}

/// Last fingerprint per repo id; reports the repos whose fingerprint changed
/// since the previous call (a repo seen for the first time is not reported).
#[derive(Debug, Default)]
pub(crate) struct WorktreeWatch {
    last: BTreeMap<String, u64>,
}

impl WorktreeWatch {
    /// `repos`: (repo id, main worktree path).
    pub(crate) fn changed(&mut self, repos: &[(String, PathBuf)]) -> Vec<String> {
        let mut out = Vec::new();
        let mut next = BTreeMap::new();
        for (id, main) in repos {
            let fp = fingerprint(main);
            if self.last.get(id).is_some_and(|&old| old != fp) {
                out.push(id.clone());
            }
            next.insert(id.clone(), fp);
        }
        self.last = next;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn notices_changes_made_with_the_git_cli() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("r");
        std::fs::create_dir(&main).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "i"]);
        assert_eq!(common_dir(&main), Some(main.join(".git")));

        let repos = vec![("r".to_owned(), main.clone())];
        let mut w = WorktreeWatch::default();
        assert!(w.changed(&repos).is_empty(), "first look reports nothing");
        assert!(w.changed(&repos).is_empty());

        let wt = tmp.path().join("wt");
        git(
            &main,
            &["worktree", "add", "-q", "-b", "a", &wt.to_string_lossy()],
        );
        assert_eq!(w.changed(&repos), vec!["r".to_owned()]);
        assert!(w.changed(&repos).is_empty());
        // A linked worktree resolves to the same common dir.
        assert_eq!(
            common_dir(&wt).map(|p| std::fs::canonicalize(p).unwrap()),
            Some(std::fs::canonicalize(main.join(".git")).unwrap())
        );

        git(&main, &["worktree", "lock", &wt.to_string_lossy()]);
        assert_eq!(w.changed(&repos).len(), 1);
        git(&main, &["worktree", "unlock", &wt.to_string_lossy()]);
        assert_eq!(w.changed(&repos).len(), 1);

        // `rm -rf` without git: now prunable.
        std::fs::remove_dir_all(&wt).unwrap();
        assert_eq!(w.changed(&repos).len(), 1);
        git(&main, &["worktree", "prune"]);
        assert_eq!(w.changed(&repos).len(), 1);
        assert!(w.changed(&repos).is_empty());

        // Not a repository: stable, no panic.
        let none = vec![("x".to_owned(), tmp.path().join("missing"))];
        assert!(w.changed(&none).is_empty());
        assert!(w.changed(&none).is_empty());
    }
}
