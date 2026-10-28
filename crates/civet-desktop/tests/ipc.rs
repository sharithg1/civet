//! The real Tauri command layer through the IPC boundary (mock runtime, the
//! app's own tauri.conf.json and capability/ACL): every command accepts the
//! exact JSON argument names `ui/src/backend.ts` sends, rejects with
//! `{ code, message }`, and is permitted for the main window.

use std::sync::{Arc, Mutex};

use civet_desktop::commands::{AppState, handler};
use civet_desktop::error::CmdError;
use civet_desktop::events::{
    AgentAttentionEvent, BootstrapProgressEvent, DaemonEventPayload, HostStateEvent, HostTarget,
    TransportChoice, UiEvents,
};
use civet_desktop::hosts::{Connector, Established, HostManager, ManagerConfig, Progress};
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{INVOKE_KEY, get_ipc_response, mock_builder};
use tauri::webview::InvokeRequest;

struct NullUi;

impl UiEvents for NullUi {
    fn host_state(&self, _: HostStateEvent) {}
    fn bootstrap_progress(&self, _: BootstrapProgressEvent) {}
    fn daemon_event(&self, _: DaemonEventPayload) {}
    fn agent_attention(&self, _: AgentAttentionEvent) {}
    fn window_focused(&self) -> bool {
        true
    }
    fn notify(&self, _: &str, _: &str) {}
}

/// Every connect fails like an unreachable ssh host; records what it was
/// asked to connect to.
#[derive(Default)]
struct Unreachable {
    calls: Mutex<Vec<(HostTarget, TransportChoice, bool)>>,
}

impl Connector for Unreachable {
    fn connect<'a>(
        &'a self,
        target: &'a HostTarget,
        choice: TransportChoice,
        replace: bool,
        _: Progress,
    ) -> BoxFuture<'a, Result<Established, CmdError>> {
        self.calls
            .lock()
            .unwrap()
            .push((target.clone(), choice, replace));
        Box::pin(async {
            Err(CmdError::bootstrap(
                "ssh failed (exit Some(255)): unreachable",
            ))
        })
    }
}

fn invoke(
    webview: &tauri::WebviewWindow<tauri::test::MockRuntime>,
    cmd: &str,
    args: Value,
) -> Result<Value, Value> {
    let req = InvokeRequest {
        cmd: cmd.into(),
        callback: CallbackFn(0),
        error: CallbackFn(1),
        url: "tauri://localhost".parse().unwrap(),
        body: InvokeBody::Json(args),
        headers: Default::default(),
        invoke_key: INVOKE_KEY.to_string(),
    };
    get_ipc_response(webview, req).map(|b| b.deserialize::<Value>().unwrap())
}

