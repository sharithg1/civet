//! Sleep/wake and network-change detection (docs/INTERFACES.md §6.4).
//!
//! A light background thread compares wall-clock and monotonic time every
//! [`TICK`]: on macOS the monotonic clock stops while the machine sleeps, so
//! a wall-clock jump larger than [`JUMP_THRESHOLD`] means "just woke up".
//! Every [`ROUTE_EVERY`] ticks it also looks at the local addresses the OS
//! would route from (a UDP `connect` sends no packets), which changes on a
//! Wi-Fi switch, VPN up/down or cable plug. Either calls the callback, which
//! calls `notify_network_change()` on every session: rebind + ping, so the
//! next packet is the reconnect. The UI adds `online` / `visibilitychange`
//! hints through the `notify_network_change` command.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant, SystemTime};

/// Poll interval (also bounds the delay between wake and the reconnect).
pub const TICK: Duration = Duration::from_millis(250);
/// Wall-clock jump that counts as a sleep.
pub const JUMP_THRESHOLD: Duration = Duration::from_secs(2);
/// Check the routed local addresses every this many ticks (2 s).
pub const ROUTE_EVERY: u32 = 8;

/// Why the detector fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeReason {
    /// Wall clock jumped relative to the monotonic clock.
    ClockJump,
    /// The local address used to reach the internet changed.
    RouteChanged,
}

/// Pure wall-vs-monotonic comparison.
#[derive(Debug, Clone)]
pub struct ClockJump {
    wall: SystemTime,
    mono: Instant,
    threshold: Duration,
}

impl ClockJump {
    /// Start at the given clock readings.
    pub fn new(wall: SystemTime, mono: Instant, threshold: Duration) -> Self {
        Self {
            wall,
            mono,
            threshold,
        }
    }

    /// Feed new readings; true if the wall clock moved by more than the
    /// threshold beyond the monotonic clock (either direction).
    pub fn check(&mut self, wall: SystemTime, mono: Instant) -> bool {
        let dm = mono.saturating_duration_since(self.mono);
        let jumped = match wall.duration_since(self.wall) {
            Ok(dw) => dw > dm + self.threshold,
            Err(back) => back.duration() > self.threshold,
        };
        self.wall = wall;
        self.mono = mono;
        jumped
    }
}

/// Local source addresses for the default IPv4 and IPv6 routes (no packets
/// are sent). Empty when offline.
pub fn routed_local_addrs() -> Vec<IpAddr> {
    let probe = |bind: IpAddr, target: SocketAddr| -> Option<IpAddr> {
        let s = UdpSocket::bind(SocketAddr::new(bind, 0)).ok()?;
        s.connect(target).ok()?;
        Some(s.local_addr().ok()?.ip())
    };
    [
        probe(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)), 53),
        ),
        probe(
            IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            SocketAddr::new(
                IpAddr::V6(Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111)),
                53,
            ),
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Run the detector on a background thread for the life of the process.
pub fn spawn(on_change: impl Fn(WakeReason) + Send + 'static) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("civet-wake".into())
        .spawn(move || {
            let mut clock = ClockJump::new(SystemTime::now(), Instant::now(), JUMP_THRESHOLD);
            let mut routes = routed_local_addrs();
            let mut tick = 0u32;
            loop {
                std::thread::sleep(TICK);
                tick = tick.wrapping_add(1);
                if clock.check(SystemTime::now(), Instant::now()) {
                    tracing::info!("wake detected (clock jump)");
                    routes = routed_local_addrs();
                    on_change(WakeReason::ClockJump);
                    continue;
                }
                if tick % ROUTE_EVERY == 0 {
                    let now = routed_local_addrs();
                    if now != routes {
                        tracing::info!(?routes, ?now, "network change detected");
                        routes = now;
                        on_change(WakeReason::RouteChanged);
                    }
                }
            }
        })
        .map(|_| ())
}

/// Also call `on_wake` from the OS wake notification where available
/// (macOS: `NSWorkspaceDidWakeNotification`, delivered on the main thread
/// right after wake, before the next poll tick). Call on the main thread
/// once the app is running; the observer lives for the process.
pub fn observe_os_wake(on_wake: impl Fn() + Send + Sync + 'static) {
    #[cfg(target_os = "macos")]
    macos::observe_did_wake(on_wake);
    #[cfg(not(target_os = "macos"))]
    drop(on_wake);
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos {
    use std::ptr::NonNull;

    use block2::RcBlock;
    use objc2_app_kit::{NSWorkspace, NSWorkspaceDidWakeNotification};
    use objc2_foundation::NSNotification;

    pub(super) fn observe_did_wake(on_wake: impl Fn() + Send + Sync + 'static) {
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        let block = RcBlock::new(move |_note: NonNull<NSNotification>| on_wake());
        // SAFETY: `NSWorkspaceDidWakeNotification` is an immutable NSString
        // constant exported by AppKit, which is loaded (Tauri runs an
        // NSApplication). `obj` and `queue` are nil, so the block runs
        // synchronously on the posting (main) thread; it only calls a
        // `Send + Sync` closure, so it is sendable as the method requires.
        let token = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(NSWorkspaceDidWakeNotification),
                None,
                None,
                &block,
            )
        };
        // Observe for the life of the process (never removed).
        std::mem::forget(token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_forward_and_backward_jumps_only() {
        let w0 = SystemTime::now();
        let m0 = Instant::now();
        let mut c = ClockJump::new(w0, m0, JUMP_THRESHOLD);
        // Normal tick: both clocks advance together.
        assert!(!c.check(
            w0 + Duration::from_millis(250),
            m0 + Duration::from_millis(250)
        ));
        // Small drift (NTP slew) is not a wake.
        assert!(!c.check(
            w0 + Duration::from_millis(1700),
            m0 + Duration::from_millis(500)
        ));
        // Slept 30 s: monotonic advanced 250 ms, wall 30 s.
        assert!(c.check(
            w0 + Duration::from_secs(32),
            m0 + Duration::from_millis(750)
        ));
        assert!(!c.check(
            w0 + Duration::from_millis(32_250),
            m0 + Duration::from_secs(1)
        ));
        // Wall clock set back by a minute.
        assert!(c.check(
            w0 - Duration::from_secs(30),
            m0 + Duration::from_millis(1250)
        ));
    }

    #[test]
    fn routed_addrs_are_not_unspecified() {
        for ip in routed_local_addrs() {
            assert!(!ip.is_unspecified(), "{ip}");
        }
    }
}
