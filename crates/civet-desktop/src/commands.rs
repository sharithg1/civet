//! Tauri commands (docs/INTERFACES.md §6.1). Thin wrappers over
//! [`HostManager`]; every one is `async` so none runs on the main thread,
//! except `write_clipboard`, which must (AppKit).
//! Keep the list in sync with `build.rs` (ACL) and the capability file.

use civet_proto::{
    AgentStatus, KillSignal, LaunchAgentRequest, PaneId, PaneInfo, RepoId, ScrollbackQuery,
    SpawnPaneRequest, SubscribeInfo, TermSize, WorktreeId, WorktreeInfo,
};
use std::sync::Arc;
use tauri::State;
use tauri::ipc::{Channel, InvokeResponseBody};

use crate::batch::RecordSink;
use crate::error::CmdError;
use crate::events::{
    DaemonInfoView, ForwardView, HostSummary, HostTarget, LaunchedAgent, PingResult, PortsEvent,
    RepoView, ScrollbackPageView, TransportChoice,
};
use crate::hosts::HostManager;

type Res<T> = Result<T, CmdError>;

/// Managed state.
#[derive(Debug, Clone)]
pub struct AppState {
    /// Every host.
    pub hosts: HostManager,
}

/// A pane's binary output channel.
struct ChannelSink(Channel<InvokeResponseBody>);

impl RecordSink for ChannelSink {
    fn send(&self, message: Vec<u8>) -> bool {
        self.0.send(InvokeResponseBody::Raw(message)).is_ok()
    }
}

/// `onBinary` strings carry one byte per char (codes 0–255).
pub fn binary_string_bytes(data: &str) -> Vec<u8> {
    data.chars()
        .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
        .collect()
}

#[tauri::command]
pub async fn connect_host(
    state: State<'_, AppState>,
    target: HostTarget,
    transport: Option<TransportChoice>,
) -> Res<HostSummary> {
    state
        .hosts
        .connect_host(target, transport.unwrap_or_default())
        .await
}

/// Connect again with the host's stored target (ssh port / extra args) and
/// transport choice (the Reconnect actions).
#[tauri::command]
pub async fn reconnect_host(state: State<'_, AppState>, host_id: String) -> Res<HostSummary> {
    state.hosts.reconnect_host(&host_id).await
}

/// Restart the host's daemon on this app's version (`--replace`; closes its
/// panes). The UI confirms first.
#[tauri::command]
pub async fn restart_daemon(state: State<'_, AppState>, host_id: String) -> Res<HostSummary> {
    state.hosts.restart_daemon(&host_id).await
}

#[tauri::command]
pub async fn disconnect_host(state: State<'_, AppState>, host_id: String) -> Res<()> {
    state.hosts.disconnect_host(&host_id).await
}

#[tauri::command]
pub async fn list_hosts(state: State<'_, AppState>) -> Res<Vec<HostSummary>> {
    Ok(state.hosts.list_hosts())
}

#[tauri::command]
pub async fn notify_network_change(state: State<'_, AppState>) -> Res<()> {
    state.hosts.notify_network_change();
    Ok(())
}

#[tauri::command]
pub async fn daemon_info(state: State<'_, AppState>, host_id: String) -> Res<DaemonInfoView> {
    state.hosts.daemon_info(&host_id).await
}

#[tauri::command]
pub async fn ping(state: State<'_, AppState>, host_id: String) -> Res<PingResult> {
    state.hosts.ping(&host_id).await
}

#[tauri::command]
pub async fn list_repos(state: State<'_, AppState>, host_id: String) -> Res<Vec<RepoView>> {
    state.hosts.list_repos(&host_id).await
}

#[tauri::command]
pub async fn add_repo(state: State<'_, AppState>, host_id: String, path: String) -> Res<RepoView> {
    state.hosts.add_repo(&host_id, path).await
}

#[tauri::command]
pub async fn remove_repo(state: State<'_, AppState>, host_id: String, repo: RepoId) -> Res<()> {
    state.hosts.remove_repo(&host_id, repo).await
}

