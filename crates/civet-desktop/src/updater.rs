//! In-app updates (`tauri-plugin-updater`) and the About window.
//!
//! Release builds check `latest.json` on the latest GitHub Release
//! (`plugins.updater.endpoints` in tauri.conf.json) shortly after launch and
//! then every few hours. A newer, correctly signed build is downloaded in the
//! background and kept in memory; the UI then offers "Restart to update",
//! which installs it (swaps the `.app` bundle) and relaunches. Daemons and
//! their panes keep running across the restart, as on any quit.
//!
//! Every state change is emitted as [`EV_UPDATE`] to all windows.

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::menu::{Menu, MenuItem, MenuItemKind};
use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::Mutex;

use crate::error::CmdError;

/// Update state changes ([`UpdateEvent`]).
pub const EV_UPDATE: &str = "civet://update";

/// Label of the About window (capabilities/about.json).
pub const ABOUT_WINDOW: &str = "about";

const MENU_ABOUT: &str = "civet-about";
const MENU_CHECK: &str = "civet-check-updates";

/// First automatic check after launch, then the interval between checks.
const FIRST_CHECK: Duration = Duration::from_secs(20);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// Where an update check stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdatePhase {
    Checking,
    UpToDate,
    Downloading,
    /// Downloaded and verified: install with `install_update`.
    Ready,
    Error,
}

/// Payload of [`EV_UPDATE`] and the result of `check_for_updates`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateEvent {
    pub phase: UpdatePhase,
    /// This app's version.
    pub current: String,
    /// The available version (downloading / ready).
    pub version: Option<String>,
    /// Started by the user (menu, About window), not the periodic check.
    pub manual: bool,
    /// The error (phase `error`).
    pub message: Option<String>,
}

/// Managed state: the downloaded update and the last event.
#[derive(Default)]
pub struct Updates {
    /// Held for a whole check + download, so checks never overlap.
    inner: Mutex<Inner>,
    /// The last event, for windows opened after it (`update_status`).
    last: std::sync::Mutex<Option<UpdateEvent>>,
}

#[derive(Default)]
struct Inner {
    ready: Option<(Update, Vec<u8>)>,
}

impl std::fmt::Debug for Updates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Updates")
            .field("last", &self.last())
            .finish_non_exhaustive()
    }
}

impl Updates {
    fn last(&self) -> Option<UpdateEvent> {
        self.last.lock().ok().and_then(|l| l.clone())
    }
}

fn emit<R: Runtime>(app: &AppHandle<R>, e: UpdateEvent) -> UpdateEvent {
    if let Some(state) = app.try_state::<Arc<Updates>>()
        && let Ok(mut last) = state.last.lock()
    {
        *last = Some(e.clone());
    }
    if let Err(err) = app.emit(EV_UPDATE, e.clone()) {
        tracing::warn!(event = EV_UPDATE, error = %err, "emit failed");
    }
    e
}

/// Check for a newer release and download it. Returns (and emits) the
/// final state. A download already done is not repeated.
pub async fn check_and_download<R: Runtime>(app: &AppHandle<R>, manual: bool) -> UpdateEvent {
    let current = app.package_info().version.to_string();
    let event = |phase, version: Option<String>, message: Option<String>| UpdateEvent {
        phase,
        current: current.clone(),
        version,
        manual,
        message,
    };
    let Some(state) = app.try_state::<Arc<Updates>>().map(|s| s.inner().clone()) else {
        return event(
            UpdatePhase::Error,
            None,
            Some("updater not initialised".into()),
        );
    };
    let mut inner = state.inner.lock().await;
    if let Some((update, _)) = &inner.ready {
        return emit(
            app,
            event(UpdatePhase::Ready, Some(update.version.clone()), None),
        );
    }
    emit(app, event(UpdatePhase::Checking, None, None));
    let result = async {
        let Some(update) = app.updater()?.check().await? else {
            return Ok(None);
        };
        tracing::info!(version = %update.version, "update available; downloading");
        emit(
            app,
            event(UpdatePhase::Downloading, Some(update.version.clone()), None),
        );
        // Verified against the configured public key before it is returned.
        let bytes = update.download(|_, _| {}, || {}).await?;
        Ok::<_, tauri_plugin_updater::Error>(Some((update, bytes)))
    }
    .await;
    let done = match result {
        Ok(None) => event(UpdatePhase::UpToDate, None, None),
        Ok(Some((update, bytes))) => {
            let version = update.version.clone();
            tracing::info!(%version, "update downloaded");
            inner.ready = Some((update, bytes));
            event(UpdatePhase::Ready, Some(version), None)
        }
        Err(e) => {
            tracing::warn!(error = %e, manual, "update check failed");
            event(UpdatePhase::Error, None, Some(e.to_string()))
        }
    };
    emit(app, done)
}

