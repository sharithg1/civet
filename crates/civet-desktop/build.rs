//! Tauri build step: generates the app context and the ACL permissions
//! (`allow-<command>`) for every custom command, which
//! `capabilities/default.json` grants to the main window.

/// Must list every `#[tauri::command]` registered in `src/commands.rs`.
const COMMANDS: &[&str] = &[
    "connect_host",
    "disconnect_host",
    "list_hosts",
    "notify_network_change",
    "daemon_info",
    "ping",
    "list_repos",
    "add_repo",
    "remove_repo",
    "list_worktrees",
    "create_worktree",
    "remove_worktree",
    "list_panes",
    "spawn_pane",
    "launch_agent",
    "attach_pane",
    "detach_pane",
    "write_input",
    "write_input_binary",
    "resize_pane",
    "kill_pane",
    "fetch_scrollback",
    "set_agent_status",
    "set_prediction",
];

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("tauri-build failed");
}
