//! `civetd handoff` and `civetd install-skills` (feature `server`, client
//! side): hand a task from an agent running in a Civet pane to a new agent
//! in a fresh worktree of the same repo, through the daemon that runs the
//! pane. In panes, the `civet` PATH shim (`<state>/hooks/bin/civet`) runs
//! this binary with that daemon's `CIVET_STATE_DIR`, so agents type
//! `civet handoff …`. See `skills/civet-handoff/SKILL.md` (the skill that
//! drives it) and `docs/INTERFACES.md` 5.1.
//!
//! Steps: find the repo and HEAD of `--from` (default: the current
//! directory) with git; connect to `<state>/civetd.sock` (key from
//! `daemon.json`; never starts a daemon); register the repo if needed;
//! `CreateWorktree` on a new branch from HEAD (a slug of the title by
//! default, `-2`, `-3`… on collision); optionally apply the uncommitted
//! changes; `SetupWorktree` (the repo's worktree template: copied files,
//! `CIVET_PORT`, the setup script in its own pane); `SpawnPane` running
//! `<agent> <prompt>` there (the template's agent command and prompt unless
//! `--agent` / a command is given). The desktop app picks it all up from the
//! daemon's events. Prints one JSON object.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use civet_proto::{
    BootstrapInfo, ClientId, CommandSpec, ErrorCode, PaneInfo, RepoId, RepoInfo, Request, Response,
    SpawnPaneRequest, TermSize, WorktreeInfo, WorktreeSetup,
};
use civet_transport::{ClientSession, ClientSessionConfig, RequestError, StreamDialer};
use tokio::process::Command;

use crate::paths::{StatePaths, state_dir_or_default};

/// The handoff skill (Claude Code and Codex read the same `SKILL.md`).
pub const SKILL_MD: &str = include_str!("../skills/civet-handoff/SKILL.md");
/// Skill directory name under `~/.claude/skills` / `~/.codex/skills`.
pub const SKILL_NAME: &str = "civet-handoff";

/// Agents `--agent` accepts without an explicit command.
const AGENTS: &[&str] = &["claude", "codex"];
/// Longest generated branch slug.
const MAX_SLUG: usize = 48;
/// Longest generated pane title.
const MAX_TITLE: usize = 60;
/// Branch suffixes tried when the generated name is taken.
const MAX_SUFFIX: u32 = 50;
/// Initial size of the new pane (clients resize it when they attach).
const PANE_SIZE: TermSize = TermSize::new(120, 36);
/// Connecting to the local socket and each request.
const TIMEOUT: Duration = Duration::from_secs(20);
/// `SetupWorktree`: the daemon copies the template's files before it
/// answers, which can take long for a big directory.
const SETUP_TIMEOUT: Duration = Duration::from_secs(300);

/// What to hand off.
#[derive(Debug, Clone)]
pub struct HandoffOptions {
    /// Prompt for the new agent (the task brief).
    pub prompt: String,
    /// Pane title; default: the prompt's first line.
    pub title: Option<String>,
    /// Branch; default: a slug of the title, suffixed on collision. An
    /// explicit branch that exists is an error.
    pub branch: Option<String>,
    /// Rev to branch from; default: HEAD of `from`.
    pub base: Option<String>,
    /// `claude` or `codex` (or any agent name when `command` is given).
    /// None: the repo template's agent command, else `claude`.
    pub agent: Option<String>,
    /// Command instead of `<agent>`; the prompt is appended as its last
    /// argument.
    pub command: Vec<String>,
    /// A directory in the source checkout; default: the current directory.
    pub from: Option<PathBuf>,
    /// Apply the source checkout's uncommitted changes (tracked and
    /// untracked, not ignored) to the new worktree.
    pub carry_changes: bool,
    /// Apply the repo's worktree template (copied files, setup script,
    /// agent command, prompt prefix).
    pub setup: bool,
    /// Daemon state dir; default `$CIVET_STATE_DIR` (set by the `civet`
    /// shim), else `~/.civet`.
    pub state_dir: Option<PathBuf>,
    /// Allow running outside a Civet pane (no `CIVET_PANE_ID`).
    pub outside_pane: bool,
}

