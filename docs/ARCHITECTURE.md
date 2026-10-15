# Civet architecture

_As of 2026-10-14. Companion to [INTERFACES.md](INTERFACES.md), which has the
exact APIs. Source plan: `../orca-tauri-mvp-plan.md` (written for "Orca
Remote" / `orcad-rs`; renamed to Civet / `civetd`)._

## What Civet is

A remote-first workspace for coding agents. A daemon, `civetd`, runs on the
remote Linux host. It owns repos, git worktrees, PTYs and the terminal screen
of every pane. A Tauri desktop app on the laptop shows those panes in
xterm.js. The one requirement that shapes everything: **when the laptop wakes
up or changes networks, the session is back immediately, with nothing on
screen suggesting it was ever gone.**

Orca, the Electron app this replaces for the remote workflow, reaches its
daemon over a WebSocket inside an SSH port forward. After sleep the TCP
connection is dead but nobody knows yet; four independent liveness checks
(5 s to 30 s) race to notice, then recovery redoes SSH and WebSocket
handshakes with backoff (see Orca's `ssh-wake-reconnect-handover.md`). The
fix is not a faster language but a different transport model: mosh's.

```
 Laptop                                                   Remote host
 ┌──────────────────────────────────────────┐            ┌────────────────────────────────────────┐
 │ civet-desktop (Tauri)                    │            │ civetd                                 │
 │  React UI + xterm.js (WebGL)             │            │  QuicServer ── per-client state        │
 │    ▲ binary Channel (VT records)         │   QUIC     │   (resume token, subscriptions)        │
 │    │ invoke / events (JSON)              │◄══════════►│        │                               │
 │  Rust core:                              │   UDP      │  SessionManager                        │
 │   HostManager → ClientSession (resume,   │            │   repos/worktrees (git CLI)            │
 │   input seq, stall detection)            │            │   panes: PTY ─► PaneTerminal           │
 │   wake detector, notifications,          │            │          (alacritty_terminal,          │
 │   predictor (phase 2)                    │            │           snapshots, diff→VT,          │
 └──────────────┬───────────────────────────┘            │           scrollback store)            │
                │ ssh (bootstrap only; fallback tunnel)   │   agent status (hooks, OSC, heuristics)│
                └────────────────────────────────────────►│  unix socket (stdio-tunnel, local)     │
                                                          └────────────────────────────────────────┘
```

## Crates

| Crate | Role |
|---|---|
| `civet-proto` | Every wire type (postcard + length-prefix framing; JSON for the UI), bootstrap line, handshake, constants. No I/O. |
| `civet-transport` | QUIC (quinn) with a pinned self-signed cert, session-key hello, stream mux, connection migration, and the same API over any byte stream. Client-side `ClientSession`: reconnect, resume, input retransmission, stall detection. |
| `civet-term` | Server-side terminal emulator per pane, immutable snapshots, snapshot→VT diff encoder, absolute-numbered scrollback store. Also used client-side as a mirror for prediction. |
| `civet-session` | PTYs (portable-pty), repos/worktrees (git CLI), input dedup, per-client subscriptions with flow control, agent status detection. |
| `civetd` | Daemon binary (`serve`, `bootstrap`, `stdio-tunnel`, `version`), client bootstrap library (ssh, install, parse), `civet-cli` test client. |
| `civet-desktop` | Tauri app: commands, binary channels, events, notifications, wake detection, prediction. React UI in `ui/`. |

## Key decisions

1. **The daemon owns the PTYs and the screen.** Panes survive client
   crashes, laptop sleep and network changes because nothing on the laptop is
   authoritative. The laptop only renders.

2. **QUIC over UDP, not TCP.** QUIC gives us what mosh built by hand:
   encryption, authenticated packet numbers (replay protection), and
   connection migration. A QUIC connection is identified by connection IDs,
   not the 4-tuple, so when the laptop's address changes the server follows
   after path validation. Unlike mosh we keep reliable ordered streams, which
   makes input simple (no custom retransmission inside a connection) while
   screen updates stay newest-wins through flow control (below).

3. **SSH for bootstrap only.** Like mosh: `ssh host civetd bootstrap`
   starts or finds the daemon and prints one JSON line (UDP port, 256-bit
   session key, SHA-256 of the daemon's certificate, version). SSH is then
   done. Trust in the certificate fingerprint comes from SSH's host
   authentication, so the self-signed cert needs no CA. The session key then
   authenticates the client inside the pinned TLS channel (first frame on the
   control stream, constant-time compare). The certificate persists across
   daemon restarts; the session key does not.

4. **Fallback = the same framed protocol over `ssh … civetd stdio-tunnel`,**
   not QUIC inside SSH. QUIC-in-TCP adds a second congestion controller and
   a user-space UDP shim for no benefit; the framed protocol already runs over
   any ordered byte stream. The tunnel just pipes stdio to the daemon's unix
   socket. Higher layers see the same `ClientConnection` / `ServerConnection`
   either way. On the stream link screen updates share the control stream
   and scrollback uses an RPC, so it has head-of-line blocking but always
   works (UDP blocked, ProxyJump bastions).

5. **Resume tokens.** Each accepted hello returns a fresh resume token.
   Presenting it on a new connection (after the old one died) restores the
   client's per-connection state on the daemon: subscriptions, retained
   snapshots, applied input sequence numbers. Without a valid token the
   session is fresh and the client must not resend old input (it might have
   been applied already: never duplicate keystrokes).

6. **alacritty_terminal on the server, xterm.js on the client, VT bytes in
   between.** The server encodes screen differences as escape sequences that
   xterm.js plays directly, so the frontend stays dumb and fast, and the
   same stream can be written to a real terminal (civet-cli attach).
   wezterm-term was the alternative; alacritty_terminal is the published,
   Zed-proven crate.

7. **git CLI, not libgit2.** Worktree semantics, hooks, credential helpers
   and config all behave exactly like the user's own git; no vendored C build.

8. **Binary Tauri channels for terminal output**, JSON only for control.
   Terminal records carry a fixed 44-byte header plus raw VT payload; batched
   at most 4 ms.

9. **Prediction in Rust, not in the webview.** The predictor needs the true
   screen to undo its guesses before a server diff is applied (a diff assumes
   the client shows exactly its base state). The desktop core keeps a mirror
   `PaneTerminal` per attached pane and emits "undo, server diff, redo" as one
   VT stream. xterm.js never knows predictions exist.

## How state sync works (the mosh model)

### Screen state

Every pane on the daemon has a `PaneTerminal`. Each `feed()` of PTY output
(or resize) that changes anything bumps the pane's **state number** (u64,
starting at 1). A `Snapshot` is an immutable copy of the visible screen,
cursor, modes, title and the absolute history count at a state number. Rows
are reference-counted, so holding a few snapshots costs little.

A **screen update** is `{state, base_state, kind, acked_input_seq, size,
history_start, history_lines, payload}`. The payload is VT bytes that turn
a terminal showing `base_state` into one showing `state` (`Diff`), or repaint
from scratch (`Full`, which starts with RIS).

### Per-client subscription and flow control

Each (client, pane) pair has a `Subscription` on the daemon:

```
       baseline (last sent)         inflight (sent, not yet acked, ≤ 8)        acked
PTY ─► PaneTerminal state N  ──►  Subscription.next():                         ▲
                                   wait for change, coalesce (2 ms / 8 ms),     │ ScreenAck{state}
                                   wait while inflight is full,                 │
                                   encode_diff(baseline → current)  ──► ScreenStream ──► client
```

- **Newest wins.** The update is always computed from the baseline to the
  *current* state at the moment of sending. If the pane produced 500 states
  while one update was in flight, the next update jumps straight to the
  newest one. That is why `cat huge.log` cannot back up the link and why a
  slow link degrades into fewer, larger frames rather than lag.
- **Bounded in-flight window** (`MAX_INFLIGHT_UPDATES` = 8). The QUIC stream
  is reliable, so within a connection the baseline is the last *sent* state;
  the window and stream flow control decide when to send. While the window
  is full the daemon just waits and coalesces.
- **Acks** release in-flight snapshots and set the *acked* snapshot, the
  baseline for a future resume.

### Client input

- The client numbers keystrokes per pane: seq 1, 2, 3… The daemon writes a
  seq to the PTY only if it is greater than the last applied one, so resending
  is always safe within a resumed session.
- The client keeps unacked input until an update's `acked_input_seq` (or an
  `InputAck`) covers it. `acked_input_seq` follows mosh's echo-ack rule: input
  counts as acknowledged once it was written at least 50 ms before the
  snapshot, so the screen likely shows its echo. This feeds prediction.

### Reconnect cases

**Case 1: wake within 6 h on the same or a different network (common).**
The QUIC connection is still alive on both ends (idle timeout 6 h). The
desktop's wake detector (wall-clock jump) calls `notify_network_change()`:
rebind the UDP socket and send a `Ping` at once. That packet *is* the
reconnect: the server validates the new path and migrates; screen updates
flow on the existing streams; the diff baseline is whatever was last sent
(reliable streams retransmit anything lost during sleep). Expected cost: one
RTT. Nothing is shown to the user, because "reconnecting" appears only if
recovery takes more than 1 s.

```
laptop wakes ─► rebind + Ping ─► server: new path, migrate ─► Pong + pending screen diffs
     0 ms                                ~RTT/2                         ~RTT
```

**Case 2: the connection is gone** (daemon idle-timed it out, NAT state lost
and a stateless reset came back, or the client restarted). The client dials
a new QUIC connection and, in the same flight as its hello, pipelines
`SubscribePane{known_state = current}` for every pane. The daemon validates
the resume token, re-binds the subscriptions, and diffs from `known_state` if
it still holds that snapshot (the acked one or one in flight), else sends a
Full. The client resends input with `seq > last_input_seq` from
`resumed_subscriptions`. Cost: QUIC handshake (1 RTT) + hello (1 RTT).

**Case 3: the link is silently dead** (no reset, no packets). The client
heartbeat (Ping after 1 s of silence) goes unanswered → `Recovering`, rebind;
after 3 s a second connection is dialed in parallel with the resume token,
and whichever answers first wins.

**Case 4: daemon restarted.** The hello is rejected with `BadKey` (new
session key) → re-run bootstrap over SSH. The new `instance_id` tells the
client that all panes are gone.

**Case 5: UDP blocked.** The initial QUIC dial times out after 3 s → stdio
tunnel over SSH, same protocol, same resume logic (each re-dial spawns a new
`ssh … stdio-tunnel`, reusing the ControlMaster connection).

### Why the user sees nothing

- No handshake in Case 1; one round trip of handshake in Case 2.
- The UI's "reconnecting" label waits 1 s after recovery starts, so sub-second
  resumes stay invisible. When it does show, it is accurate, because it is
  driven by the session's own stall detection rather than `connected` lying.
- No output is lost: the server screen is authoritative and history that
  scrolled off while the client was away is replayed or fetchable (next
  section).

## Scrollback

mosh does not sync scrollback; Civet does, designed in from the start.

- Lines that scroll off the top of a pane's primary screen get **absolute
  line numbers** (0 = first line ever) and go into the daemon's
  `ScrollbackStore` as compact VT (10 000 lines per pane by default, oldest
  evicted). Numbers are never reused, so a client can ask for "the 200 lines
  before line 52 113" without races.
- **Replay into xterm's own scrollback.** Each update says
  `history_start..history_lines`: the payload first pushes those lines
  through the client terminal (so they land in xterm.js's scrollback in
  order), then paints the screen. A Diff replays at most 2000 lines; a Full
  clears and replays the newest 1000. So normal scrolling in xterm.js just
  works, even after coalescing skipped hundreds of intermediate states.
- **Gaps and older history** are fetched in pages:
  `ScrollbackQuery::Before { end, limit }` pages upward,
  `Range { start, end }` fills a known gap. QUIC uses one bidi stream per
  request so a big page never delays keystrokes or screen updates. The
  desktop shows older history in a read-only history view above the live
  terminal (xterm.js cannot prepend to its scrollback).

## Local echo prediction (phase 2)

Follows mosh. When the RTT is above about 30 ms, printable keystrokes (and
Backspace/Left/Right) are drawn locally right away, underlined, at the
predicted cursor position, inside the desktop Rust core. Predictions are
retired when the server's screen agrees and `acked_input_seq` covers the
keystroke. A misprediction erases the guesses (from the mirror's true
cells) and pauses prediction briefly. No prediction in the alternate screen
unless recent keystrokes were echoed (full-screen apps), and none when echo
is off (password prompts: keystrokes not echoed recently). Target: keystroke
→ pixels < 10 ms regardless of RTT.

## Agent status

Per pane, the daemon derives `Unknown | Running | NeedsInput | Done | Idle`
from, in order of trust: process exit; the pane's hook file
(`$CIVET_STATUS_FILE`, written by agent hooks such as Claude Code's
`Stop`/`Notification`; the daemon passes generated hook settings to `claude`
automatically); terminal escape codes (OSC 9 / OSC 777 notifications,
OSC 133 prompt marks); and heuristics for known agent CLIs (output activity,
BEL followed by quiet, idle timeout). Changes are pushed as
`Event::AgentStatus`; the desktop turns transitions into Done/NeedsInput
into a sidebar badge and, when unfocused, a native notification.

## Security notes

- Trust anchor: SSH. The fingerprint and session key travel only over SSH
  stdout; `daemon.json`, `key.der` and `civetd.sock` are mode 0600.
- QUIC/TLS 1.3 encrypts and authenticates everything, including migration
  (path validation, packet numbers prevent replay; the Orca handover's
  requirements for a UDP transport are covered by QUIC itself).
- The session key is compared in constant time; resume tokens are rotated
  per connection and bound to the `ClientId`.
- Any holder of the session key has full shell access as the daemon's user,
  same as SSH. Keys are never logged; input bytes are never logged.

## Failure modes

| Situation | Behavior |
|---|---|
| Wi-Fi switch / lid close and open (< 6 h) | Migration, ~1 RTT, invisible |
| Long sleep (> 6 h, < 24 h) | New connection + resume, ~2 RTT |
| > 24 h away | Fresh session: Full repaint per pane; panes still running |
| Client app restart | Fresh session; reattach panes with Full repaints; unsent input lost |
| Daemon crash / host reboot | Panes lost; bootstrap starts a new daemon |
| UDP blocked | SSH stdio tunnel, same features, more latency under loss |
| Huge output burst | Coalesced to ≤ 1 update / 8 ms per pane; history replay capped, rest fetchable |
| Daemon upgrade | New binary installed side by side; running compatible daemon is reattached; incompatible one replaced only when it has no panes (or `--replace`) |

## Open questions carried from the plan

- How common is blocked inbound UDP among target hosts (decides how much to
  invest in the tunnel path)?
- Device-stored credentials for reconnecting after daemon restarts without
  SSH?
- Scrollback retention limits per pane and memory budget (target < 20 MB
  idle per session).
- Hook coverage for agents beyond Claude Code (Codex `notify`, Gemini…).
- Linux client (WebKitGTK WebGL performance) in or out.
- QUIC 0-RTT for Case 2 (would remove one RTT; needs replay-safe hello).
