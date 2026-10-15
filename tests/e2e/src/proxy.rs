//! In-test UDP impairment proxy, placed between a QUIC client and the daemon.
//!
//! ```text
//!  client ──UDP──► [client-facing socket]  UdpProxy  [daemon-facing socket] ──UDP──► civetd
//!                   (fixed: the client dials it)       (swappable: NAT rebinding)
//! ```
//!
//! Every datagram, in each direction, independently gets a fate from the
//! current [`Impairment`]: dropped (loss or blackhole) or delivered after
//! `delay + uniform(0..jitter)` (plus `reorder_extra` with probability
//! `reorder`, so later packets overtake it). Delivery is scheduled on a
//! per-direction timer heap, so independent delays reorder naturally.
//!
//! Roaming is simulated two ways:
//!
//! * the client rebinds its own UDP socket (`ClientSession::notify_network_change`):
//!   the proxy follows the client's newest source address automatically;
//! * [`UdpProxy::rebind_upstream`] swaps the proxy's daemon-facing socket, so
//!   the daemon sees the client at a new source port (NAT rebinding / a new
//!   network). The old port is closed, like a NAT mapping that is gone.
//!
//! The impairment can be changed at any time with [`UdpProxy::set`] /
//! [`UdpProxy::update`]; packets already scheduled are still delivered.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// Network conditions applied to each direction independently.
#[derive(Debug, Clone, PartialEq)]
pub struct Impairment {
    /// Probability (0..=1) that a datagram is dropped.
    pub loss: f64,
    /// One-way base delay (RTT = 2 × delay).
    pub delay: Duration,
    /// Extra uniform random delay in `0..jitter`.
    pub jitter: Duration,
    /// Probability that a datagram is held back by `reorder_extra`.
    pub reorder: f64,
    /// Extra delay for reordered datagrams.
    pub reorder_extra: Duration,
    /// Drop everything (laptop asleep, Wi-Fi gone, UDP blocked).
    pub blackhole: bool,
}

impl Default for Impairment {
    fn default() -> Self {
        Self {
            loss: 0.0,
            delay: Duration::ZERO,
            jitter: Duration::ZERO,
            reorder: 0.0,
            reorder_extra: Duration::from_millis(15),
            blackhole: false,
        }
    }
}

impl Impairment {
    /// Clean link with the given round-trip time.
    pub fn rtt(rtt: Duration) -> Self {
        Self {
            delay: rtt / 2,
            ..Self::default()
        }
    }

    /// Loss + RTT, with 10% jitter and 2% reordering (a lossy Wi-Fi link).
    pub fn lossy(loss: f64, rtt: Duration) -> Self {
        Self {
            loss,
            delay: rtt / 2,
            jitter: rtt / 10,
            reorder: 0.02,
            ..Self::default()
        }
    }
}

/// Counters (monotonic).
#[derive(Debug, Default)]
pub struct ProxyStats {
    /// Datagrams client → daemon received / dropped.
    pub up_packets: AtomicU64,
    /// Dropped client → daemon.
    pub up_dropped: AtomicU64,
    /// Datagrams daemon → client received.
    pub down_packets: AtomicU64,
    /// Dropped daemon → client.
    pub down_dropped: AtomicU64,
    /// Times the client's source address changed (client-side roaming).
    pub client_addr_changes: AtomicU64,
    /// Times the daemon-facing socket was swapped (NAT rebinding).
    pub upstream_rebinds: AtomicU64,
}

impl ProxyStats {
    /// Snapshot as plain numbers (up, up_dropped, down, down_dropped).
    pub fn totals(&self) -> (u64, u64, u64, u64) {
        (
            self.up_packets.load(Ordering::Relaxed),
            self.up_dropped.load(Ordering::Relaxed),
            self.down_packets.load(Ordering::Relaxed),
            self.down_dropped.load(Ordering::Relaxed),
        )
    }
}

#[derive(Debug, Clone, Copy)]
enum Dir {
    Up,
    Down,
}