/// Result, printed as JSON.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Handoff {
    /// Repository id.
    pub repo: RepoId,
    /// New worktree path.
    pub worktree: String,
    /// New branch.
    pub branch: String,
    /// Commit it was branched from.
    pub base: String,
    /// New pane id.
    pub pane: u64,
    /// Pane title.
    pub title: String,
    /// Agent.
    pub agent: String,
    /// Uncommitted changes were applied.
    pub carried_changes: bool,
    /// The pane running the template's setup script.
    pub setup_pane: Option<u64>,
    /// The worktree's `CIVET_PORT`.
    pub port: Option<u16>,
    /// Files copied from the main checkout by the template.
    pub copied: Vec<String>,
    /// Template problems that did not stop the handoff.
    pub warnings: Vec<String>,
}

/// Run a handoff. Errors are meant to be shown as is.
pub async fn handoff(opts: HandoffOptions) -> anyhow::Result<Handoff> {
    if !opts.outside_pane && std::env::var_os(civet_proto::ENV_PANE_ID).is_none() {
        bail!(
            "not running inside a Civet pane ({} is not set); start the agent from a Civet \
             pane, or pass --outside-pane to use the daemon of {} anyway",
            civet_proto::ENV_PANE_ID,
            state_dir_or_default(opts.state_dir.as_deref()).display()
        );
    }
    let prompt = opts.prompt.trim();
    if prompt.is_empty() {
        bail!("the prompt is empty");
    }
    let agent = opts.agent.as_deref().map(|a| a.trim().to_ascii_lowercase());
    if let Some(a) = &agent
        && opts.command.is_empty()
        && !AGENTS.contains(&a.as_str())
    {
        bail!("unknown agent {a:?} (claude or codex, or give a command after --)");
    }
    let title = opts
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| truncate_chars(t, MAX_TITLE))
        .unwrap_or_else(|| default_title(prompt));

    let from = match &opts.from {
        Some(p) => p.clone(),
        None => std::env::current_dir().context("current directory")?,
    };
    let src = SourceCheckout::inspect(&from).await?;
    let base = match opts
        .base
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        Some(b) => b.to_owned(),
        None => src.head.clone(),
    };
    let patch = if opts.carry_changes {
        if base != src.head {
            bail!("--carry-changes needs the new worktree to start at HEAD (drop --base)");
        }
        Some(src.uncommitted_patch().await?)
    } else {
        None
    };

    let state_dir = state_dir_or_default(opts.state_dir.as_deref());
    let session = connect(&state_dir).await?;
    let r = run(
        &session,
        &opts,
        &src,
        &base,
        &title,
        agent.as_deref(),
        prompt,
        patch,
    )
    .await;
    session.close().await;
    r
}

#[allow(clippy::too_many_arguments)]
async fn run(
    session: &ClientSession,
    opts: &HandoffOptions,
    src: &SourceCheckout,
    base: &str,
    title: &str,
    agent: Option<&str>,
    prompt: &str,
    patch: Option<Vec<u8>>,
) -> anyhow::Result<Handoff> {
    let repo = find_or_add_repo(session, src).await?;
    let wt = create_worktree(session, &repo, opts.branch.as_deref(), title, base).await?;
    let branch = wt.branch.clone().unwrap_or_default();

    let carried = match &patch {
        Some(p) if !p.is_empty() => {
            if let Err(e) = apply_patch(Path::new(&wt.path), p).await {
                // Nothing runs there yet: leave no half-made worktree behind.
                let _ = request(
                    session,
                    Request::RemoveWorktree {
                        worktree: wt.id.clone(),
                        force: true,
                    },
                )
                .await;
                return Err(e.context(format!(
                    "applying the uncommitted changes to {} (worktree removed again)",
                    wt.path
                )));
            }
            true
        }
        _ => false,
    };

    let (setup, setup_error) = match opts.setup {
        true => match setup_worktree(session, &wt).await {
            Ok(s) => (Some(s), None),
            Err(e) => (None, Some(e)),
        },
        false => (None, None),
    };
    let template = setup.as_ref().map(|s| &s.template);
    let (mut argv, agent) = agent_argv(
        &opts.command,
        agent,
        template.and_then(|t| t.agent.as_deref()),
    )?;
    argv.push(with_prompt_prefix(
        template.and_then(|t| t.prompt.as_deref()),
        prompt,
    ));
    let (program, args) = argv.split_first().expect("non-empty");
    let pane = spawn(
        session,
        SpawnPaneRequest {
            worktree: Some(wt.id.clone()),
            cwd: None,
            command: Some(CommandSpec::new(program.clone(), args.iter().cloned())),
            env: Vec::new(),
            size: PANE_SIZE,
            title: Some(title.to_owned()),
            agent: Some(agent.clone()),
        },
    )
    .await
    .with_context(|| {
        format!(
            "worktree {} was created, but starting {program} failed",
            wt.path
        )
    })?;

    Ok(Handoff {
        repo: repo.id,
        worktree: wt.path,
        branch,
        base: base.to_owned(),
        pane: pane.id.0,
        title: title.to_owned(),
        agent,
        carried_changes: carried,
        setup_pane: setup
            .as_ref()
            .and_then(|s| s.setup_pane.as_ref())
            .map(|p| p.id.0),
        port: setup.as_ref().and_then(|s| s.port),
        copied: setup.as_ref().map(|s| s.copied.clone()).unwrap_or_default(),
        warnings: setup
            .map(|s| s.warnings)
            .unwrap_or_default()
            .into_iter()
            .chain(setup_error)
            .collect(),
    })
}

