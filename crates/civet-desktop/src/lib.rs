//! Civet desktop app, Tauri Rust side (docs/INTERFACES.md §6).
//!
//! * [`hosts`]: `HostManager`, one `ClientSession` per host, the per-host
//!   event task, and the logic behind every command.
//! * [`commands`]: the `#[tauri::command]` wrappers.
//! * [`pane`] / [`batch`] / [`records`]: per-pane mirror, binary channel
//!   records and 4 ms / 256 KiB batching.
//! * [`predict`]: local-echo prediction.
//! * [`linkstate`]: `LinkState` → host state ("reconnecting" after 1 s).
//! * [`wake`]: sleep/wake and network-change detection.
//! * [`connect`]: ssh bootstrap, QUIC with stdio-tunnel fallback, local mode.
//!
//! The React frontend lives in `ui/`.

pub mod batch;
pub mod clipboard;
pub mod commands;
pub mod connect;
pub mod error;
pub mod events;
pub mod hosts;
pub mod linkstate;
pub mod pane;
pub mod predict;
pub mod records;
pub mod wake;

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, RunEvent};
use tauri_plugin_notification::NotificationExt;

use crate::commands::AppState;
use crate::connect::SshConnector;
use crate::events::{
    AgentAttentionEvent, BootstrapProgressEvent, ClipboardEvent, ClipboardWrite,
    DaemonEventPayload, EV_AGENT_ATTENTION, EV_BOOTSTRAP_PROGRESS, EV_CLIPBOARD, EV_DAEMON_EVENT,
    EV_HOST_STATE, HostStateEvent, UiEvents,
};
use crate::hosts::{HostManager, ManagerConfig};

/// Emits Tauri events and native notifications.
struct TauriUi<R: tauri::Runtime> {
    app: AppHandle<R>,
}

impl<R: tauri::Runtime> TauriUi<R> {
    fn emit<T: Serialize + Clone>(&self, name: &str, payload: T) {
        if let Err(e) = self.app.emit(name, payload) {
            tracing::warn!(event = name, error = %e, "emit failed");
        }
    }
}

impl<R: tauri::Runtime> UiEvents for TauriUi<R> {
    fn host_state(&self, e: HostStateEvent) {
        self.emit(EV_HOST_STATE, e);
    }

    fn bootstrap_progress(&self, e: BootstrapProgressEvent) {
        self.emit(EV_BOOTSTRAP_PROGRESS, e);
    }

    fn daemon_event(&self, e: DaemonEventPayload) {
        self.emit(EV_DAEMON_EVENT, e);
    }

    fn agent_attention(&self, e: AgentAttentionEvent) {
        self.emit(EV_AGENT_ATTENTION, e);
    }

    fn window_focused(&self) -> bool {
        self.app
            .webview_windows()
            .values()
            .any(|w| w.is_focused().unwrap_or(false))
    }

    fn clipboard(&self, write: ClipboardWrite) {
        // AppKit's pasteboard belongs on the main thread.
        let app = self.app.clone();
        let res = self.app.run_on_main_thread(move || {
            let ok = clipboard::write_text(&write.text);
            tracing::info!(
                host = %write.host_id,
                pane = %write.pane,
                len = write.text.len(),
                ok,
                "OSC 52 clipboard write"
            );
            let e = ClipboardEvent {
                host_id: write.host_id,
                pane: write.pane,
                chars: write.text.chars().count(),
                ok,
            };
            if let Err(err) = app.emit(EV_CLIPBOARD, e) {
                tracing::warn!(event = EV_CLIPBOARD, error = %err, "emit failed");
            }
        });
        if let Err(e) = res {
            tracing::warn!(error = %e, "clipboard write not scheduled");
        }
    }

    fn notify(&self, title: &str, body: &str) {
        if let Err(e) = self
            .app
            .notification()
            .builder()
            .title(title)
            .body(body)
            .show()
        {
            tracing::warn!(error = %e, "notification failed");
        }
    }
}

/// The app context (config, assets, ACL) generated from `tauri.conf.json`.
/// One expansion per binary: the IPC tests use it with the mock runtime.
pub fn context<R: tauri::Runtime>() -> tauri::Context<R> {
    tauri::generate_context!()
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("CIVET_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

/// Run the app (blocks until exit).
///
/// # Panics
/// If the tokio runtime cannot be created or Tauri fails to start.
pub fn run() {
    init_logging();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("civet-rt")
        .build()
        .expect("tokio runtime");
    let handle = rt.handle().clone();
    // Commands and our background tasks share one runtime.
    tauri::async_runtime::set(handle.clone());

    // Its own id (not civet-cli's): two clients sharing an id kick each
    // other off the daemon (REPLACED) in a reconnect storm.
    let client_id = civetd::paths::load_or_create_client_id_file(
        &civetd::paths::default_state_dir(),
        civetd::paths::DESKTOP_CLIENT_ID_FILE,
    )
    .unwrap_or_else(|e| {
        tracing::warn!(error = %e, "client id not persisted; using an ephemeral one");
        civet_proto::ClientId::generate().expect("random client id")
    });

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let ui = Arc::new(TauriUi {
                app: app.handle().clone(),
            });
            // Bundled daemons (`<Resources>/civet-dist`, see tauri.conf.json):
            // Linux musl builds for remotes and this Mac's civetd for Local.
            // In dev the dir is empty and the search falls through to
            // $CIVET_DIST_DIR, ~/.civet/dist and the workspace civetd.
            let resource_dir = app.path().resource_dir().ok();
            let connector = Arc::new(SshConnector::new(client_id, resource_dir.as_deref()));
            tracing::info!(
                bundled = ?connector.bundled_dist,
                "daemon binaries: bundled dir, then $CIVET_DIST_DIR, ~/.civet/dist"
            );
            let hosts = HostManager::new(ManagerConfig::default(), ui, connector, handle.clone());
            let wake_hosts = hosts.clone();
            wake::spawn(move |reason| {
                tracing::debug!(?reason, "notify_network_change");
                wake_hosts.notify_network_change();
            })?;
            let os_wake_hosts = hosts.clone();
            wake::observe_os_wake(move || {
                tracing::info!("wake notification from the OS");
                os_wake_hosts.notify_network_change();
            });
            app.manage(AppState { hosts });
            Ok(())
        })
        .on_page_load(|webview, payload| {
            // A (re)loading webview has no live channels: keep the views
            // cached for an instant re-attach.
            if payload.event() == tauri::webview::PageLoadEvent::Started
                && let Some(state) = webview.try_state::<AppState>()
            {
                state.hosts.detach_all();
            }
        })
        .invoke_handler(commands::handler())
        .build(context())
        .expect("error while building the Civet app");

    app.run(move |app, event| {
        if let RunEvent::Exit = event
            && let Some(state) = app.try_state::<AppState>()
        {
            let hosts = state.hosts.clone();
            tauri::async_runtime::block_on(async move { hosts.shutdown().await });
        }
    });
    drop(rt);
}