struct Shared {
    imp: Mutex<Impairment>,
    rng: Mutex<StdRng>,
    client_addr: Mutex<Option<SocketAddr>>,
    upstream: Mutex<Arc<UdpSocket>>,
    upstream_gen: watch::Sender<u64>,
    stats: ProxyStats,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Shared {
    /// None = drop; Some(delay) = deliver after delay.
    fn fate(&self, dir: Dir) -> Option<Duration> {
        let imp = lock(&self.imp).clone();
        let (count, dropped) = match dir {
            Dir::Up => (&self.stats.up_packets, &self.stats.up_dropped),
            Dir::Down => (&self.stats.down_packets, &self.stats.down_dropped),
        };
        count.fetch_add(1, Ordering::Relaxed);
        let mut rng = lock(&self.rng);
        if imp.blackhole || (imp.loss > 0.0 && rng.random_bool(imp.loss.clamp(0.0, 1.0))) {
            dropped.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let mut d = imp.delay;
        if !imp.jitter.is_zero() {
            d += imp.jitter.mul_f64(rng.random::<f64>());
        }
        if imp.reorder > 0.0 && rng.random_bool(imp.reorder.clamp(0.0, 1.0)) {
            d += imp.reorder_extra;
        }
        Some(d)
    }
}

struct Delayed {
    at: Instant,
    seq: u64,
    data: Vec<u8>,
}

impl PartialEq for Delayed {
    fn eq(&self, o: &Self) -> bool {
        (self.at, self.seq) == (o.at, o.seq)
    }
}
impl Eq for Delayed {}
impl PartialOrd for Delayed {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Delayed {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        (self.at, self.seq).cmp(&(o.at, o.seq))
    }
}

/// The proxy. Dropping it stops all its tasks and closes its sockets.
pub struct UdpProxy {
    listen: SocketAddr,
    target: SocketAddr,
    shared: Arc<Shared>,
    tasks: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for UdpProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UdpProxy")
            .field("listen", &self.listen)
            .field("target", &self.target)
            .field("impairment", &*lock(&self.shared.imp))
            .finish()
    }
}

impl Drop for UdpProxy {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

impl UdpProxy {
    /// Listen on 127.0.0.1:0 and forward to `target`. The RNG seed comes
    /// from `CIVET_E2E_SEED` (default 42) so lossy runs are reproducible.
    pub async fn start(target: SocketAddr, imp: Impairment) -> std::io::Result<Self> {
        let seed = std::env::var("CIVET_E2E_SEED")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(42u64);
        let client_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);
        let upstream = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);
        let listen = client_sock.local_addr()?;
        let (gen_tx, gen_rx) = watch::channel(0u64);
        let shared = Arc::new(Shared {
            imp: Mutex::new(imp),
            rng: Mutex::new(StdRng::seed_from_u64(seed)),
            client_addr: Mutex::new(None),
            upstream: Mutex::new(upstream),
            upstream_gen: gen_tx,
            stats: ProxyStats::default(),
        });
        let (up_tx, up_rx) = mpsc::unbounded_channel();
        let (down_tx, down_rx) = mpsc::unbounded_channel();
        let tasks = vec![
            tokio::spawn(client_reader(shared.clone(), client_sock.clone(), up_tx)),
            tokio::spawn(upstream_reader(shared.clone(), gen_rx, down_tx)),
            tokio::spawn(scheduler(up_rx, {
                let shared = shared.clone();
                move |data| {
                    let sock = lock(&shared.upstream).clone();
                    async move {
                        let _ = sock.send_to(&data, target).await;
                    }
                }
            })),
            tokio::spawn(scheduler(down_rx, {
                let shared = shared.clone();
                let client_sock = client_sock.clone();
                move |data| {
                    let to = *lock(&shared.client_addr);
                    let sock = client_sock.clone();
                    async move {
                        if let Some(to) = to {
                            let _ = sock.send_to(&data, to).await;
                        }
                    }
                }
            })),
        ];
        Ok(Self {
            listen,
            target,
            shared,
            tasks,
        })
    }

    /// Address the client should dial.
    pub fn addr(&self) -> SocketAddr {
        self.listen
    }

    /// Current impairment.
    pub fn impairment(&self) -> Impairment {
        lock(&self.shared.imp).clone()
    }

    /// Replace the impairment.
    pub fn set(&self, imp: Impairment) {
        *lock(&self.shared.imp) = imp;
    }

    /// Change the impairment in place.
    pub fn update(&self, f: impl FnOnce(&mut Impairment)) {
        f(&mut lock(&self.shared.imp));
    }

    /// Drop all traffic (or stop doing so).
    pub fn blackhole(&self, on: bool) {
        self.update(|i| i.blackhole = on);
    }

    /// NAT rebinding: swap the daemon-facing socket for a new one (new
    /// source port as seen by the daemon) and close the old one. Returns the
    /// new daemon-facing address.
    pub fn rebind_upstream(&self) -> std::io::Result<SocketAddr> {
        let std_sock = std::net::UdpSocket::bind("127.0.0.1:0")?;
        std_sock.set_nonblocking(true)?;
        let sock = Arc::new(UdpSocket::from_std(std_sock)?);
        let addr = sock.local_addr()?;
        *lock(&self.shared.upstream) = sock;
        self.shared.upstream_gen.send_modify(|g| *g += 1);
        self.shared
            .stats
            .upstream_rebinds
            .fetch_add(1, Ordering::Relaxed);
        Ok(addr)
    }

    /// The daemon-facing (source) address the daemon currently sees.
    pub fn upstream_addr(&self) -> std::io::Result<SocketAddr> {
        lock(&self.shared.upstream).local_addr()
    }

    /// Last client source address seen.
    pub fn client_addr(&self) -> Option<SocketAddr> {
        *lock(&self.shared.client_addr)
    }

    /// Counters.
    pub fn stats(&self) -> &ProxyStats {
        &self.shared.stats
    }
}