#[tauri::command]
pub async fn prepare_file_drop(
    state: State<'_, AppState>,
    host_id: String,
    paths: Vec<String>,
) -> Res<Vec<crate::hosts::DroppedFile>> {
    state.hosts.prepare_file_drop(&host_id, paths).await
}

/// Raw-body command (no JSON array of bytes): the file contents are the
/// invoke body, host and file name ride in `x-civet-host` / `x-civet-name`.
#[tauri::command]
pub async fn upload_file(
    state: State<'_, AppState>,
    request: tauri::ipc::Request<'_>,
) -> Res<String> {
    let tauri::ipc::InvokeBody::Raw(data) = request.body() else {
        return Err(CmdError::new(
            "invalidArgument",
            "upload_file expects a raw body",
        ));
    };
    let header = |k: &str| {
        request
            .headers()
            .get(k)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .ok_or_else(|| CmdError::new("invalidArgument", format!("upload_file: missing {k}")))
    };
    let (host_id, name) = (header("x-civet-host")?, header("x-civet-name")?);
    state.hosts.upload_file(&host_id, name, data.clone()).await
}

#[tauri::command]
pub async fn list_worktrees(
    state: State<'_, AppState>,
    host_id: String,
    repo: RepoId,
) -> Res<Vec<WorktreeInfo>> {
    state.hosts.list_worktrees(&host_id, repo).await
}

#[tauri::command]
pub async fn list_pull_requests(
    state: State<'_, AppState>,
    host_id: String,
    repo: RepoId,
) -> Res<Vec<civet_proto::PullRequestInfo>> {
    state.hosts.list_pull_requests(&host_id, repo).await
}

#[tauri::command]
pub async fn worktree_template(
    state: State<'_, AppState>,
    host_id: String,
    repo: RepoId,
) -> Res<civet_proto::WorktreeTemplateInfo> {
    state.hosts.worktree_template(&host_id, repo).await
}

#[tauri::command]
pub async fn set_worktree_template(
    state: State<'_, AppState>,
    host_id: String,
    repo: RepoId,
    template: civet_proto::WorktreeTemplate,
) -> Res<civet_proto::WorktreeTemplateInfo> {
    state
        .hosts
        .set_worktree_template(&host_id, repo, template)
        .await
}

/// Apply the repo's worktree template to a new worktree (copies, port,
/// setup pane); the UI then starts the agent.
#[tauri::command]
pub async fn setup_worktree(
    state: State<'_, AppState>,
    host_id: String,
    worktree: WorktreeId,
    size: civet_proto::TermSize,
) -> Res<civet_proto::WorktreeSetup> {
    state.hosts.setup_worktree(&host_id, worktree, size).await
}

#[tauri::command]
pub async fn pull_request_detail(
    state: State<'_, AppState>,
    host_id: String,
    worktree: WorktreeId,
    threads: bool,
) -> Res<Option<civet_proto::PullRequestDetail>> {
    state
        .hosts
        .pull_request_detail(&host_id, worktree, threads)
        .await
}

#[tauri::command]
pub async fn create_pull_request(
    state: State<'_, AppState>,
    host_id: String,
    request: civet_proto::CreatePullRequest,
) -> Res<civet_proto::PullRequestInfo> {
    state.hosts.create_pull_request(&host_id, request).await
}

#[tauri::command]
pub async fn git_status(
    state: State<'_, AppState>,
    host_id: String,
    worktree: WorktreeId,
) -> Res<civet_proto::GitStatusInfo> {
    state.hosts.git_status(&host_id, worktree).await
}

#[tauri::command]
pub async fn git_diff(
    state: State<'_, AppState>,
    host_id: String,
    worktree: WorktreeId,
    path: String,
    old_path: Option<String>,
    staged: bool,
    context_lines: u32,
) -> Res<civet_proto::GitDiffInfo> {
    state
        .hosts
        .git_diff(&host_id, worktree, path, old_path, staged, context_lines)
        .await
}

