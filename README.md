# Civet

Civet is a remote-first workspace for coding agents. A Rust daemon,
`civetd`, runs on your Linux dev box. It owns repos, git worktrees, the
PTYs of every shell and agent pane, and each pane's terminal screen and
scrollback. A Tauri desktop app on your Mac shows those panes in xterm.js.
The client talks to the daemon over a mosh-style transport on QUIC: SSH is
used once to start the daemon and exchange a session key, then the server
sends screen diffs, the client numbers its keystrokes, and the session
survives sleep, Wi-Fi changes and client restarts without a visible
reconnect. Civet is a port of the remote-agent workflow of Orca (the
Electron app; plan: `../orca-tauri-mvp-plan.md`, written for "Orca Remote" /
`orcad-rs`), with the transport model changed so that waking up is the next
packet, not a new TCP + SSH + WebSocket handshake.

## Architecture

* [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): why it is shaped this way,
  state sync, reconnect cases, scrollback, prediction, failure modes.
* [docs/INTERFACES.md](docs/INTERFACES.md): the binding contract between
  the crates (wire types, APIs, constants, on-disk layout, env vars).
* [crates/civet-term/SPIKE.md](crates/civet-term/SPIKE.md): terminal crate
  choice. [crates/civet-session/HOOKS.md](crates/civet-session/HOOKS.md):
  agent status hooks.

```
 Mac: civet-desktop (Tauri + React + xterm.js)        Linux host: civetd
      ClientSession (resume, input seq, stall)  ══QUIC/UDP══►  SessionManager
      wake detector, prediction                                PTYs ─► PaneTerminal (alacritty_terminal)
            └── ssh (bootstrap; stdio-tunnel fallback) ──────►  diff → VT, scrollback store
```

## Crates

| Crate | Path | Role |
|---|---|---|
| `civet-proto` | `crates/civet-proto` | Wire types (postcard frames, JSON for the UI), bootstrap line, handshake, constants. No I/O. |
| `civet-transport` | `crates/civet-transport` | QUIC (quinn, pinned self-signed cert, session-key hello, migration), the same API over any byte stream, and the client's resumable `ClientSession`. |
| `civet-term` | `crates/civet-term` | Server-side terminal per pane (alacritty_terminal), snapshots, snapshot → VT diff encoder, absolute-numbered scrollback. |
| `civet-session` | `crates/civet-session` | PTYs, repos/worktrees via the git CLI, input dedup, per-client subscriptions with mosh-style flow control, agent status. |
| `civetd` | `crates/civetd` | The daemon (`serve`, `bootstrap`, `stdio-tunnel`, `version`), the client bootstrap library (ssh, install, stdio tunnel) and `civet-cli`, a test client. |
| `civet-desktop` | `crates/civet-desktop` | Tauri 2 app: commands, binary channels, wake detection, notifications, local-echo prediction; React UI in `ui/`. |
| `civet-e2e` | `tests/e2e` | Hardening harness: UDP impairment proxy, network-change / loss / sleep / restart / fallback / throughput / memory scenarios. |

`civet-desktop` and `civet-e2e` are not default members: `cargo build` /
`cargo test` at the root cover the daemon-side crates only.

## Build and run

Requirements: a stable Rust toolchain (pinned by `rust-toolchain.toml`),
`git`; for the desktop app Node + pnpm; for remote binaries zig +
`cargo-zigbuild`.

### First run

1. Put cargo on your `PATH`. rustup installs it in `~/.cargo/bin` but only
   adds it to the shell rc files it knows about; if `cargo` is not found:

   ```sh
   source ~/.cargo/env                          # this shell only
   echo '. "$HOME/.cargo/env"' >> ~/.zshrc      # every new shell
   ```

2. Build the Linux daemons the app uploads to your dev box (once per
   version; the app refuses a stale file and says so):

   ```sh
   brew install zig && cargo install --locked cargo-zigbuild
   scripts/build-remote.sh --install            # x86_64 + aarch64 musl -> ~/.civet/dist
   ```

3. Run the app:

   ```sh
   cd crates/civet-desktop/ui && pnpm install
   pnpm tauri dev                               # builds the Rust side too
   ```

   Connect "This Mac" (uses the workspace `target/debug/civetd`; build it
   with `cargo build -p civetd` first) or an ssh destination (key or agent
   auth; `~/.ssh/config` aliases work).

