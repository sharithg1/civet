//! Tauri build step: generates the app context and the ACL permissions
//! (`allow-<command>`) for every custom command, which
//! `capabilities/default.json` grants to the main window.

/// Must list every `#[tauri::command]` registered in `src/commands.rs`.
const COMMANDS: &[&str] = &[
    "connect_host",
    "reconnect_host",
    "restart_daemon",
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
    "write_clipboard",
];

/// Bundled daemon binaries (`bundle.resources` in tauri.conf.json):
/// `civetd-<triple>` for Linux remotes and this Mac, written by
/// `scripts/build-remote.sh --bundle` (the `beforeBuildCommand` of `tauri
/// build`). Created empty here so `cargo build` / `tauri dev` work without
/// it (dev falls back to `$CIVET_DIST_DIR`, `~/.civet/dist` and the
/// workspace `civetd`).
const BUNDLED_DIST: &str = "civet-dist";

fn main() {
    let dir = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"))
        .join(BUNDLED_DIST);
    std::fs::create_dir_all(&dir).expect("creating the bundled civet-dist dir");
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("tauri-build failed");
}
