//! Pull requests via the GitHub CLI (`gh`), run on the host so it uses the
//! host's own `gh auth`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use civet_proto::{
    CheckState, CreatePullRequest, PullRequestCheck, PullRequestDetail, PullRequestInfo,
    PullRequestState, ReviewDecision, ReviewThread,
};
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
    Ok(list_pull_requests_with_heads(dir)
        .await?
        .into_iter()
        .map(|(pr, _)| pr)
        .collect())
}

/// [`list_pull_requests`] with each PR's head commit, if `gh` reported one.
pub(crate) async fn list_pull_requests_with_heads(
    dir: &Path,
) -> Result<Vec<(PullRequestInfo, Option<String>)>, SessionError> {
    let out = gh(
        dir,
        &[
            "pr",
            "list",
            "--state",
            "all",
            "--limit",
            LIST_LIMIT,
            "--json",
            "number,title,url,state,isDraft,headRefName,headRefOid,isCrossRepository",
        ],
        "gh pr list",
        TIMEOUT,
    )
    .await?;
    parse_pr_list(&out)
}

/// Run `gh` in `dir` without prompts; its stdout. `what` names it in errors.
async fn gh(
    dir: &Path,
    args: &[&str],
    what: &str,
    timeout: Duration,
) -> Result<Vec<u8>, SessionError> {
    let gh = find_gh().ok_or_else(|| SessionError::Spawn("gh not found".into()))?;
    let mut cmd = tokio::process::Command::new(&gh);
    cmd.args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("NO_COLOR", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .kill_on_drop(true);
    let out = tokio::time::timeout(timeout, cmd.output())
        .await
        .map_err(|_| SessionError::Git(format!("{what} timed out")))?
        .map_err(|e| SessionError::Spawn(format!("failed to run {}: {e}", gh.display())))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        return Err(SessionError::Git(if err.is_empty() {
            format!("{what} failed")
        } else {
            err
        }));
    }
    Ok(out.stdout)
}

/// The newest non-fork pull request whose head is `branch`, with its checks
/// and reviews: one GraphQL query (`gh api graphql`, owner and name from
/// the checkout's remote). Review thread comments only with `threads`.
pub(crate) async fn pull_request_detail(
    dir: &Path,
    branch: &str,
    threads: bool,
) -> Result<Option<PullRequestDetail>, SessionError> {
    let branch = format!("branch={branch}");
    let threads = format!("threads={threads}");
    let query = format!("query={DETAIL_QUERY}");
    let out = gh(
        dir,
        &[
            "api",
            "graphql",
            "-F",
            "owner={owner}",
            "-F",
            "name={repo}",
            "-f",
            &branch,
            "-F",
            &threads,
            "-f",
            &query,
        ],
        "gh api graphql",
        TIMEOUT,
    )
    .await?;
    parse_detail(&out)
}

/// The detail query. Checks and review threads stop at 100 each (GitHub's
/// own page shows no more checks than that either).
const DETAIL_QUERY: &str = "query($owner: String!, $name: String!, $branch: String!, $threads: Boolean!) {
  repository(owner: $owner, name: $name) {
    pullRequests(headRefName: $branch, first: 10, orderBy: {field: CREATED_AT, direction: DESC}) {
      nodes {
        number title url state isDraft isCrossRepository baseRefName headRefName reviewDecision
        comments { totalCount }
        latestOpinionatedReviews(first: 100) { nodes { state } }
        commits(last: 1) { nodes { commit { statusCheckRollup { contexts(first: 100) { nodes {
          __typename
          ... on CheckRun { name status conclusion detailsUrl }
          ... on StatusContext { context state targetUrl }
        } } } } } }
        reviewThreads(first: 100) { nodes {
          isResolved isOutdated path line
          comments(first: 1) @include(if: $threads) { totalCount nodes { author { login } body url } }
        } }
      }
    }
  }
}";

