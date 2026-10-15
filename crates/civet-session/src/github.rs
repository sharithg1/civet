//! Pull requests via the GitHub CLI (`gh`), run on the host so it uses the
//! host's own `gh auth`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use civet_proto::{PullRequestInfo, PullRequestState};
use serde::Deserialize;

use crate::SessionError;

/// Most PRs asked for; `gh` lists newest first, so worktrees on branches
/// whose PR is older than this simply show none.
const LIST_LIMIT: &str = "100";
const TIMEOUT: Duration = Duration::from_secs(20);

/// Directories checked for `gh` after `$PATH`. Why: a daemon started over
/// SSH gets a non-login PATH that often lacks Homebrew / ~/.local.
const FALLBACK_DIRS: &[&str] = &[
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/home/linuxbrew/.linuxbrew/bin",
    "/usr/bin",
    "/snap/bin",
];

fn find_gh() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let home = dirs::home_dir();
    std::env::split_paths(&path)
        .chain(FALLBACK_DIRS.iter().map(PathBuf::from))
        .chain(
            home.iter()
                .flat_map(|h| [h.join(".local/bin"), h.join("bin")]),
        )
        .map(|d| d.join("gh"))
        .find(|p| p.is_file())
}

/// `gh pr list --state all` in `dir`: the repository's newest PRs, fork
/// PRs left out. Spawn if `gh` is missing; Git with gh's stderr if it fails
/// (not authenticated, no GitHub remote...).
pub(crate) async fn list_pull_requests(dir: &Path) -> Result<Vec<PullRequestInfo>, SessionError> {
    let gh = find_gh().ok_or_else(|| SessionError::Spawn("gh not found".into()))?;
    let mut cmd = tokio::process::Command::new(&gh);
    cmd.args([
        "pr",
        "list",
        "--state",
        "all",
        "--limit",
        LIST_LIMIT,
        "--json",
        "number,title,url,state,isDraft,headRefName,isCrossRepository",
    ])
    .current_dir(dir)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .env("GH_PROMPT_DISABLED", "1")
    .env("GH_NO_UPDATE_NOTIFIER", "1")
    .env("NO_COLOR", "1")
    .env("GIT_TERMINAL_PROMPT", "0")
    .kill_on_drop(true);
    let out = tokio::time::timeout(TIMEOUT, cmd.output())
        .await
        .map_err(|_| SessionError::Git("gh pr list timed out".into()))?
        .map_err(|e| SessionError::Spawn(format!("failed to run {}: {e}", gh.display())))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        return Err(SessionError::Git(if err.is_empty() {
            "gh pr list failed".into()
        } else {
            err
        }));
    }
    parse_pr_list(&out.stdout)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPr {
    number: u64,
    title: String,
    url: String,
    state: String,
    #[serde(default)]
    is_draft: bool,
    head_ref_name: String,
    #[serde(default)]
    is_cross_repository: bool,
}

fn parse_pr_list(json: &[u8]) -> Result<Vec<PullRequestInfo>, SessionError> {
    let prs: Vec<GhPr> = serde_json::from_slice(json)
        .map_err(|e| SessionError::Git(format!("unexpected gh output: {e}")))?;
    Ok(prs
        .into_iter()
        .filter(|p| !p.is_cross_repository)
        .map(|p| PullRequestInfo {
            state: match p.state.as_str() {
                "MERGED" => PullRequestState::Merged,
                "CLOSED" => PullRequestState::Closed,
                _ if p.is_draft => PullRequestState::Draft,
                _ => PullRequestState::Open,
            },
            number: p.number,
            branch: p.head_ref_name,
            title: p.title,
            url: p.url,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gh_json() {
        let json = br#"[
            {"number":3,"title":"Draft","url":"u3","state":"OPEN","isDraft":true,"headRefName":"a","isCrossRepository":false},
            {"number":2,"title":"Fork","url":"u2","state":"OPEN","isDraft":false,"headRefName":"main","isCrossRepository":true},
            {"number":1,"title":"Done","url":"u1","state":"MERGED","isDraft":false,"headRefName":"b","isCrossRepository":false},
            {"number":0,"title":"Nope","url":"u0","state":"CLOSED","isDraft":false,"headRefName":"c","isCrossRepository":false}
        ]"#;
        let prs = parse_pr_list(json).unwrap();
        let got: Vec<_> = prs
            .iter()
            .map(|p| (p.number, p.branch.as_str(), p.state))
            .collect();
        assert_eq!(
            got,
            [
                (3, "a", PullRequestState::Draft),
                (1, "b", PullRequestState::Merged),
                (0, "c", PullRequestState::Closed),
            ]
        );
        assert!(parse_pr_list(b"not json").is_err());
    }
}