/// The worktree's agent activity timeline (turns, files changed per turn).
#[tauri::command]
pub async fn agent_activity(
    state: State<'_, AppState>,
    host_id: String,
    worktree: WorktreeId,
) -> Res<civet_proto::AgentActivity> {
    state.hosts.agent_activity(&host_id, worktree).await
}

/// One file's diff within an agent turn, between the turn's snapshots.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn turn_diff(
    state: State<'_, AppState>,
    host_id: String,
    worktree: WorktreeId,
    from: String,
    to: String,
    path: String,
    old_path: Option<String>,
    context_lines: u32,
) -> Res<civet_proto::GitDiffInfo> {
    state
        .hosts
        .turn_diff(&host_id, worktree, from, to, path, old_path, context_lines)
        .await
}

#[tauri::command]
pub async fn worktree_lifecycle(
    state: State<'_, AppState>,
    host_id: String,
    repo: RepoId,
    fetch: bool,
    review: bool,
) -> Res<civet_proto::RepoLifecycle> {
    state
        .hosts
        .worktree_lifecycle(&host_id, repo, fetch, review)
        .await
}

#[tauri::command]
pub async fn update_worktree(
    state: State<'_, AppState>,
    host_id: String,
    worktree: WorktreeId,
    strategy: civet_proto::UpdateStrategy,
) -> Res<civet_proto::WorktreeUpdate> {
    state
        .hosts
        .update_worktree(&host_id, worktree, strategy)
        .await
}

#[tauri::command]
pub async fn git_branch(
    state: State<'_, AppState>,
    host_id: String,
    worktree: WorktreeId,
) -> Res<civet_proto::GitBranchInfo> {
    state.hosts.git_branch(&host_id, worktree).await
}

#[tauri::command]
pub async fn git_action(
    state: State<'_, AppState>,
    host_id: String,
    worktree: WorktreeId,
    action: civet_proto::GitAction,
) -> Res<Option<civet_proto::GitDiscardSnapshot>> {
    state.hosts.git_action(&host_id, worktree, action).await
}

#[tauri::command]
pub async fn create_worktree(
    state: State<'_, AppState>,
    host_id: String,
    repo: RepoId,
    branch: String,
    base: Option<String>,
    path: Option<String>,
) -> Res<WorktreeInfo> {
    state
        .hosts
        .create_worktree(&host_id, repo, branch, base, path)
        .await
}

#[tauri::command]
pub async fn remove_worktree(
    state: State<'_, AppState>,
    host_id: String,
    worktree: WorktreeId,
    force: bool,
) -> Res<()> {
    state.hosts.remove_worktree(&host_id, worktree, force).await
}

#[tauri::command]
pub async fn list_dirs(
    state: State<'_, AppState>,
    host_id: String,
    path: String,
) -> Res<civet_proto::HostDirListing> {
    state.hosts.list_dirs(&host_id, path).await
}

/// CPU, memory, disk, network and GPU use of the host; `processes` also
/// lists processes by pane (the stats view).
#[tauri::command]
pub async fn host_stats(
    state: State<'_, AppState>,
    host_id: String,
    since: Option<u64>,
    processes: bool,
) -> Res<civet_proto::HostStats> {
    state.hosts.host_stats(&host_id, since, processes).await
}

#[tauri::command]
pub async fn list_files(
    state: State<'_, AppState>,
    host_id: String,
    worktree: WorktreeId,
    dir: String,
    include_ignored: bool,
) -> Res<civet_proto::DirListing> {
    state
        .hosts
        .list_files(&host_id, worktree, dir, include_ignored)
        .await
}

#[tauri::command]
pub async fn read_file(
    state: State<'_, AppState>,
    host_id: String,
    worktree: WorktreeId,
    path: String,
    max_bytes: u64,
) -> Res<civet_proto::FilePreview> {
    state
        .hosts
        .read_file(&host_id, worktree, path, max_bytes)
        .await
}

#[tauri::command]
pub async fn list_all_files(
    state: State<'_, AppState>,
    host_id: String,
    worktree: WorktreeId,
) -> Res<civet_proto::FileList> {
    state.hosts.list_all_files(&host_id, worktree).await
}