/// `SetupWorktree` for the new worktree. A failure is printed (and ends up
/// in the result's `warnings`) and the handoff goes on without the
/// template: the agent matters more.
async fn setup_worktree(
    session: &ClientSession,
    wt: &WorktreeInfo,
) -> Result<WorktreeSetup, String> {
    let req = Request::SetupWorktree {
        worktree: wt.id.clone(),
        size: PANE_SIZE,
    };
    let r = match request_within(session, req, SETUP_TIMEOUT).await {
        Ok(Response::WorktreeSetUp(s)) => {
            for w in &s.warnings {
                eprintln!("civet handoff: worktree template: {w}");
            }
            Ok(s)
        }
        Ok(other) => Err(format!(
            "unexpected response to the worktree setup: {other:?}"
        )),
        Err(e) => Err(format!("the worktree template was not applied: {e}")),
    };
    r.inspect_err(|e| eprintln!("civet handoff: {e}"))
}

/// argv (without the prompt) and the agent name: the command after `--`;
/// else the template's agent command if it runs `--agent` (or no `--agent`
/// was given: `claude --model opus` for `--agent claude`); else `--agent`;
/// else `claude`.
fn agent_argv(
    command: &[String],
    agent: Option<&str>,
    template_agent: Option<&str>,
) -> anyhow::Result<(Vec<String>, String)> {
    if !command.is_empty() {
        return Ok((command.to_vec(), agent.unwrap_or("claude").to_owned()));
    }
    if let Some(line) = template_agent.filter(|l| !l.trim().is_empty()) {
        let argv = split_shell_words(line)
            .with_context(|| format!("the worktree template's agent command {line:?}"))?;
        let name = argv[0].rsplit('/').next().unwrap_or(&argv[0]).to_owned();
        if agent.is_none_or(|a| a == name) {
            return Ok((argv, name));
        }
    }
    let a = agent.unwrap_or("claude");
    Ok((vec![a.to_owned()], a.to_owned()))
}

/// The brief, after the template's default prompt (if any) and a blank line.
fn with_prompt_prefix(prefix: Option<&str>, prompt: &str) -> String {
    match prefix.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => format!("{p}\n\n{prompt}"),
        None => prompt.to_owned(),
    }
}