async fn client_reader(
    shared: Arc<Shared>,
    sock: Arc<UdpSocket>,
    out: mpsc::UnboundedSender<(Duration, Vec<u8>)>,
) {
    let mut buf = vec![0u8; 65_536];
    loop {
        let Ok((n, from)) = sock.recv_from(&mut buf).await else {
            // e.g. ICMP-induced errors on some platforms: keep going.
            tokio::time::sleep(Duration::from_millis(1)).await;
            continue;
        };
        {
            let mut c = lock(&shared.client_addr);
            if *c != Some(from) {
                if c.is_some() {
                    shared
                        .stats
                        .client_addr_changes
                        .fetch_add(1, Ordering::Relaxed);
                }
                *c = Some(from);
            }
        }
        if let Some(d) = shared.fate(Dir::Up)
            && out.send((d, buf[..n].to_vec())).is_err()
        {
            return;
        }
    }
}

async fn upstream_reader(
    shared: Arc<Shared>,
    mut gen_rx: watch::Receiver<u64>,
    out: mpsc::UnboundedSender<(Duration, Vec<u8>)>,
) {
    let mut buf = vec![0u8; 65_536];
    loop {
        gen_rx.borrow_and_update();
        let sock = lock(&shared.upstream).clone();
        loop {
            tokio::select! {
                r = sock.recv_from(&mut buf) => {
                    let Ok((n, _)) = r else {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                        continue;
                    };
                    if let Some(d) = shared.fate(Dir::Down)
                        && out.send((d, buf[..n].to_vec())).is_err()
                    {
                        return;
                    }
                }
                r = gen_rx.changed() => {
                    if r.is_err() {
                        return;
                    }
                    break; // socket swapped: the old one is dropped here
                }
            }
        }
    }
}

async fn scheduler<F, Fut>(mut rx: mpsc::UnboundedReceiver<(Duration, Vec<u8>)>, send: F)
where
    F: Fn(Vec<u8>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let mut heap: BinaryHeap<Reverse<Delayed>> = BinaryHeap::new();
    let mut seq = 0u64;
    loop {
        let next = heap.peek().map(|Reverse(d)| d.at);
        tokio::select! {
            m = rx.recv() => match m {
                Some((delay, data)) => {
                    seq += 1;
                    heap.push(Reverse(Delayed { at: Instant::now() + delay, seq, data }));
                }
                None => return,
            },
            () = sleep_until_opt(next) => {
                let now = Instant::now();
                while heap.peek().is_some_and(|Reverse(d)| d.at <= now) {
                    let Reverse(d) = heap.pop().expect("peeked");
                    send(d.data).await;
                }
            }
        }
    }
}

async fn sleep_until_opt(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Echo server + proxy: delay, loss accounting, blackhole and upstream
    /// rebinding behave as advertised.
    #[tokio::test]
    async fn proxy_basics() {
        let echo = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let echo_addr = echo.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::<SocketAddr>::new()));
        let seen2 = seen.clone();
        let echo_task = tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            loop {
                let (n, from) = echo.recv_from(&mut buf).await.unwrap();
                seen2.lock().unwrap().push(from);
                echo.send_to(&buf[..n], from).await.unwrap();
            }
        });
        let proxy = UdpProxy::start(echo_addr, Impairment::rtt(Duration::from_millis(40)))
            .await
            .unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut buf = [0u8; 2048];

        let t0 = Instant::now();
        client.send_to(b"ping", proxy.addr()).await.unwrap();
        let (n, _) = tokio::time::timeout(Duration::from_secs(2), client.recv_from(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf[..n], b"ping");
        assert!(
            t0.elapsed() >= Duration::from_millis(40),
            "{:?}",
            t0.elapsed()
        );

        // Blackhole: nothing comes back.
        proxy.blackhole(true);
        client.send_to(b"lost", proxy.addr()).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(200), client.recv_from(&mut buf))
                .await
                .is_err()
        );
        proxy.blackhole(false);

        // NAT rebinding: the echo server sees a new source port.
        let before = proxy.upstream_addr().unwrap();
        let after = proxy.rebind_upstream().unwrap();
        assert_ne!(before, after);
        client.send_to(b"moved", proxy.addr()).await.unwrap();
        let (n, _) = tokio::time::timeout(Duration::from_secs(2), client.recv_from(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf[..n], b"moved");
        assert_eq!(*seen.lock().unwrap().last().unwrap(), after);

        // 50% loss over many packets is roughly 50%.
        proxy.set(Impairment {
            loss: 0.5,
            ..Impairment::default()
        });
        let (up0, upd0, _, _) = proxy.stats().totals();
        for _ in 0..400 {
            client.send_to(b"x", proxy.addr()).await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (up1, upd1, _, _) = proxy.stats().totals();
        let ratio = (upd1 - upd0) as f64 / (up1 - up0) as f64;
        assert!((0.4..0.6).contains(&ratio), "loss ratio {ratio}");
        echo_task.abort();
    }
}