#[tauri::command]
pub async fn file_op(
    state: State<'_, AppState>,
    host_id: String,
    worktree: WorktreeId,
    op: civet_proto::FileOp,
) -> Res<civet_proto::FileOpResult> {
    state.hosts.file_op(&host_id, worktree, op).await
}

#[tauri::command]
pub async fn upload_to_worktree(
    state: State<'_, AppState>,
    host_id: String,
    worktree: WorktreeId,
    dir: String,
    paths: Vec<String>,
) -> Res<Vec<crate::hosts::DroppedFile>> {
    state
        .hosts
        .upload_to_worktree(&host_id, worktree, dir, paths)
        .await
}

#[tauri::command]
pub async fn download_file(
    state: State<'_, AppState>,
    host_id: String,
    worktree: WorktreeId,
    path: String,
    dest: String,
) -> Res<u64> {
    state
        .hosts
        .download_file(&host_id, worktree, path, dest)
        .await
}

#[tauri::command]
pub async fn list_panes(state: State<'_, AppState>, host_id: String) -> Res<Vec<PaneInfo>> {
    state.hosts.list_panes(&host_id).await
}

#[tauri::command]
pub async fn spawn_pane(
    state: State<'_, AppState>,
    host_id: String,
    request: SpawnPaneRequest,
) -> Res<PaneInfo> {
    state.hosts.spawn_pane(&host_id, request).await
}

#[tauri::command]
pub async fn launch_agent(
    state: State<'_, AppState>,
    host_id: String,
    request: LaunchAgentRequest,
) -> Res<LaunchedAgent> {
    state.hosts.launch_agent(&host_id, request).await
}

#[tauri::command]
pub async fn attach_pane(
    state: State<'_, AppState>,
    host_id: String,
    pane: PaneId,
    on_output: Channel<InvokeResponseBody>,
) -> Res<SubscribeInfo> {
    state
        .hosts
        .attach_pane(&host_id, pane, Arc::new(ChannelSink(on_output)))
        .await
}

#[tauri::command]
pub async fn detach_pane(state: State<'_, AppState>, host_id: String, pane: PaneId) -> Res<()> {
    state.hosts.detach_pane(&host_id, pane)
}

#[tauri::command]
pub async fn write_input(
    state: State<'_, AppState>,
    host_id: String,
    pane: PaneId,
    data: String,
) -> Res<()> {
    state
        .hosts
        .write_input(&host_id, pane, data.into_bytes())
        .map(|_| ())
}

#[tauri::command]
pub async fn write_input_binary(
    state: State<'_, AppState>,
    host_id: String,
    pane: PaneId,
    data: String,
) -> Res<()> {
    state
        .hosts
        .write_input(&host_id, pane, binary_string_bytes(&data))
        .map(|_| ())
}

#[tauri::command]
pub async fn resize_pane(
    state: State<'_, AppState>,
    host_id: String,
    pane: PaneId,
    size: TermSize,
) -> Res<()> {
    state.hosts.resize_pane(&host_id, pane, size).await
}

#[tauri::command]
pub async fn kill_pane(
    state: State<'_, AppState>,
    host_id: String,
    pane: PaneId,
    signal: Option<KillSignal>,
) -> Res<()> {
    state.hosts.kill_pane(&host_id, pane, signal).await
}

#[tauri::command]
pub async fn fetch_scrollback(
    state: State<'_, AppState>,
    host_id: String,
    pane: PaneId,
    query: ScrollbackQuery,
) -> Res<ScrollbackPageView> {
    state.hosts.fetch_scrollback(&host_id, pane, query).await
}

#[tauri::command]
pub async fn set_agent_status(
    state: State<'_, AppState>,
    host_id: String,
    pane: PaneId,
    status: AgentStatus,
) -> Res<()> {
    state.hosts.set_agent_status(&host_id, pane, status).await
}

#[tauri::command]
pub async fn set_prediction(state: State<'_, AppState>, enabled: bool) -> Res<()> {
    state.hosts.set_prediction(enabled);
    Ok(())
}