A packaged app (`pnpm tauri build`) needs none of steps 1–2 at runtime: it
bundles the daemons (see [Desktop app](#desktop-app)).

### Daemon

```sh
cargo build --release -p civetd                       # target/release/{civetd,civet-cli}
target/release/civetd version
target/release/civetd serve --state-dir /tmp/civet    # foreground, Ctrl-C to stop
target/release/civetd bootstrap --state-dir /tmp/civet  # start detached or reattach; prints the JSON line
```

State lives in `~/.civet` (override with `--state-dir` or
`CIVET_STATE_DIR`). UDP ports 60001–60999 (`CIVET_PORT_RANGE`), logs in
`<state>/logs/civetd.log` when detached (`CIVET_LOG` filter).

Static Linux binaries for remote hosts (`civetd-<triple>` files, the layout
the bootstrap uploads from):

```sh
scripts/build-remote.sh                 # x86_64 + aarch64 musl -> target/civet-dist/
export CIVET_DIST_DIR=$PWD/target/civet-dist
```

### civet-cli

Local daemon (no ssh), new shell pane attached to this terminal
(Ctrl-] detaches):

```sh
scripts/dev-local.sh                    # throwaway state dir, stops the daemon on exit
scripts/dev-local.sh --stdio            # the UDP-blocked fallback path
scripts/dev-local.sh --rebind 2         # migrate the UDP socket every 2 s, prints resume latency
target/release/civet-cli --local connect           # same, against ~/.civet
```

Over ssh (bootstraps the remote: uploads the matching binary if needed,
starts or reattaches the daemon, then switches to QUIC; falls back to an
`ssh … civetd stdio-tunnel` if UDP is blocked):

```sh
civet-cli --ssh devbox connect                     # new shell pane, attached
civet-cli --ssh devbox --dist target/civet-dist connect -- claude
civet-cli --ssh devbox --stdio connect             # force the ssh tunnel
civet-cli --ssh devbox panes                       # also: ping --count 20, events, info
civet-cli --ssh devbox launch REPO my-branch --attach -- claude
civet-cli --ssh devbox scrollback 3 --limit 200
```

Data commands print JSON; see `civet-cli --help` and
[INTERFACES.md §5.4](docs/INTERFACES.md).

### Desktop app

```sh
cd crates/civet-desktop/ui
pnpm install
pnpm tauri dev          # the Tauri app (builds the Rust side too)
pnpm dev                # UI only in a browser, against the in-memory mock backend
pnpm tauri build --bundles app   # release Civet.app with the daemons bundled
```

`tauri build` first runs `scripts/build-remote.sh --bundle`
(`beforeBuildCommand`): `civetd` for x86_64 and aarch64 Linux (musl, via
zig) and for this Mac, into `crates/civet-desktop/civet-dist/`, which is
bundled as `Civet.app/Contents/Resources/civet-dist/`. The app uploads from
there first (then `$CIVET_DIST_DIR`, `~/.civet/dist`) and runs the bundled
Mac `civetd` for "This Mac". `tauri dev` stays fast: it bundles nothing and
uses `~/.civet/dist` plus the workspace `target/debug/civetd`.
`CIVET_SKIP_DAEMONS=1 pnpm tauri build` skips the daemon builds.
`pnpm tauri build --target x86_64-apple-darwin` bundles the x86_64 Mac
`civetd` (the script follows Tauri's `TAURI_ENV_TARGET_TRIPLE`).

Releases (tag, GitHub Release with the app and the daemons, signing):
see [docs/RELEASING.md](docs/RELEASING.md).

**Daemon upgrades.** Every app version uploads its own `civetd-<version>`
next to older ones (never overwriting a running binary). On connect the new
binary's `bootstrap` decides what runs:

* no daemon, or an older daemon with no panes: start / replace it on the new
  version;
* an older, protocol-compatible daemon with live panes: keep it (your
  sessions survive) and report the upgrade. The app shows
  "civetd v0.1.1 running · v0.1.2 available · 3 live panes" above the
  terminal, in the host popover (status bar) and the sidebar, with a
  one-time toast, and offers **Restart daemon (closes 3 panes)**. It asks
  first, then re-bootstraps with `--replace` (`restart_daemon`): the old
  daemon stops (its panes' processes get SIGHUP), the new one starts, and the
  app reconnects. "Later" hides the banner until the next version;
* an incompatible daemon with live panes: the connect fails with
  "incompatible … has live panes" and the banner offers the same restart.

The same applies to "This Mac". `civet-cli --replace` / `civetd bootstrap
--replace` do it by hand.

**Shell integration on the host.** Every pane gets `TERM_PROGRAM=civet`
(so agents do not emit another terminal's private escape codes),
`CIVET_PANE_ID`, `CIVET_STATUS_FILE` and a shim dir first on `PATH` with
`claude` and `codex` wrappers. The wrappers exec the real program with
Civet's status hooks added (`claude --settings <hooks json>`,
`codex -c notify=…`) without touching `~/.claude` or `~/.codex`, so `claude`
typed in any shell pane reports running / needs input / done. zsh panes
start with `ZDOTDIR` pointing at a small shim that restores your `ZDOTDIR`,
sources your `.zshenv` and keeps the shim dir first after your startup
files; bash uses `PROMPT_COMMAND`, fish `-C`. Details and manual wiring:
[crates/civet-session/HOOKS.md](crates/civet-session/HOOKS.md). Only agent
panes (launched as an agent, or reporting through a hook) raise attention
toasts and notifications; a shell exiting does not.

**Settings → General → Terminal → Local echo prediction**: Auto (mosh-style
underlined guesses on links slower than ~30 ms) or Off; stored with the
other preferences.

## Tests

```sh
cargo test                                  # daemon-side crates (fast)
cargo test-core                             # everything except the desktop crate
cargo test -p civet-desktop                 # desktop Rust side; UI: cd crates/civet-desktop/ui && pnpm test
cargo clippy-core                           # lint gate (-D warnings)

cargo test -p civet-e2e                     # hardening scenarios, ~2 min (debug)
scripts/e2e-report.sh                       # all of them incl. long ones, release, metrics table
cargo test -p civet-e2e --release -- --include-ignored --nocapture --test-threads=1
```

The hardening harness (`tests/e2e`) puts an in-test UDP proxy between a
real `ClientSession` and an in-process daemon (or the real `civetd`
binary). The proxy drops X% of datagrams, adds latency + jitter, reorders,
blackholes for a duration, and changes the source port the daemon sees
(NAT rebinding) while the client can rebind its own socket (roaming).
Every daemon runs in a temp state dir and is killed even when a test fails.

| Test (`tests/e2e/tests/`) | Scenario |
|---|---|
| `network_change` | 20 client rebinds + 20 NAT rebindings at 0 and 100 ms RTT; keystroke → echo after each; must migrate, not reconnect |
| `loss_latency` | 5% and 20% loss at 100 ms RTT: typed text arrives exactly once in order, screen converges, history exact. `soak_roaming_under_loss` (ignored): 60 s of typing at 10% loss with a network change every 2 s |
| `sleep` | blackhole 10 s (`CIVET_E2E_SLEEP_SECS`) while a pane prints; time to live after wake; nothing lost |
| `client_restart` | client dies without Goodbye; reconnect with the persisted resume token (Diff from the known state), and as a fresh session (Full + scrollback fetch) |
| `udp_fallback` | UDP blocked: QUIC times out (3 s), real `civetd stdio-tunnel` takes over; tunnel killed → resumed |
| `throughput` | `cat` 5 MB and `yes \| head -n 200000`; ignored: 50 MB and 2 000 000 lines over QUIC and the tunnel |
| `scrollback_gap` | output larger than one replay while away; gap filled with `Range` pages; whole history paged with `Before` |
| `memory` | real daemon RSS: baseline, 10 idle shell panes, then 10 000-line scrollback each |
| `daemon_restart` | SIGKILL / SIGTERM the real daemon under a connected client: `Failed { needs_bootstrap }`, re-bootstrap, new instance id, old panes gone |
| `udp_failover` | UDP blackholed mid-session: the `FailoverDialer` moves the session to the stdio tunnel (resumed, input exactly once); UDP restored: back on QUIC |

Knobs: `CIVET_E2E_RUNS`, `CIVET_E2E_RTTS_MS`, `CIVET_E2E_SLEEP_SECS`,
`CIVET_E2E_SOAK_SECS`, `CIVET_E2E_SEED`, `CIVET_E2E_LOG` (tracing filter),
`CIVET_E2E_METRICS` (JSON-lines output), `CIVETD_BIN` (daemon binary;
otherwise built on first use).

Real-host testing: `scripts/netem.sh` (Linux `tc netem` loss/latency
profiles, UDP blackhole, UDP block for the fallback, NAT-rebinding roaming)
and `scripts/sleep-wake-loop.sh` (macOS `pmset` sleep/wake cycles; read it
first, it really sleeps the Mac).

## Measured metrics vs the plan

Measured 2026-10-14 on an Apple M-series Mac (macOS, loopback + the in-test
impairment proxy), `--release`, `scripts/e2e-report.sh` plus
`CIVET_E2E_SLEEP_SECS=60/300` runs. Latencies are keystroke → echo visible
on the client's mirror of the screen (server echo, no local prediction).

| Plan success metric | Target | Measured | Status |
|---|---|---|---|
| Network back → live, typeable terminal | < 200 ms | Client rebind (wake / Wi-Fi switch, `notify_network_change`), 20 runs: p50 7.3 / p95 9.9 ms at 0 ms RTT; p50 109.7 / p95 112.2 ms at 100 ms RTT (= 1 RTT). After a blackout with output running, at 50 ms RTT: 10 s → 118 ms to echo, 60 s → 116 ms, 300 s → 168 ms. Silent NAT rebinding, 20 runs: p95 9.9 ms at 0 ms RTT, **p95 214.9 ms at 100 ms RTT** (2 RTT, see note 1) | Met for wake / network change; NAT rebinding misses at 100 ms RTT by design of QUIC |
| Keystroke → screen (local echo) | < 10 ms | What is measured: `ClientSession::send_input` → the echoed bytes in the client's mirror of the screen (in-process client, loopback, release), i.e. the server echo with no prediction: p50 9.0 ms at 0 ms RTT. Not measured: xterm.js rendering in the webview, and local-echo prediction (the predicted cells are queued to the pane's channel in the same `write_input` call and flushed by the ≤ 4 ms batcher; on by default above 30 ms RTT, Settings → Local echo prediction) | Server echo met on a LAN-like link; end-to-end on screen not measured |
| Survives Wi-Fi change, lid close/open, client restart: 100% of runs, no lost output | 100% | 40/40 rebinds at each RTT migrated without reconnect; 10 / 60 / 300 s blackouts with output running: resumed in place, every line present once and in order (300 s: 5084 lines printed while away, 3078-line replay gap fetched); client restart (token or fresh): every line present; 60 s soak at 10% loss with 21 network changes (incl. 7 s blackouts, 2 resumed re-dials): typed text exact. 5% / 20% loss at 100 ms RTT: input exactly once and in order, screen converged, history exact. Real macOS sleep (`scripts/sleep-wake-loop.sh`): not run here | Met (simulated) |
| Idle daemon memory per session | < 20 MB | +0.27 MiB per idle shell pane (daemon RSS 6.8 → 9.5 MiB for 10 panes); +5.1 MiB per pane after filling the 10 000-line scrollback (was 18.4 MiB with a 64-snapshot ring; note 2) | Met |
| Installed client size | < 25 MB | `Civet.app` 0.1.2 (`pnpm tauri build --bundles app`, aarch64 release, 2026-10-15): **20.2 MiB**, of which the app binary is 8.6 MB and the bundled daemons 12.5 MB (`civetd` x86_64 Linux 4.4 MB, aarch64 Linux 4.0 MB, aarch64 macOS 4.1 MB) | Met |

Other numbers (same runs):

| Scenario | Result |
|---|---|
| Loss 5% / 20%, 100 ms RTT, 1463 keystrokes typed one per message | Delivered in 1.26 / 1.38 s; echo p50 122 / 130 ms, p95 394 / 668 ms |
| Client restart, resume token | First update (Diff from the known state) 9.6 ms after dial |
| Client restart, fresh session | Full repaint 9.9 ms; complete history (482 lines fetched) 11.8 ms |
| UDP blocked | QUIC gives up at 3.0 s, stdio tunnel live at 3.03 s; tunnel echo p50 9.4 ms; tunnel process killed → resumed in 4 ms |
| UDP blocked mid-session (2026-10-15) | On the tunnel 5.0–5.2 s after the blackhole (1 s heartbeat + 1 s stall + 3 s race, then QUIC and tunnel raced); back on QUIC 2.0 s after UDP returns (probe interval 2 s in the test, 10 s default) |
| Throughput, `cat` 50 MiB (819 200 lines) | 1.33 s over QUIC (37.5 MiB/s), 1.06 s over the tunnel (47.3 MiB/s) since PTY reads are batched (2026-10-15; was 1.66 s / 30 MiB/s; note 3) |
| Throughput, `yes \| head -n 2000000` | 20.8 s; a bare macOS PTY read loop takes 19.5 s for the same command (`head` line-buffers to a tty: 1.4 M reads), so Civet adds ~7% |
| Scrollback gap after a resume (6000 lines while away) | 3982-line gap filled with `Range` pages in 0.7 ms (unix socket); 5981 lines paged upward with `Before` in 12 pages: 0.7 ms (unix), 326 ms (QUIC, 20 ms RTT) |
| Daemon restart (real binary) | SIGTERM: client `Failed { needs_bootstrap }` at once (Goodbye); SIGKILL: 117–136 ms after the new daemon is up (was 5.0 s; note 4). Re-bootstrap → new pane echo 18–38 ms; instance id changed, old panes gone |

Notes:

1. A NAT rebinding the client does not know about leaves the daemon with one
   small keystroke datagram on an unvalidated path; QUIC's anti-amplification
   limit (3× bytes received, RFC 9000 §8) holds the echo until the path
   challenge completes, +1 RTT. With the 2 s client keep-alive this only
   affects a keystroke sent in the instant after a rebinding while idle; a
   wake or Wi-Fi change goes through `notify_network_change` (1 RTT).
2. Mostly scrollback itself now. `civet-term`'s ring of recent snapshots
   (`DEFAULT_SNAPSHOT_RING`) was 64 and cost about 7 MiB per scrolling
   pane; it is 2 since 2026-10-15 (it is only the last fallback for a
   resume baseline; a miss yields a correct Full). Idle panes that never
   scrolled cost almost nothing.
3. Bare PTY `cat` of the same file: 0.29 s. macOS PTY reads return ~1 KiB;
   the reader now keeps reading while `poll` says more is ready (up to
   64 KiB) before feeding `PaneTerminal` once.
4. The QUIC stateless-reset key and the connection-ID generator key are
   derived from the persisted TLS identity key, so a restarted daemon
   answers the old connection's next packet with a reset the client
   recognizes (before: a random per-start key, and quinn dropped the old
   CIDs unanswered, so the client waited for its stall timers, 5.0 s).

## Open questions from the plan, as settled by the implementation

| Plan question | Answer |
|---|---|
| Terminal emulator crate | `alacritty_terminal` 0.26: published, 40 deps vs 281, all diff-encoding requirements met by our own encoder. See [crates/civet-term/SPIKE.md](crates/civet-term/SPIKE.md). |
| UDP reachability / how much the SSH fallback matters | Still unknown for the target hosts, so the fallback is complete rather than minimal: the same framed protocol over `ssh … civetd stdio-tunnel` (not QUIC-in-SSH), same resume logic, chosen automatically after a 3 s QUIC timeout. Measured cost above. |
| Auth model | A per-daemon-instance 256-bit session key, delivered only over SSH stdout, plus the daemon's pinned certificate fingerprint (persistent across restarts); resume tokens rotate per connection. No device-stored keys: a daemon restart means re-bootstrapping over SSH (automatic in the desktop app). |
| Coexisting with today's orcad | Fully separate: its own binary name, state dir (`~/.civet`), UDP range (60001–60999) and socket, so both run side by side on one host. |
| Agent status detection | Both: Claude Code hooks writing `$CIVET_STATUS_FILE` (installed automatically for `claude`), then OSC 9 / 777 / 133 escape codes, then output / BEL / idle heuristics for known agent CLIs. See [crates/civet-session/HOOKS.md](crates/civet-session/HOOKS.md). |
| Scrollback limits | 10 000 lines per pane in daemon memory (`scrollback_limit`), kept until the pane is killed or the daemon stops; not persisted to disk. A Diff replays ≤ 2000 lines, a Full ≤ 1000; older lines are fetched in pages. Memory cost: note 2 above. |
| Transport inside SSH when UDP is blocked | Not QUIC-in-TCP: the framed protocol directly over the stdio tunnel (one congestion controller, no UDP shim). |
| Linux client | Not decided by this work; the desktop is built and tested on macOS. |