/// `gh pr create` for `branch` (`--head`, so gh never pushes or asks), then
/// `gh pr view` of the new PR for its title. Returns what it created.
pub(crate) async fn create_pull_request(
    dir: &Path,
    branch: &str,
    req: &CreatePullRequest,
) -> Result<PullRequestInfo, SessionError> {
    let args = create_args(branch, req);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = gh(dir, &args, "gh pr create", CREATE_TIMEOUT).await?;
    let url = String::from_utf8_lossy(&out)
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with("https://") && l.contains("/pull/"))
        .map(str::to_owned)
        .ok_or_else(|| SessionError::Git("gh pr create printed no pull request URL".into()))?;
    let viewed = gh(
        dir,
        &[
            "pr",
            "view",
            &url,
            "--json",
            "number,title,url,state,isDraft,headRefName,isCrossRepository",
        ],
        "gh pr view",
        TIMEOUT,
    )
    .await
    .ok()
    .and_then(|json| serde_json::from_slice::<GhPr>(&json).ok());
    if let Some(pr) = viewed {
        return Ok(pr.into_info());
    }
    // The PR exists; only the lookup failed: describe it from what we sent.
    let number = url
        .rsplit('/')
        .next()
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| SessionError::Git(format!("unexpected pull request URL {url}")))?;
    Ok(PullRequestInfo {
        number,
        branch: branch.to_owned(),
        title: req.title.trim().to_owned(),
        url,
        state: if req.draft {
            PullRequestState::Draft
        } else {
            PullRequestState::Open
        },
    })
}

/// `gh pr create` pushes nothing but talks to GitHub twice.
const CREATE_TIMEOUT: Duration = Duration::from_secs(60);

fn create_args(branch: &str, req: &CreatePullRequest) -> Vec<String> {
    let mut args = vec![
        "pr".to_owned(),
        "create".to_owned(),
        format!("--head={branch}"),
    ];
    let title = req.title.trim();
    if title.is_empty() {
        args.push("--fill".into());
        if !req.body.trim().is_empty() {
            args.push(format!("--body={}", req.body));
        }
    } else {
        args.push(format!("--title={title}"));
        args.push(format!("--body={}", req.body));
    }
    if let Some(base) = req.base.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        args.push(format!("--base={base}"));
    }
    if req.draft {
        args.push("--draft".into());
    }
    args
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
    head_ref_oid: Option<String>,
    #[serde(default)]
    is_cross_repository: bool,
}

impl GhPr {
    fn into_info(self) -> PullRequestInfo {
        PullRequestInfo {
            state: pr_state(&self.state, self.is_draft),
            number: self.number,
            branch: self.head_ref_name,
            title: self.title,
            url: self.url,
        }
    }
}

fn pr_state(state: &str, is_draft: bool) -> PullRequestState {
    match state {
        "MERGED" => PullRequestState::Merged,
        "CLOSED" => PullRequestState::Closed,
        _ if is_draft => PullRequestState::Draft,
        _ => PullRequestState::Open,
    }
}

fn parse_pr_list(json: &[u8]) -> Result<Vec<(PullRequestInfo, Option<String>)>, SessionError> {
    let prs: Vec<GhPr> = serde_json::from_slice(json)
        .map_err(|e| SessionError::Git(format!("unexpected gh output: {e}")))?;
    Ok(prs
        .into_iter()
        .filter(|p| !p.is_cross_repository)
        .map(|p| {
            let oid = p.head_ref_oid.clone().filter(|o| !o.is_empty());
            (p.into_info(), oid)
        })
        .collect())
}

// ----- pull request detail (GraphQL) ----------------------------------------------

#[derive(Deserialize)]
struct GqlResponse {
    data: Option<GqlData>,
    #[serde(default)]
    errors: Vec<GqlError>,
}

#[derive(Deserialize)]
struct GqlError {
    message: String,
}

