//! UI-facing JSON types (commands and `listen` events, docs/INTERFACES.md
//! §6.1 / §6.3) and the [`UiEvents`] sink the core reports through.

use civet_proto::{AgentStatus, Event, PaneId};
use civet_transport::TransportKind;
use serde::{Deserialize, Serialize};

/// `civet://host-state`
pub const EV_HOST_STATE: &str = "civet://host-state";
/// `civet://bootstrap-progress`
pub const EV_BOOTSTRAP_PROGRESS: &str = "civet://bootstrap-progress";
/// `civet://daemon-event`
pub const EV_DAEMON_EVENT: &str = "civet://daemon-event";
/// `civet://agent-attention`
pub const EV_AGENT_ATTENTION: &str = "civet://agent-attention";

/// What `connect_host` connects to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum HostTarget {
    /// A remote host over SSH.
    Ssh {
        /// `host`, `user@host` or an ssh_config alias.
        destination: String,
        /// SSH port.
        #[serde(default)]
        port: Option<u16>,
        /// Extra ssh arguments.
        #[serde(default)]
        extra_args: Vec<String>,
    },
    /// A daemon on this machine.
    Local,
}

impl HostTarget {
    /// Stable host id: `"local"` or the trimmed ssh destination.
    pub fn host_id(&self) -> String {
        match self {
            Self::Ssh { destination, .. } => destination.trim().to_string(),
            Self::Local => "local".to_string(),
        }
    }

    /// Sidebar label.
    pub fn label(&self) -> String {
        match self {
            Self::Ssh { destination, .. } => destination.trim().to_string(),
            Self::Local => "This Mac".to_string(),
        }
    }
}

/// `transport` argument of `connect_host`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TransportChoice {
    /// QUIC, falling back to the SSH stdio tunnel when UDP is blocked.
    #[default]
    Auto,
    /// QUIC only.
    Quic,
    /// Always the SSH stdio tunnel (or the unix socket for a local host).
    Ssh,
}

/// Host connection state shown by the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HostState {
    /// First connect (bootstrap + dial) in progress.
    Connecting,
    /// Live.
    Connected,
    /// Recovery has taken more than 1 s.
    Reconnecting,
    /// Closed by the user.
    Disconnected,
    /// Gave up; needs a user-initiated reconnect.
    Failed,
}

/// Returned by `connect_host` / `list_hosts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostSummary {
    /// `"local"` or the ssh destination.
    pub host_id: String,
    /// Display name.
    pub label: String,
    /// Connection state.
    pub state: HostState,
    /// Current link kind.
    pub transport: Option<TransportKind>,
    /// Running daemon's version.
    pub daemon_version: Option<String>,
    /// Daemon instance id (u64 as a decimal string).
    pub instance_id: Option<String>,
}

/// `civet://host-state`
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostStateEvent {
    /// Host.
    pub host_id: String,
    /// New state.
    pub state: HostState,
    /// Link kind, if connected.
    pub transport: Option<TransportKind>,
    /// Latest RTT.
    pub rtt_ms: Option<u32>,
    /// Why (failures, reconnect reason).
    pub message: Option<String>,
}

/// `civet://bootstrap-progress` steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProgressStep {
    /// Probing the remote (uname, installed version).
    Probe,
    /// Uploading civetd.
    Upload,
    /// Starting / finding the daemon.
    Start,
    /// Resolving the QUIC address.
    Resolve,
    /// Dialing QUIC.
    Dial,
    /// UDP blocked: using the SSH stdio tunnel.
    Fallback,
}

/// `civet://bootstrap-progress`
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapProgressEvent {
    /// Host.
    pub host_id: String,
    /// Step.
    pub step: ProgressStep,
    /// Detail.
    pub message: Option<String>,
}

/// `civet://daemon-event`
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonEventPayload {
    /// Host.
    pub host_id: String,
    /// Proto event (JSON form).
    pub event: Event,
}

/// `civet://agent-attention`
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAttentionEvent {
    /// Host.
    pub host_id: String,
    /// Pane.
    pub pane: PaneId,
    /// `done` or `needsInput`.
    pub status: AgentStatus,
    /// Status before.
    pub previous: AgentStatus,
    /// Pane title (or worktree name).
    pub title: String,
    /// Worktree id (path), if any.
    pub worktree: Option<String>,
    /// Detail from the hook / OSC notification.
    pub message: Option<String>,
}

/// `fetch_scrollback` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrollbackPageView {
    /// Absolute number of `lines[0]`.
    pub start: u64,
    /// Oldest line the daemon still has.
    pub oldest_available: u64,
    /// One past the newest line.
    pub end_available: u64,
    /// VT text per line (UTF-8 decoded).
    pub lines: Vec<String>,
    /// Soft-wrap flag per line.
    pub wrapped: Vec<bool>,
}

/// `ping` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PingResult {
    /// Round trip of a control-stream ping.
    pub rtt_ms: u32,
}

/// `launch_agent` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchedAgent {
    /// The new worktree.
    pub worktree: civet_proto::WorktreeInfo,
    /// The agent's pane.
    pub pane: civet_proto::PaneInfo,
}

/// Where the core reports to the UI (the Tauri app emits events and shows
/// native notifications; tests record).
pub trait UiEvents: Send + Sync + 'static {
    /// `civet://host-state`
    fn host_state(&self, e: HostStateEvent);
    /// `civet://bootstrap-progress`
    fn bootstrap_progress(&self, e: BootstrapProgressEvent);
    /// `civet://daemon-event`
    fn daemon_event(&self, e: DaemonEventPayload);
    /// `civet://agent-attention`
    fn agent_attention(&self, e: AgentAttentionEvent);
    /// Is any app window focused (no native notification then)?
    fn window_focused(&self) -> bool;
    /// Show a native notification.
    fn notify(&self, title: &str, body: &str);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_target_json_matches_the_ui() {
        let t: HostTarget = serde_json::from_str(
            r#"{"kind":"ssh","destination":" dev@box ","port":2222,"extraArgs":["-J","b"]}"#,
        )
        .unwrap();
        assert_eq!(
            t,
            HostTarget::Ssh {
                destination: " dev@box ".into(),
                port: Some(2222),
                extra_args: vec!["-J".into(), "b".into()]
            }
        );
        assert_eq!(t.host_id(), "dev@box");
        let t: HostTarget = serde_json::from_str(r#"{"kind":"ssh","destination":"h"}"#).unwrap();
        assert!(matches!(t, HostTarget::Ssh { port: None, .. }));
        let t: HostTarget = serde_json::from_str(r#"{"kind":"local"}"#).unwrap();
        assert_eq!(t.host_id(), "local");
        let c: TransportChoice = serde_json::from_str(r#""ssh""#).unwrap();
        assert_eq!(c, TransportChoice::Ssh);
    }

    #[test]
    fn summary_json_shape() {
        let s = HostSummary {
            host_id: "local".into(),
            label: "This Mac".into(),
            state: HostState::Reconnecting,
            transport: Some(TransportKind::Stream),
            daemon_version: Some("0.1.0".into()),
            instance_id: Some(u64::MAX.to_string()),
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["hostId"], "local");
        assert_eq!(v["state"], "reconnecting");
        assert_eq!(v["transport"], "stream");
        assert_eq!(v["instanceId"], "18446744073709551615");
    }
}
