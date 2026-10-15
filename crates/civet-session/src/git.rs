//! Thin async wrapper around the `git` CLI plus parsers for its output.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use sha2::{Digest, Sha256};

use crate::SessionError;

/// Runs git commands with a fixed binary and a predictable environment.
#[derive(Debug, Clone)]
pub(crate) struct Git {
    bin: PathBuf,
}

/// Captured output of a git command that may fail.
#[derive(Debug)]
pub(crate) struct GitOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Git {
    pub(crate) fn new(bin: PathBuf) -> Self {
        Self { bin }
    }

    /// Run `git -C dir args...`, returning output regardless of exit status.
    pub(crate) async fn output(
        &self,
        dir: &Path,
        args: &[&str],
    ) -> Result<GitOutput, SessionError> {
        let mut cmd = tokio::process::Command::new(&self.bin);
        cmd.arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .kill_on_drop(true);
        let out = cmd
            .output()
            .await
            .map_err(|e| SessionError::Git(format!("failed to run {}: {e}", self.bin.display())))?;
        Ok(GitOutput {
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        })
    }

    /// Run and require success; returns stdout. The error carries git's
    /// trimmed stderr (or the exit status if stderr is empty).
    pub(crate) async fn run(&self, dir: &Path, args: &[&str]) -> Result<String, SessionError> {
        let out = self.output(dir, args).await?;
        if out.success {
            Ok(out.stdout)
        } else if out.stderr.is_empty() {
            Err(SessionError::Git(format!("git {} failed", args.join(" "))))
        } else {
            Err(SessionError::Git(out.stderr))
        }
    }

    /// True if `git -C dir args` exits 0.
    pub(crate) async fn succeeds(&self, dir: &Path, args: &[&str]) -> Result<bool, SessionError> {
        Ok(self.output(dir, args).await?.success)
    }

    /// True if `refs/heads/<branch>` exists.
    pub(crate) async fn branch_exists(
        &self,
        dir: &Path,
        branch: &str,
    ) -> Result<bool, SessionError> {
        let r = format!("refs/heads/{branch}");
        self.succeeds(dir, &["show-ref", "--verify", "--quiet", &r])
            .await
    }

    /// True if `full_ref` (e.g. `refs/remotes/origin/main`) exists.
    pub(crate) async fn ref_exists(
        &self,
        dir: &Path,
        full_ref: &str,
    ) -> Result<bool, SessionError> {
        self.succeeds(dir, &["show-ref", "--verify", "--quiet", full_ref])
            .await
    }

    /// Validate a branch name with `git check-ref-format --branch`.
    pub(crate) async fn check_branch_name(
        &self,
        dir: &Path,
        branch: &str,
    ) -> Result<(), SessionError> {
        if branch.is_empty() || branch.starts_with('-') {
            return Err(SessionError::InvalidArgument(format!(
                "invalid branch name {branch:?}"
            )));
        }
        if self
            .succeeds(dir, &["check-ref-format", "--branch", branch])
            .await?
        {
            Ok(())
        } else {
            Err(SessionError::InvalidArgument(format!(
                "invalid branch name {branch:?}"
            )))
        }
    }

    /// The repository's default branch: `origin/HEAD`'s target, else a local
    /// `main` or `master`, else the branch checked out in `dir`.
    pub(crate) async fn default_branch(&self, dir: &Path) -> Option<String> {
        if let Ok(out) = self
            .run(
                dir,
                &[
                    "symbolic-ref",
                    "--quiet",
                    "--short",
                    "refs/remotes/origin/HEAD",
                ],
            )
            .await
        {
            let name = out.trim();
            if let Some(b) = name.strip_prefix("origin/").filter(|b| !b.is_empty()) {
                return Some(b.to_owned());
            }
        }
        for b in ["main", "master"] {
            if self.branch_exists(dir, b).await.unwrap_or(false) {
                return Some(b.to_owned());
            }
        }
        self.run(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .await
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }
}

/// One entry of `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PorcelainWorktree {
    pub path: String,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
    pub locked: bool,
    pub prunable: bool,
}

/// Parse `git worktree list --porcelain` output. The first entry is the main
/// worktree (or the bare repository directory).
pub(crate) fn parse_worktree_porcelain(text: &str) -> Vec<PorcelainWorktree> {
    let mut out = Vec::new();
    let mut cur: Option<PorcelainWorktree> = None;
    for line in text.lines() {
        if line.is_empty() {
            if let Some(w) = cur.take() {
                out.push(w);
            }
            continue;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        if key == "worktree" {
            if let Some(w) = cur.take() {
                out.push(w);
            }
            cur = Some(PorcelainWorktree {
                path: value.to_owned(),
                ..Default::default()
            });
            continue;
        }
        let Some(w) = cur.as_mut() else { continue };
        match key {
            "HEAD" => {
                let unborn = value.is_empty() || value.bytes().all(|b| b == b'0');
                w.head = (!unborn).then(|| value.to_owned());
            }
            "branch" => {
                w.branch = Some(
                    value
                        .strip_prefix("refs/heads/")
                        .unwrap_or(value)
                        .to_owned(),
                );
            }
            "bare" => w.bare = true,
            "detached" => w.detached = true,
            "locked" => w.locked = true,
            "prunable" => w.prunable = true,
            _ => {}
        }
    }
    if let Some(w) = cur.take() {
        out.push(w);
    }
    out
}

/// Canonicalize if the path exists, else return it unchanged.
pub(crate) fn canonical_or_same(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Lossy path → string.
pub(crate) fn path_string(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// RepoId: hex of the first 8 bytes of SHA-256(canonical common git dir).
pub(crate) fn repo_id_for(common_dir: &Path) -> String {
    let digest = Sha256::digest(path_string(common_dir).as_bytes());
    hex::encode(&digest[..8])
}

/// Directory name for a branch in the default worktree layout.
pub(crate) fn branch_dir_name(branch: &str) -> String {
    branch.replace('/', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain() {
        let text = "worktree /r/main\nHEAD 1111111111111111111111111111111111111111\nbranch refs/heads/main\n\nworktree /r/wt\nHEAD 2222222222222222222222222222222222222222\ndetached\nlocked reason here\n\nworktree /r/new\nHEAD 0000000000000000000000000000000000000000\nbranch refs/heads/feat/x\nprunable gitdir file points to non-existent location\n\n";
        let w = parse_worktree_porcelain(text);
        assert_eq!(w.len(), 3);
        assert_eq!(w[0].path, "/r/main");
        assert_eq!(w[0].branch.as_deref(), Some("main"));
        assert!(w[0].head.is_some());
        assert!(w[1].detached && w[1].locked && w[1].branch.is_none());
        assert_eq!(w[2].branch.as_deref(), Some("feat/x"));
        assert_eq!(w[2].head, None);
        assert!(w[2].prunable);

        let bare = parse_worktree_porcelain("worktree /r/x.git\nbare\n");
        assert_eq!(bare.len(), 1);
        assert!(bare[0].bare);
    }

    #[test]
    fn ids_and_names() {
        let id = repo_id_for(Path::new("/tmp/x/.git"));
        assert_eq!(id.len(), 16);
        assert_eq!(id, repo_id_for(Path::new("/tmp/x/.git")));
        assert_ne!(id, repo_id_for(Path::new("/tmp/y/.git")));
        assert_eq!(branch_dir_name("feat/a/b"), "feat-a-b");
    }
}