#[derive(Deserialize)]
struct GqlData {
    repository: Option<GqlRepo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlRepo {
    pull_requests: Nodes<GqlPr>,
}

#[derive(Deserialize)]
struct Nodes<T> {
    #[serde(default = "Vec::new")]
    nodes: Vec<T>,
}

#[derive(Deserialize)]
struct Count {
    #[serde(rename = "totalCount")]
    total_count: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlPr {
    number: u64,
    title: String,
    url: String,
    state: String,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    is_cross_repository: bool,
    base_ref_name: String,
    head_ref_name: String,
    review_decision: Option<String>,
    comments: Count,
    latest_opinionated_reviews: Option<Nodes<GqlReview>>,
    commits: Nodes<GqlCommitNode>,
    review_threads: Nodes<GqlThread>,
}

#[derive(Deserialize)]
struct GqlReview {
    state: String,
}

#[derive(Deserialize)]
struct GqlCommitNode {
    commit: GqlCommit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlCommit {
    status_check_rollup: Option<GqlRollup>,
}

#[derive(Deserialize)]
struct GqlRollup {
    contexts: Nodes<GqlContext>,
}

/// A `CheckRun` or a `StatusContext`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlContext {
    // CheckRun
    name: Option<String>,
    status: Option<String>,
    conclusion: Option<String>,
    details_url: Option<String>,
    // StatusContext
    context: Option<String>,
    state: Option<String>,
    target_url: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlThread {
    is_resolved: bool,
    #[serde(default)]
    is_outdated: bool,
    path: String,
    line: Option<u64>,
    comments: Option<GqlThreadComments>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlThreadComments {
    total_count: u64,
    nodes: Vec<GqlComment>,
}

#[derive(Deserialize)]
struct GqlComment {
    author: Option<GqlAuthor>,
    body: String,
    url: String,
}

#[derive(Deserialize)]
struct GqlAuthor {
    login: String,
}

fn parse_detail(json: &[u8]) -> Result<Option<PullRequestDetail>, SessionError> {
    let resp: GqlResponse = serde_json::from_slice(json)
        .map_err(|e| SessionError::Git(format!("unexpected gh output: {e}")))?;
    let Some(repo) = resp.data.and_then(|d| d.repository) else {
        let msg = resp
            .errors
            .into_iter()
            .map(|e| e.message)
            .collect::<Vec<_>>();
        return Err(SessionError::Git(if msg.is_empty() {
            "repository not found on GitHub".into()
        } else {
            msg.join("\n")
        }));
    };
    let Some(pr) = repo
        .pull_requests
        .nodes
        .into_iter()
        .find(|p| !p.is_cross_repository)
    else {
        return Ok(None);
    };

    let checks: Vec<PullRequestCheck> = pr
        .commits
        .nodes
        .into_iter()
        .flat_map(|c| c.commit.status_check_rollup)
        .flat_map(|r| r.contexts.nodes)
        .filter_map(check)
        .collect();
    let checks_state = (!checks.is_empty()).then(|| {
        let any = |s| checks.iter().any(|c| c.state == s);
        if any(CheckState::Failed) {
            CheckState::Failed
        } else if any(CheckState::Pending) {
            CheckState::Pending
        } else {
            CheckState::Passed
        }
    });
    let reviews = pr
        .latest_opinionated_reviews
        .map(|r| r.nodes)
        .unwrap_or_default();
    let count = |state: &str| reviews.iter().filter(|r| r.state == state).count() as u64;
    let unresolved: Vec<GqlThread> = pr
        .review_threads
        .nodes
        .into_iter()
        .filter(|t| !t.is_resolved)
        .collect();
    let unresolved_threads = unresolved.len() as u64;
    let threads = unresolved
        .into_iter()
        .filter_map(|t| {
            let comments = t.comments?;
            let first = comments.nodes.into_iter().next()?;
            Some(ReviewThread {
                path: t.path,
                line: t.line,
                outdated: t.is_outdated,
                author: first
                    .author
                    .map(|a| a.login)
                    .unwrap_or_else(|| "ghost".into()),
                body: plain_text(&first.body, civet_proto::MAX_REVIEW_COMMENT_CHARS),
                url: first.url,
                comments: comments.total_count,
            })
        })
        .collect();
    Ok(Some(PullRequestDetail {
        pr: PullRequestInfo {
            state: pr_state(&pr.state, pr.is_draft),
            number: pr.number,
            branch: pr.head_ref_name,
            title: pr.title,
            url: pr.url,
        },
        base: pr.base_ref_name,
        checks_state,
        checks,
        review_decision: match pr.review_decision.as_deref() {
            Some("APPROVED") => Some(ReviewDecision::Approved),
            Some("CHANGES_REQUESTED") => Some(ReviewDecision::ChangesRequested),
            Some("REVIEW_REQUIRED") => Some(ReviewDecision::ReviewRequired),
            _ => None,
        },
        approvals: count("APPROVED"),
        changes_requested: count("CHANGES_REQUESTED"),
        comments: pr.comments.total_count,
        unresolved_threads,
        threads,
    }))
}

/// One rollup entry; None for an unknown kind (neither name nor context).
fn check(c: GqlContext) -> Option<PullRequestCheck> {
    if let Some(name) = c.name {
        let state = match (c.status.as_deref(), c.conclusion.as_deref()) {
            (Some(s), _) if s != "COMPLETED" => CheckState::Pending,
            (_, Some("SUCCESS")) => CheckState::Passed,
            (_, Some("NEUTRAL" | "SKIPPED" | "STALE")) => CheckState::Skipped,
            (_, Some("CANCELLED")) => CheckState::Cancelled,
            (_, Some(_)) => CheckState::Failed,
            (_, None) => CheckState::Pending,
        };
        return Some(PullRequestCheck {
            name,
            state,
            url: c.details_url,
        });
    }
    let state = match c.state.as_deref() {
        Some("SUCCESS") => CheckState::Passed,
        Some("FAILURE" | "ERROR") => CheckState::Failed,
        _ => CheckState::Pending,
    };
    Some(PullRequestCheck {
        name: c.context?,
        state,
        url: c.target_url,
    })
}

/// A comment body as one line of plain text: HTML comments, `<details>`
/// blocks (bots fold their prompts into them) and tags left out, whitespace
/// collapsed, at most `max` characters.
fn plain_text(body: &str, max: usize) -> String {
    let mut s = body.to_owned();
    for (open, close) in [("<!--", "-->"), ("<details", "</details>")] {
        while let Some(i) = s.find(open) {
            let end = s[i..].find(close).map_or(s.len(), |j| i + j + close.len());
            s.replace_range(i..end, " ");
        }
    }
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if in_tag => {}
            _ => out.push(c),
        }
    }
    let text = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max {
        return text;
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gh_json() {
        let json = br#"[
            {"number":3,"title":"Draft","url":"u3","state":"OPEN","isDraft":true,"headRefName":"a","isCrossRepository":false},
            {"number":2,"title":"Fork","url":"u2","state":"OPEN","isDraft":false,"headRefName":"main","isCrossRepository":true},
            {"number":1,"title":"Done","url":"u1","state":"MERGED","isDraft":false,"headRefName":"b","headRefOid":"abc","isCrossRepository":false},
            {"number":0,"title":"Nope","url":"u0","state":"CLOSED","isDraft":false,"headRefName":"c","isCrossRepository":false}
        ]"#;
        let prs = parse_pr_list(json).unwrap();
        let got: Vec<_> = prs
            .iter()
            .map(|(p, oid)| (p.number, p.branch.as_str(), p.state, oid.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                (3, "a", PullRequestState::Draft, None),
                (1, "b", PullRequestState::Merged, Some("abc")),
                (0, "c", PullRequestState::Closed, None),
            ]
        );
        assert!(parse_pr_list(b"not json").is_err());
    }

