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
    "prepare_file_drop",
    "upload_file",
    "list_worktrees",
    "list_pull_requests",
    "git_status",
    "git_diff",
    "git_branch",
    "git_action",
    "create_worktree",
    "remove_worktree",
    "list_files",
    "read_file",
    "list_all_files",
    "file_op",
    "upload_to_worktree",
    "download_file",
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
    "list_ports",
    "forward_port",
    "stop_forward",
    "list_forwards",
    "open_local_url",
    "open_pull_request",
    "open_web_url",
    // src/updater.rs
    "app_info",
    "check_for_updates",
    "update_status",
    "install_update",
];

/// Bundled daemon binaries (`bundle.resources` in tauri.conf.json):
/// `civetd-<triple>` for Linux remotes and this Mac, written by
/// `scripts/build-remote.sh --bundle` (the `beforeBuildCommand` of `tauri
/// build`). Created empty here so `cargo build` / `tauri dev` work without
/// it (dev falls back to `$CIVET_DIST_DIR`, `~/.civet/dist` and the
/// workspace `civetd`).
const BUNDLED_DIST: &str = "civet-dist";

/// `git` output, trimmed; None on any failure (no git, not a checkout).
fn git(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).output().ok()?;
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !s.is_empty()).then_some(s)
}

/// The commit (and its date) shown in the About window: `$CIVET_COMMIT`
/// (release CI sets it, so a cached build script reruns per commit), else
/// git's HEAD.
fn build_commit() {
    println!("cargo:rerun-if-env-changed=CIVET_COMMIT");
    let commit = std::env::var("CIVET_COMMIT")
        .ok()
        .filter(|c| !c.is_empty())
        .or_else(|| git(&["rev-parse", "HEAD"]))
        .unwrap_or_default();
    let date = if commit.is_empty() {
        String::new()
    } else {
        git(&["show", "-s", "--format=%cI", &commit]).unwrap_or_default()
    };
    println!("cargo:rustc-env=CIVET_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=CIVET_BUILD_DATE={date}");
}

fn main() {
    build_commit();
    let dir = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"))
        .join(BUNDLED_DIST);
    std::fs::create_dir_all(&dir).expect("creating the bundled civet-dist dir");
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("tauri-build failed");
}