#[tauri::command]
pub async fn list_ports(state: State<'_, AppState>, host_id: String) -> Res<PortsEvent> {
    state.hosts.list_ports(&host_id)
}

#[tauri::command]
pub async fn forward_port(
    state: State<'_, AppState>,
    host_id: String,
    remote_port: u16,
    local_port: Option<u16>,
) -> Res<ForwardView> {
    state
        .hosts
        .forward_port(&host_id, remote_port, local_port)
        .await
}

#[tauri::command]
pub async fn stop_forward(
    state: State<'_, AppState>,
    host_id: String,
    remote_port: u16,
) -> Res<()> {
    state.hosts.stop_forward(&host_id, remote_port)
}

#[tauri::command]
pub async fn list_forwards(state: State<'_, AppState>, host_id: String) -> Res<Vec<ForwardView>> {
    state.hosts.list_forwards(&host_id)
}

/// Open a forwarded port in the default browser. Only
/// `http(s)://localhost` / `127.0.0.1` URLs: the UI never needs anything
/// else, and nothing else should be openable from the webview.
#[tauri::command]
pub async fn open_local_url(url: String) -> Res<()> {
    if !is_local_url(&url) {
        return Err(CmdError::new(
            "invalidArgument",
            "only http://localhost URLs can be opened",
        ));
    }
    open_in_browser(&url).await
}

/// Open a pull request (sidebar PR icon) in the default browser. Only
/// `https://<host>/<owner>/<repo>/pull/<number>`: the URL comes from `gh`
/// on the host, so it is checked like any other input.
#[tauri::command]
pub async fn open_pull_request(url: String) -> Res<()> {
    if !is_pull_request_url(&url) {
        return Err(CmdError::new("invalidArgument", "not a pull request URL"));
    }
    open_in_browser(&url).await
}

/// Open a link clicked in a Markdown preview in the default browser. Only
/// `http(s)://` URLs: never `file://`, app schemes or anything `open` would
/// launch locally.
#[tauri::command]
pub async fn open_web_url(url: String) -> Res<()> {
    if !is_web_url(&url) {
        return Err(CmdError::new(
            "invalidArgument",
            "only http(s) URLs can be opened",
        ));
    }
    open_in_browser(&url).await
}