/// Install the downloaded update and relaunch into it.
async fn install_and_restart<R: Runtime>(app: &AppHandle<R>) -> Result<(), CmdError> {
    let state = app
        .try_state::<Arc<Updates>>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| CmdError::new("update", "updater not initialised"))?;
    // Held through the install so no check runs meanwhile. The download
    // stays in `ready` until the restart, so a failed install can be retried.
    let inner = state.inner.lock().await;
    let (update, bytes) = inner
        .ready
        .clone()
        .ok_or_else(|| CmdError::new("update", "no update has been downloaded"))?;
    tracing::info!(version = %update.version, "installing update");
    // Replaces the .app bundle on disk (blocking file work).
    tauri::async_runtime::spawn_blocking(move || update.install(bytes))
        .await
        .map_err(|e| CmdError::new("update", e.to_string()))?
        .map_err(|e| CmdError::new("update", format!("install failed: {e}")))?;
    app.restart()
}

/// Periodic checks (release builds only; `CIVET_NO_UPDATE_CHECK=1` turns them off).
pub fn spawn_periodic_checks<R: Runtime>(app: AppHandle<R>) {
    if cfg!(debug_assertions) || std::env::var_os("CIVET_NO_UPDATE_CHECK").is_some() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK).await;
        loop {
            check_and_download(&app, false).await;
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

/// The default menu, with "About Civet" (our window instead of the native
/// panel) and "Check for Updates…" at the top of the app menu.
pub fn menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let menu = Menu::default(app)?;
    if let Some(MenuItemKind::Submenu(app_menu)) = menu.items()?.first() {
        app_menu.remove_at(0)?;
        let about = MenuItem::with_id(app, MENU_ABOUT, "About Civet", true, None::<&str>)?;
        let check = MenuItem::with_id(app, MENU_CHECK, "Check for Updates…", true, None::<&str>)?;
        app_menu.insert(&about, 0)?;
        app_menu.insert(&check, 1)?;
    }
    Ok(menu)
}

/// Menu clicks.
pub fn on_menu_event<R: Runtime>(app: &AppHandle<R>, id: &str) {
    match id {
        MENU_ABOUT => open_about(app),
        MENU_CHECK => {
            open_about(app);
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                check_and_download(&app, true).await;
            });
        }
        _ => {}
    }
}

/// Show (or focus) the About window: the same UI bundle, which renders
/// the About view for this window label.
pub fn open_about<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window(ABOUT_WINDOW) {
        let _ = w.unminimize();
        let _ = w.set_focus();
        return;
    }
    let built = WebviewWindowBuilder::new(app, ABOUT_WINDOW, WebviewUrl::default())
        .title("About Civet")
        .inner_size(400.0, 330.0)
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        .center()
        .background_color(tauri::window::Color(0x15, 0x16, 0x16, 0xff))
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true)
        .build();
    if let Err(e) = built {
        tracing::warn!(error = %e, "could not open the About window");
    }
}

/// Version details for the About window and "Copy version info".
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub version: String,
    /// `aarch64` / `x86_64`.
    pub arch: &'static str,
    /// The commit the app was built from (empty if unknown).
    pub commit: &'static str,
    /// That commit's date, RFC 3339 (empty if unknown).
    pub commit_date: &'static str,
    /// e.g. `macOS 15.4.1`.
    pub os: String,
}

fn os_version() -> String {
    std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| format!("macOS {}", String::from_utf8_lossy(&o.stdout).trim()))
        .unwrap_or_else(|| std::env::consts::OS.to_string())
}

#[tauri::command]
pub async fn app_info<R: Runtime>(app: AppHandle<R>) -> Result<AppInfo, CmdError> {
    Ok(AppInfo {
        version: app.package_info().version.to_string(),
        arch: std::env::consts::ARCH,
        commit: env!("CIVET_BUILD_COMMIT"),
        commit_date: env!("CIVET_BUILD_DATE"),
        os: os_version(),
    })
}

/// Check now (and download a newer version).
#[tauri::command]
pub async fn check_for_updates<R: Runtime>(app: AppHandle<R>) -> Result<UpdateEvent, CmdError> {
    Ok(check_and_download(&app, true).await)
}

/// The last update event, if any check ran.
#[tauri::command]
pub async fn update_status<R: Runtime>(app: AppHandle<R>) -> Result<Option<UpdateEvent>, CmdError> {
    Ok(app.try_state::<Arc<Updates>>().and_then(|s| s.last()))
}

/// Install the downloaded update and relaunch (does not return on success).
#[tauri::command]
pub async fn install_update<R: Runtime>(app: AppHandle<R>) -> Result<(), CmdError> {
    install_and_restart(&app).await
}