#[test]
fn every_command_through_ipc() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    tauri::async_runtime::set(rt.handle().clone());
    let connector = Arc::new(Unreachable::default());
    let hosts = HostManager::new(
        ManagerConfig::default(),
        Arc::new(NullUi),
        connector.clone(),
        rt.handle().clone(),
    );
    let app = mock_builder()
        .manage(AppState { hosts })
        .invoke_handler(handler())
        .build(civet_desktop::context())
        .expect("app");
    let w = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();

    assert_eq!(invoke(&w, "list_hosts", json!({})), Ok(json!([])));
    assert_eq!(
        invoke(&w, "notify_network_change", json!({})),
        Ok(Value::Null)
    );
    assert_eq!(
        invoke(&w, "set_prediction", json!({ "enabled": false })),
        Ok(Value::Null)
    );

    // connect_host: bootstrap failure → { code: "bootstrap" }, host listed as failed.
    let err = invoke(
        &w,
        "connect_host",
        json!({ "target": { "kind": "ssh", "destination": "box", "port": null, "extraArgs": [] }, "transport": "auto" }),
    )
    .unwrap_err();
    assert_eq!(err["code"], "bootstrap", "{err}");
    assert!(err["message"].as_str().unwrap().contains("unreachable"));
    let hosts = invoke(&w, "list_hosts", json!({})).unwrap();
    assert_eq!(hosts[0]["hostId"], "box");
    assert_eq!(hosts[0]["state"], "failed");
    assert_eq!(hosts[0]["upgrade"], Value::Null);

    // reconnect_host / restart_daemon reuse the stored target (port and
    // extra ssh args) and transport choice; restart passes replace.
    let target = json!({ "kind": "ssh", "destination": "opts", "port": 2222, "extraArgs": ["-J", "bastion"] });
    let _ = invoke(
        &w,
        "connect_host",
        json!({ "target": target, "transport": "ssh" }),
    );
    let err = invoke(&w, "reconnect_host", json!({ "hostId": "opts" })).unwrap_err();
    assert_eq!(err["code"], "bootstrap", "{err}");
    let err = invoke(&w, "restart_daemon", json!({ "hostId": "opts" })).unwrap_err();
    assert_eq!(err["code"], "bootstrap", "{err}");
    let expected = HostTarget::Ssh {
        destination: "opts".into(),
        port: Some(2222),
        extra_args: vec!["-J".into(), "bastion".into()],
    };
    {
        let calls = connector.calls.lock().unwrap();
        assert_eq!(calls.len(), 4, "{calls:?}");
        assert_eq!(calls[2], (expected.clone(), TransportChoice::Ssh, false));
        assert_eq!(calls[3], (expected, TransportChoice::Ssh, true));
    }
    let err = invoke(&w, "reconnect_host", json!({ "hostId": "nope" })).unwrap_err();
    assert_eq!(err["code"], "noSuchHost", "{err}");
    let err = invoke(&w, "restart_daemon", json!({ "hostId": "nope" })).unwrap_err();
    assert_eq!(err["code"], "noSuchHost", "{err}");

    // Every host command with the UI's argument names: the arguments
    // deserialize (else Tauri rejects with a string, not { code }) and the
    // unknown host is reported as noSuchHost.
    let size = json!({ "cols": 80, "rows": 24, "pixelWidth": 0, "pixelHeight": 0 });
    let spawn = json!({
        "worktree": null, "cwd": null, "command": { "program": "sh", "args": [] },
        "env": [["A", "1"]], "size": size, "title": null, "agent": null
    });
    let launch = json!({
        "repo": "r", "branch": "b", "base": null, "worktreePath": null,
        "command": { "program": "claude", "args": [] }, "env": [], "size": size,
        "title": null, "agent": "claude"
    });
    let calls: Vec<(&str, Value)> = vec![
        ("disconnect_host", json!({ "hostId": "x" })),
        ("daemon_info", json!({ "hostId": "x" })),
        ("ping", json!({ "hostId": "x" })),
        ("list_repos", json!({ "hostId": "x" })),
        ("add_repo", json!({ "hostId": "x", "path": "/r" })),
        ("remove_repo", json!({ "hostId": "x", "repo": "r" })),
        ("list_dirs", json!({ "hostId": "x", "path": "~/src" })),
        (
            "host_stats",
            json!({ "hostId": "x", "since": null, "processes": true }),
        ),
        ("list_worktrees", json!({ "hostId": "x", "repo": "r" })),
        ("worktree_template", json!({ "hostId": "x", "repo": "r" })),
        (
            "set_worktree_template",
            json!({ "hostId": "x", "repo": "r", "template": { "setup": null, "copy": [".env"], "agent": null, "prompt": null } }),
        ),
        (
            "setup_worktree",
            json!({ "hostId": "x", "worktree": "/w", "size": size }),
        ),
        ("list_pull_requests", json!({ "hostId": "x", "repo": "r" })),
        (
            "pull_request_detail",
            json!({ "hostId": "x", "worktree": "/w", "threads": true }),
        ),
        (
            "create_pull_request",
            json!({ "hostId": "x", "request": { "worktree": "/w", "title": "t", "body": "", "base": null, "draft": false } }),
        ),
        ("git_status", json!({ "hostId": "x", "worktree": "/w" })),
        (
            "git_diff",
            json!({ "hostId": "x", "worktree": "/w", "path": "a", "oldPath": null, "staged": false, "contextLines": 3 }),
        ),
        ("git_branch", json!({ "hostId": "x", "worktree": "/w" })),
        (
            "git_action",
            json!({ "hostId": "x", "worktree": "/w", "action": { "discard": { "paths": ["a"], "staged": false } } }),
        ),
        (
            "worktree_lifecycle",
            json!({ "hostId": "x", "repo": "r", "fetch": true, "review": false }),
        ),
        (
            "update_worktree",
            json!({ "hostId": "x", "worktree": "/w", "strategy": "rebase" }),
        ),
        (
            "create_worktree",
            json!({ "hostId": "x", "repo": "r", "branch": "b", "base": null, "path": null }),
        ),
        (
            "remove_worktree",
            json!({ "hostId": "x", "worktree": "/w", "force": true }),
        ),
        (
            "list_files",
            json!({ "hostId": "x", "worktree": "/w", "dir": "", "includeIgnored": false }),
        ),
        (
            "read_file",
            json!({ "hostId": "x", "worktree": "/w", "path": "a", "maxBytes": 1024 }),
        ),
        ("list_all_files", json!({ "hostId": "x", "worktree": "/w" })),
        (
            "file_op",
            json!({ "hostId": "x", "worktree": "/w", "op": { "move": { "from": "a", "to": "b/a" } } }),
        ),
        (
            "file_op",
            json!({ "hostId": "x", "worktree": "/w", "op": { "restore": { "trashId": "1-2-3" } } }),
        ),
        (
            "upload_to_worktree",
            json!({ "hostId": "x", "worktree": "/w", "dir": "src", "paths": ["/tmp/a.png"] }),
        ),
        (
            "download_file",
            json!({ "hostId": "x", "worktree": "/w", "path": "a", "dest": "/tmp/a" }),
        ),
        ("list_panes", json!({ "hostId": "x" })),
        ("spawn_pane", json!({ "hostId": "x", "request": spawn })),
        ("launch_agent", json!({ "hostId": "x", "request": launch })),
        (
            "attach_pane",
            json!({ "hostId": "x", "pane": 1, "onOutput": "__CHANNEL__:7" }),
        ),
        ("detach_pane", json!({ "hostId": "x", "pane": 1 })),
        (
            "write_input",
            json!({ "hostId": "x", "pane": 1, "data": "ls\r" }),
        ),
        (
            "write_input_binary",
            json!({ "hostId": "x", "pane": 1, "data": "\u{1b}[M !!" }),
        ),
        (
            "resize_pane",
            json!({ "hostId": "x", "pane": 1, "size": size }),
        ),
        ("kill_pane", json!({ "hostId": "x", "pane": 1 })),
        (
            "kill_pane",
            json!({ "hostId": "x", "pane": 1, "signal": "terminate" }),
        ),
        (
            "fetch_scrollback",
            json!({ "hostId": "x", "pane": 1, "query": { "before": { "end": null, "limit": 200 } } }),
        ),
        (
            "fetch_scrollback",
            json!({ "hostId": "x", "pane": 1, "query": { "range": { "start": 0, "end": 10 } } }),
        ),
        (
            "set_agent_status",
            json!({ "hostId": "x", "pane": 1, "status": "needsInput" }),
        ),
    ];
    for (cmd, args) in calls {
        let err = invoke(&w, cmd, args).unwrap_err();
        assert_eq!(err["code"], "noSuchHost", "{cmd}: {err}");
    }
    // A malformed argument is rejected by Tauri itself (sanity check that
    // the loop above really exercised deserialization).
    let err = invoke(&w, "kill_pane", json!({ "hostId": "x", "pane": "one" })).unwrap_err();
    assert!(err.is_string(), "{err}");
    drop(app);
    rt.shutdown_background();
}