async fn open_in_browser(url: &str) -> Res<()> {
    let status = tokio::process::Command::new("/usr/bin/open")
        .arg(url)
        .status()
        .await
        .map_err(|e| CmdError::new("io", format!("open: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(CmdError::new("io", format!("open {url} failed ({status})")))
    }
}

/// `https://<host>/<owner>/<repo>/pull/<number>` (github.com or an
/// Enterprise host), nothing after the number but an optional `/`.
pub fn is_pull_request_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let ok_part = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    let parts: Vec<&str> = rest.trim_end_matches('/').split('/').collect();
    matches!(
        parts.as_slice(),
        [host, owner, repo, "pull", number]
            if ok_part(host)
                && ok_part(owner)
                && ok_part(repo)
                && !number.is_empty()
                && number.chars().all(|c| c.is_ascii_digit())
    )
}

/// `http(s)://<host>...` with a non-empty host and no whitespace or control
/// characters.
pub fn is_web_url(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    !authority.is_empty()
        && !authority.starts_with('-')
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// `http(s)://localhost[:port][/...]` or the same with `127.0.0.1`.
pub fn is_local_url(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    matches!(host, "localhost" | "127.0.0.1")
        && port.is_none_or(|p| !p.is_empty() && p.parse::<u16>().is_ok())
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Put `text` on the system clipboard (Cmd+C on an xterm selection).
/// Synchronous on purpose: Tauri runs sync commands on the main thread,
/// where AppKit's pasteboard wants to be used. Write only.
#[tauri::command]
pub fn write_clipboard(text: String) -> Res<()> {
    if crate::clipboard::write_text(&text) {
        Ok(())
    } else {
        Err(CmdError::new("clipboard", "could not write the clipboard"))
    }
}

/// The clipboard's plain text ("" if none), for Paste as File. Sync for
/// the main thread, like `write_clipboard`.
#[tauri::command]
pub fn read_clipboard() -> String {
    crate::clipboard::read_text().unwrap_or_default()
}

/// The invoke handler with every command.
pub fn handler<R: tauri::Runtime>() -> impl Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static
{
    tauri::generate_handler![
        connect_host,
        reconnect_host,
        restart_daemon,
        disconnect_host,
        list_hosts,
        notify_network_change,
        daemon_info,
        ping,
        list_repos,
        add_repo,
        remove_repo,
        list_dirs,
        host_stats,
        prepare_file_drop,
        upload_file,
        list_worktrees,
        list_pull_requests,
        pull_request_detail,
        create_pull_request,
        git_status,
        git_diff,
        agent_activity,
        turn_diff,
        git_branch,
        git_action,
        worktree_lifecycle,
        update_worktree,
        create_worktree,
        worktree_template,
        set_worktree_template,
        setup_worktree,
        remove_worktree,
        list_files,
        read_file,
        list_all_files,
        file_op,
        upload_to_worktree,
        download_file,
        list_panes,
        spawn_pane,
        launch_agent,
        attach_pane,
        detach_pane,
        write_input,
        write_input_binary,
        resize_pane,
        kill_pane,
        fetch_scrollback,
        set_agent_status,
        set_prediction,
        write_clipboard,
        read_clipboard,
        list_ports,
        forward_port,
        stop_forward,
        list_forwards,
        open_local_url,
        open_pull_request,
        open_web_url,
        crate::updater::app_info,
        crate::updater::check_for_updates,
        crate::updater::update_status,
        crate::updater::install_update,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_strings_map_char_codes_to_bytes() {
        assert_eq!(
            binary_string_bytes("\u{0}a\u{ff}\u{80}"),
            vec![0, b'a', 0xff, 0x80]
        );
        assert_eq!(binary_string_bytes("\u{100}"), vec![b'?']);
    }

    #[test]
    fn only_web_urls_open() {
        assert!(is_web_url("https://example.com"));
        assert!(is_web_url("http://example.com/a?b#c"));
        assert!(!is_web_url("https://"));
        assert!(!is_web_url("https:///etc/passwd"));
        assert!(!is_web_url("file:///etc/passwd"));
        assert!(!is_web_url("vscode://file/x"));
        assert!(!is_web_url("https://example.com/a b"));
        assert!(!is_web_url("https://example.com/\n"));
        assert!(!is_web_url("HTTPS://example.com"));
    }

    #[test]
    fn only_local_urls_open() {
        assert!(is_local_url("http://localhost:5173"));
        assert!(is_local_url("http://localhost:5173/a?b#c"));
        assert!(is_local_url("https://127.0.0.1:8443/"));
        assert!(is_local_url("http://localhost"));
        assert!(!is_local_url("http://localhost.evil.com:80"));
        assert!(!is_local_url("http://localhost:/"));
        assert!(!is_local_url("http://localhost:99999"));
        assert!(!is_local_url("http://user@localhost:80"));
        assert!(!is_local_url("file:///etc/passwd"));
        assert!(!is_local_url("http://example.com"));
        assert!(!is_local_url("http://localhost:80/ -a Terminal"));
    }

    #[test]
    fn pull_request_urls() {
        assert!(is_pull_request_url("https://github.com/o/r/pull/5"));
        assert!(is_pull_request_url(
            "https://ghe.corp.example/my-org/my.repo/pull/12/"
        ));
        assert!(!is_pull_request_url("http://github.com/o/r/pull/5"));
        assert!(!is_pull_request_url("https://github.com/o/r/issues/5"));
        assert!(!is_pull_request_url("https://github.com/o/r/pull/5/files"));
        assert!(!is_pull_request_url("https://user@github.com/o/r/pull/5"));
        assert!(!is_pull_request_url("https://github.com/o/r/pull/5?x=1"));
        assert!(!is_pull_request_url("https://github.com/o/r/pull/"));
        assert!(!is_pull_request_url(
            "https://github.com/o/r/pull/5 -a Terminal"
        ));
        assert!(!is_pull_request_url("file:///o/r/pull/5"));
    }
}
