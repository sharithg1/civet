//! Tauri commands (docs/INTERFACES.md §6.1). Thin wrappers over
//! [`HostManager`]; every one is `async` so none runs on the main thread,
//! except `write_clipboard`, which must (AppKit).
//! Keep the list in sync with `build.rs` (ACL) and the capability file.

use civet_proto::{
    AgentStatus, KillSignal, LaunchAgentRequest, PaneId, PaneInfo, RepoId, RepoInfo,
    ScrollbackQuery, SpawnPaneRequest, SubscribeInfo, TermSize, WorktreeId, WorktreeInfo,
};
use std::sync::Arc;
use tauri::State;
use tauri::ipc::{Channel, InvokeResponseBody};

use crate::batch::RecordSink;
use crate::error::CmdError;
use crate::events::{
    DaemonInfoView, HostSummary, HostTarget, LaunchedAgent, PingResult, ScrollbackPageView,
    TransportChoice,
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
pub async fn list_repos(state: State<'_, AppState>, host_id: String) -> Res<Vec<RepoInfo>> {
    state.hosts.list_repos(&host_id).await
}

#[tauri::command]
pub async fn add_repo(state: State<'_, AppState>, host_id: String, path: String) -> Res<RepoInfo> {
    state.hosts.add_repo(&host_id, path).await
}

#[tauri::command]
pub async fn remove_repo(state: State<'_, AppState>, host_id: String, repo: RepoId) -> Res<()> {
    state.hosts.remove_repo(&host_id, repo).await
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
        list_worktrees,
        create_worktree,
        remove_worktree,
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
}