    #[test]
    fn parses_detail() {
        let json = br#"{"data":{"repository":{"pullRequests":{"nodes":[
            {"number":9,"title":"Fork","url":"u9","state":"OPEN","isDraft":false,"isCrossRepository":true,
             "baseRefName":"main","headRefName":"feat","reviewDecision":null,"comments":{"totalCount":0},
             "latestOpinionatedReviews":{"nodes":[]},"commits":{"nodes":[]},"reviewThreads":{"nodes":[]}},
            {"number":7,"title":"Feat","url":"https://github.com/o/r/pull/7","state":"OPEN","isDraft":true,
             "isCrossRepository":false,"baseRefName":"dev","headRefName":"feat",
             "reviewDecision":"CHANGES_REQUESTED","comments":{"totalCount":4},
             "latestOpinionatedReviews":{"nodes":[{"state":"APPROVED"},{"state":"CHANGES_REQUESTED"},{"state":"APPROVED"}]},
             "commits":{"nodes":[{"commit":{"statusCheckRollup":{"contexts":{"nodes":[
                {"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"SUCCESS","detailsUrl":"d1"},
                {"__typename":"CheckRun","name":"lint","status":"IN_PROGRESS","conclusion":null,"detailsUrl":"d2"},
                {"__typename":"CheckRun","name":"bot","status":"COMPLETED","conclusion":"CANCELLED","detailsUrl":null},
                {"__typename":"StatusContext","context":"ci/old","state":"ERROR","targetUrl":"t"},
                {"__typename":"CheckRun","name":"opt","status":"COMPLETED","conclusion":"SKIPPED","detailsUrl":null}
             ]}}}}]},
             "reviewThreads":{"nodes":[
                {"isResolved":true,"isOutdated":false,"path":"a.rs","line":3,
                 "comments":{"totalCount":1,"nodes":[{"author":{"login":"x"},"body":"done","url":"c0"}]}},
                {"isResolved":false,"isOutdated":false,"path":"b.rs","line":12,
                 "comments":{"totalCount":2,"nodes":[{"author":{"login":"rev"},"body":"<!-- x -->Use **`foo`** here","url":"c1"}]}},
                {"isResolved":false,"isOutdated":true,"path":"c.rs","line":null,
                 "comments":{"totalCount":1,"nodes":[{"author":null,"body":"gone","url":"c2"}]}}
             ]}}
        ]}}}}"#;
        let d = parse_detail(json).unwrap().unwrap();
        assert_eq!(d.pr.number, 7);
        assert_eq!(d.pr.state, PullRequestState::Draft);
        assert_eq!(d.base, "dev");
        let checks: Vec<_> = d
            .checks
            .iter()
            .map(|c| (c.name.as_str(), c.state))
            .collect();
        assert_eq!(
            checks,
            [
                ("test", CheckState::Passed),
                ("lint", CheckState::Pending),
                ("bot", CheckState::Cancelled),
                ("ci/old", CheckState::Failed),
                ("opt", CheckState::Skipped),
            ]
        );
        assert_eq!(d.checks_state, Some(CheckState::Failed));
        assert_eq!(d.review_decision, Some(ReviewDecision::ChangesRequested));
        assert_eq!((d.approvals, d.changes_requested), (2, 1));
        assert_eq!(d.comments, 4);
        assert_eq!(d.unresolved_threads, 2);
        assert_eq!(d.threads.len(), 2);
        assert_eq!(d.threads[0].path, "b.rs");
        assert_eq!(d.threads[0].line, Some(12));
        assert_eq!(d.threads[0].body, "Use **`foo`** here");
        assert_eq!(d.threads[0].comments, 2);
        assert_eq!(d.threads[1].author, "ghost");
        assert!(d.threads[1].outdated);
    }

    #[test]
    fn detail_without_threads_pending_and_none() {
        // Threads not asked for: no comments, but the count is there.
        let json = br#"{"data":{"repository":{"pullRequests":{"nodes":[
            {"number":1,"title":"T","url":"u","state":"MERGED","isDraft":false,"isCrossRepository":false,
             "baseRefName":"main","headRefName":"b","reviewDecision":"APPROVED","comments":{"totalCount":0},
             "latestOpinionatedReviews":{"nodes":[]},
             "commits":{"nodes":[{"commit":{"statusCheckRollup":{"contexts":{"nodes":[
                {"__typename":"CheckRun","name":"a","status":"COMPLETED","conclusion":"SUCCESS","detailsUrl":null},
                {"__typename":"StatusContext","context":"b","state":"PENDING","targetUrl":null}
             ]}}}}]},
             "reviewThreads":{"nodes":[{"isResolved":false,"isOutdated":false,"path":"x","line":1}]}}
        ]}}}}"#;
        let d = parse_detail(json).unwrap().unwrap();
        assert_eq!(d.pr.state, PullRequestState::Merged);
        assert_eq!(d.checks_state, Some(CheckState::Pending));
        assert_eq!(d.review_decision, Some(ReviewDecision::Approved));
        assert_eq!(d.unresolved_threads, 1);
        assert!(d.threads.is_empty());

        // No PR, no checks.
        let none = br#"{"data":{"repository":{"pullRequests":{"nodes":[]}}}}"#;
        assert_eq!(parse_detail(none).unwrap(), None);
        let no_checks = br#"{"data":{"repository":{"pullRequests":{"nodes":[
            {"number":1,"title":"T","url":"u","state":"OPEN","baseRefName":"main","headRefName":"b",
             "reviewDecision":null,"comments":{"totalCount":0},"latestOpinionatedReviews":null,
             "commits":{"nodes":[{"commit":{"statusCheckRollup":null}}]},"reviewThreads":{"nodes":[]}}
        ]}}}}"#;
        let d = parse_detail(no_checks).unwrap().unwrap();
        assert_eq!(d.checks_state, None);
        assert_eq!(d.review_decision, None);

        // GraphQL errors come back as the message.
        let err = br#"{"data":{"repository":null},"errors":[{"message":"Could not resolve to a Repository"}]}"#;
        match parse_detail(err) {
            Err(SessionError::Git(m)) => assert_eq!(m, "Could not resolve to a Repository"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn comment_bodies_become_plain_text() {
        assert_eq!(plain_text("a\n\n  b", 100), "a b");
        assert_eq!(
            plain_text(
                "<!-- meta -->**Major**\n<details><summary>Prompt</summary>long</details>\nFix <b>it</b>.",
                100
            ),
            "**Major** Fix it ."
        );
        // An unclosed block runs to the end.
        assert_eq!(plain_text("keep <!-- never closed", 100), "keep");
        assert_eq!(plain_text("abcdef ghij", 8), "abcdef…");
        assert_eq!(plain_text("éééééé", 4), "ééé…");
    }

    #[test]
    fn create_arguments() {
        let req = |title: &str, body: &str, base: Option<&str>, draft| CreatePullRequest {
            worktree: civet_proto::WorktreeId::from("/w"),
            title: title.into(),
            body: body.into(),
            base: base.map(Into::into),
            draft,
        };
        assert_eq!(
            create_args("feat/x", &req(" Add x ", "-body", Some("dev"), true)),
            [
                "pr",
                "create",
                "--head=feat/x",
                "--title=Add x",
                "--body=-body",
                "--base=dev",
                "--draft"
            ]
        );
        // No title: from the commits; an empty body and base are left to gh.
        assert_eq!(
            create_args("b", &req("", " ", Some(" "), false)),
            ["pr", "create", "--head=b", "--fill"]
        );
        assert_eq!(
            create_args("b", &req("", "Why", None, false)),
            ["pr", "create", "--head=b", "--fill", "--body=Why"]
        );
    }
}