/// POSIX-shell-like word splitting: whitespace separates words; single
/// quotes are literal; double quotes allow `\"`, `\\`, `\$` and `\``;
/// a backslash outside quotes escapes the next character. No expansion.
pub fn split_shell_words(line: &str) -> anyhow::Result<Vec<String>> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => cur.push(c),
                        None => bail!("unterminated single quote"),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\' | '$' | '`')) => cur.push(c),
                            Some(c) => {
                                cur.push('\\');
                                cur.push(c);
                            }
                            None => bail!("unterminated double quote"),
                        },
                        Some(c) => cur.push(c),
                        None => bail!("unterminated double quote"),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(c) => cur.push(c),
                    None => bail!("trailing backslash"),
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    if words.is_empty() {
        bail!("empty command");
    }
    Ok(words)
}

/// Connect to the running daemon of `state_dir` over its unix socket.
async fn connect(state_dir: &Path) -> anyhow::Result<ClientSession> {
    let paths = StatePaths::new(state_dir);
    let not_running = |why: String| {
        anyhow::anyhow!(
            "no Civet daemon is reachable for {} ({why}); is the Civet app connected to this host?",
            state_dir.display()
        )
    };
    let line = std::fs::read_to_string(paths.daemon_json())
        .map_err(|e| not_running(format!("{}: {e}", paths.daemon_json().display())))?;
    let info = BootstrapInfo::from_json_line(&line)
        .map_err(|e| not_running(format!("bad daemon.json: {e}")))?;
    let socket = paths.socket();
    let dialer = StreamDialer {
        factory: move || tokio::net::UnixStream::connect(socket.clone()),
    };
    let config = ClientSessionConfig::new(
        info.session_key.clone(),
        ClientId::generate()?,
        "civet-handoff",
        crate::VERSION,
    );
    let (session, _events) =
        tokio::time::timeout(TIMEOUT, ClientSession::connect(config, Arc::new(dialer)))
            .await
            .map_err(|_| not_running("timed out".into()))?
            .map_err(|e| not_running(e.to_string()))?;
    Ok(session)
}

async fn request(session: &ClientSession, req: Request) -> Result<Response, RequestError> {
    request_within(session, req, TIMEOUT).await
}

async fn request_within(
    session: &ClientSession,
    req: Request,
    limit: Duration,
) -> Result<Response, RequestError> {
    match tokio::time::timeout(limit, session.request(req)).await {
        Ok(r) => r,
        Err(_) => Err(RequestError::ConnectionLost),
    }
}

fn rpc_code(e: &RequestError) -> Option<ErrorCode> {
    match e {
        RequestError::Rpc(r) => Some(r.code),
        _ => None,
    }
}

/// The registered repo of `src`, registering it if needed.
async fn find_or_add_repo(
    session: &ClientSession,
    src: &SourceCheckout,
) -> anyhow::Result<RepoInfo> {
    match request(session, Request::ListRepos).await? {
        Response::Repos(repos) => {
            if let Some(r) = repos.into_iter().find(|r| same_path(&r.path, &src.main)) {
                return Ok(r);
            }
        }
        other => bail!("unexpected response {other:?}"),
    }
    let path = src.main.display().to_string();
    match request(session, Request::AddRepo { path: path.clone() }).await {
        Ok(Response::Repo(r)) => Ok(r),
        Ok(other) => bail!("unexpected response {other:?}"),
        Err(e) => Err(anyhow::Error::new(e).context(format!("registering repo {path}"))),
    }
}

/// `CreateWorktree` on `branch`, or on a slug of `title` with a numeric
/// suffix until one is free.
async fn create_worktree(
    session: &ClientSession,
    repo: &RepoInfo,
    branch: Option<&str>,
    title: &str,
    base: &str,
) -> anyhow::Result<WorktreeInfo> {
    let explicit = branch.map(str::trim).filter(|b| !b.is_empty());
    let stem = explicit.map(str::to_owned).unwrap_or_else(|| slug(title));
    for n in 1..=MAX_SUFFIX {
        let branch = if n == 1 {
            stem.clone()
        } else {
            format!("{stem}-{n}")
        };
        let req = Request::CreateWorktree {
            repo: repo.id.clone(),
            branch: branch.clone(),
            base: Some(base.to_owned()),
            path: None,
        };
        match request(session, req).await {
            Ok(Response::Worktree(w)) => return Ok(w),
            Ok(other) => bail!("unexpected response {other:?}"),
            Err(e) if explicit.is_none() && rpc_code(&e) == Some(ErrorCode::AlreadyExists) => {}
            Err(e) => {
                return Err(anyhow::Error::new(e).context(format!("creating worktree {branch}")));
            }
        }
    }
    bail!("no free branch name {stem}, {stem}-2 … {stem}-{MAX_SUFFIX}; pass --branch")
}

async fn spawn(session: &ClientSession, req: SpawnPaneRequest) -> anyhow::Result<PaneInfo> {
    match request(session, Request::SpawnPane(req)).await? {
        Response::PaneSpawned(p) => Ok(p),
        other => bail!("unexpected response {other:?}"),
    }
}

fn same_path(a: &str, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(Path::new(a)) == canon(b)
}

/// The checkout the handoff comes from.
#[derive(Debug)]
struct SourceCheckout {
    /// Top level of the current worktree.
    top: PathBuf,
    /// Main worktree (what the daemon registers as the repo).
    main: PathBuf,
    /// HEAD commit.
    head: String,
}

impl SourceCheckout {
    async fn inspect(from: &Path) -> anyhow::Result<Self> {
        let top = git(from, &["rev-parse", "--show-toplevel"])
            .await
            .with_context(|| format!("{} is not inside a git checkout", from.display()))?;
        let top = PathBuf::from(top.trim());
        let head = git(&top, &["rev-parse", "--verify", "HEAD^{commit}"])
            .await
            .context("the checkout has no commits yet")?
            .trim()
            .to_owned();
        let list = git(&top, &["worktree", "list", "--porcelain"]).await?;
        let main = list
            .lines()
            .find_map(|l| l.strip_prefix("worktree "))
            .map(PathBuf::from)
            .unwrap_or_else(|| top.clone());
        Ok(Self { top, main, head })
    }

    /// `git diff --binary HEAD` plus every untracked, non-ignored file as a
    /// new-file diff. Reads only: the index and stash are left alone.
    async fn uncommitted_patch(&self) -> anyhow::Result<Vec<u8>> {
        let mut patch = git_bytes(&self.top, &["diff", "--binary", "HEAD"], &[0]).await?;
        let others = git_bytes(
            &self.top,
            &["ls-files", "-z", "--others", "--exclude-standard"],
            &[0],
        )
        .await?;
        for file in others.split(|b| *b == 0).filter(|f| !f.is_empty()) {
            let file = String::from_utf8_lossy(file);
            // --no-index exits 1 when the files differ, which they do.
            let d = git_bytes(
                &self.top,
                &["diff", "--no-index", "--binary", "--", "/dev/null", &file],
                &[0, 1],
            )
            .await?;
            patch.extend_from_slice(&d);
        }
        Ok(patch)
    }
}

async fn apply_patch(dir: &Path, patch: &[u8]) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt as _;
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["apply", "--binary", "--whitespace=nowarn", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("running git")?;
    let mut stdin = child.stdin.take().expect("piped");
    stdin.write_all(patch).await?;
    drop(stdin);
    let out = child.wait_with_output().await?;
    if !out.status.success() {
        bail!("git apply: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

async fn git(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = git_bytes(dir, args, &[0]).await?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Run git in `dir`; exit codes in `ok` count as success.
async fn git_bytes(dir: &Path, args: &[&str], ok: &[i32]) -> anyhow::Result<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .context("running git")?;
    if !out.status.code().is_some_and(|c| ok.contains(&c)) {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

/// Branch name from a title: lowercase ASCII words joined by `-`, at most
/// [`MAX_SLUG`] characters, `handoff` if nothing is left.
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for word in title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        let word = word.to_ascii_lowercase();
        let sep = usize::from(!out.is_empty());
        if out.len() + sep + word.len() > MAX_SLUG {
            if out.is_empty() {
                out = word[..MAX_SLUG].to_owned();
            }
            break;
        }
        if sep == 1 {
            out.push('-');
        }
        out.push_str(&word);
    }
    if out.is_empty() {
        "handoff".to_owned()
    } else {
        out
    }
}

/// Title from a prompt: its first non-empty line without Markdown heading
/// marks, at most [`MAX_TITLE`] characters.
pub fn default_title(prompt: &str) -> String {
    let line = prompt
        .lines()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .find(|l| !l.is_empty())
        .unwrap_or("handoff");
    truncate_chars(line, MAX_TITLE)
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut t: String = s.chars().take(max - 1).collect();
    t.truncate(t.trim_end().len());
    t.push('…');
    t
}

/// Where `install-skills` writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillTarget {
    /// `~/.claude/skills/civet-handoff/SKILL.md`
    Claude,
    /// `${CODEX_HOME:-~/.codex}/skills/civet-handoff/SKILL.md`
    Codex,
}

impl SkillTarget {
    /// The skill file for this agent under `home`.
    pub fn path(self, home: &Path, codex_home: Option<&Path>) -> PathBuf {
        let root = match self {
            SkillTarget::Claude => home.join(".claude"),
            SkillTarget::Codex => codex_home
                .map(Path::to_path_buf)
                .unwrap_or_else(|| home.join(".codex")),
        };
        root.join("skills").join(SKILL_NAME).join("SKILL.md")
    }
}

/// Write the skill for each target; returns the files written.
pub fn install_skills(targets: &[SkillTarget]) -> anyhow::Result<Vec<PathBuf>> {
    let home = dirs::home_dir().context("no home directory")?;
    let codex_home = std::env::var_os("CODEX_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let mut written = Vec::new();
    for t in targets {
        let file = t.path(&home, codex_home.as_deref());
        let dir = file.parent().expect("has a parent");
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        std::fs::write(&file, SKILL_MD).with_context(|| format!("writing {}", file.display()))?;
        written.push(file);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(
            slug("Fix the flaky `ports` test!"),
            "fix-the-flaky-ports-test"
        );
        assert_eq!(slug("Bump tokio → 1.54"), "bump-tokio-1-54");
        assert_eq!(slug("日本語"), "handoff");
        let long = slug(&"word ".repeat(30));
        assert!(long.len() <= MAX_SLUG && !long.ends_with('-'), "{long}");
        assert_eq!(slug(&"x".repeat(80)).len(), MAX_SLUG);
    }

    #[test]
    fn titles() {
        assert_eq!(default_title("\n\n# Fix the bug\n\nDetails"), "Fix the bug");
        let t = default_title(&"a".repeat(100));
        assert_eq!(t.chars().count(), MAX_TITLE);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn shell_words() {
        let w = |s: &str| split_shell_words(s).unwrap();
        assert_eq!(w("claude"), ["claude"]);
        assert_eq!(w("  claude --model opus  "), ["claude", "--model", "opus"]);
        assert_eq!(
            w(r#"codex -c 'a b' "c \"d\" \x" e\ f"#),
            ["codex", "-c", "a b", r#"c "d" \x"#, "e f"]
        );
        assert_eq!(w("''"), [""]);
        assert!(split_shell_words("'open").is_err());
        assert!(split_shell_words("   ").is_err());
    }

    #[test]
    fn agent_choice() {
        let none: &[String] = &[];
        let cmd = vec!["/bin/x".to_owned()];
        assert_eq!(
            agent_argv(&cmd, None, Some("codex")).unwrap(),
            (cmd.clone(), "claude".to_owned())
        );
        assert_eq!(
            agent_argv(none, Some("codex"), Some("claude --x")).unwrap(),
            (vec!["codex".to_owned()], "codex".to_owned())
        );
        assert_eq!(
            agent_argv(none, Some("claude"), Some("claude --x")).unwrap(),
            (
                vec!["claude".to_owned(), "--x".to_owned()],
                "claude".to_owned()
            )
        );
        assert_eq!(
            agent_argv(none, None, Some("/opt/bin/codex --full-auto")).unwrap(),
            (
                vec!["/opt/bin/codex".to_owned(), "--full-auto".to_owned()],
                "codex".to_owned()
            )
        );
        assert_eq!(
            agent_argv(none, None, Some(" ")).unwrap(),
            (vec!["claude".to_owned()], "claude".to_owned())
        );
        assert_eq!(
            with_prompt_prefix(Some(" Be brief. "), "Fix it"),
            "Be brief.\n\nFix it"
        );
        assert_eq!(with_prompt_prefix(Some(""), "Fix it"), "Fix it");
    }

    #[test]
    fn skill_paths() {
        let home = Path::new("/h");
        assert_eq!(
            SkillTarget::Claude.path(home, None),
            Path::new("/h/.claude/skills/civet-handoff/SKILL.md")
        );
        assert_eq!(
            SkillTarget::Codex.path(home, Some(Path::new("/c"))),
            Path::new("/c/skills/civet-handoff/SKILL.md")
        );
        assert!(SKILL_MD.starts_with("---\nname: civet-handoff\n"));
    }
}
