# civet-term spike: alacritty_terminal vs wezterm-term for diff encoding

_2026-10-14. Answers the plan's open question "Terminal emulator crate:
`alacritty_terminal` or `wezterm-term`. Choose after a week-1 spike on diff
encoding."_

## Decision

**alacritty_terminal 0.26** (already pinned in the workspace). No blocker
found; every requirement in INTERFACES.md §3 is met and covered by tests.
wezterm-term's change-tracking API is nicer, but it is not published on
crates.io and weighs about 7x more in dependencies.

## What was compared

| | alacritty_terminal 0.26 | wezterm-term |
|---|---|---|
| Published on crates.io | Yes (used by Zed) | **No.** Upstream is git-only. On crates.io there are only third-party forks (`tattoy-wezterm-term 0.1.0-fork.5`, `zeughaus-wezterm-term`) |
| Dependency closure (`cargo tree`, unique crates) | 40 | 281 (the tattoy fork) |
| Clean release build of just the crate (M5 Pro) | 4.3 s wall / 23 s CPU | 16.2 s wall / 165 s CPU |
| Raw parse+emulate throughput | 212 MB/s (plain text, 200x60) | not measured (build-only spike) |
| Dirty-row detection | None usable. Damage tracking relies on the renderer polling the cursor every frame; `Term::input` does not mark cells | `Line::changed_since(seqno)` plus a per-terminal `current_seqno()`, cheap and exact |
| Diff to escape sequences | None | termwiz `Surface::diff_lines` / `diff_region` → `Change` list → a terminfo renderer |
| Scrollback numbering | Grid history at `Line(-n)` with `history_size()` / `clear_history()`. No hook for "line scrolled off" | `StableRowIndex` (absolute row numbers) |
| Synchronized output `?2026` | Built into its `Processor` (buffers until ESU or a 150 ms timer) | Its own handling |
| Title / OSC 9, 777, 133, 7 | Title is private (it only emits events); the OSCs are ignored | More complete |

Diff encoding is ours either way. Neither crate emits what the contract
needs: a VT stream for xterm.js that replays history lines into the client's
scrollback, mirrors input-affecting modes, switches alt screens and repaints
from a known base. termwiz's renderer targets terminfo terminals and knows
nothing about history replay or our state numbering. wezterm's real
advantages are cheap dirty-row detection and stable row numbers. Measured
against an unpublished, much heavier dependency, those are not worth it. Our
costs on the alacritty side are small and contained:

* **Dirty rows.** We convert each visible row and compare it with the
  previous snapshot's row, reusing the `Arc` when equal. This costs about
  70 µs per `feed()` on a dense 200x60 screen. Rows are converted only up to
  their last non-blank cell.
* **Scrollback capture.** This uses the method recommended in INTERFACES.md:
  alacritty keeps a fixed history of `max(4096, 4·rows, …)` lines. A
  conservative byte scanner (`scan.rs`) splits PTY input so that one chunk
  can push at most 900 lines. Before RIS, ED and DECSET `h` it also splits,
  because those could clear or hide alacritty's history. After every chunk
  the history is moved into the `ScrollbackStore` and `clear_history()` is
  called. 900 lines stays below alacritty's 1000-row spare cache, so rows are
  recycled instead of reallocated. That cache limit was a 1.7x throughput
  difference for line-dense output.
* **`?2026`.** A custom `Timeout` (`NoSync`) disables buffering. Bytes apply
  immediately and a snapshot is taken only at the end of each `feed()`, so
  the scrollback bound holds and no timer thread is needed.
* **Title and OSCs.** A second `vte::Parser` runs over the same bytes. It
  handles OSC 0/2, the title stack (`CSI 22/23 t`), RIS, OSC 9/777/133/7 and
  XTVERSION. It runs at about 4 GB/s, which is negligible.

## Encoder design (summary)

* **Snapshot.** Rows are `Arc<Row>` and shared between states. Cells are
  normalized: an orphaned wide-char half becomes a blank (DCH/ICH/ECH can
  create these in alacritty), and a leading wide-char spacer becomes a blank.
* **Same size and screen, no history growth.** Changed runs are merged across
  gaps of at most 3 cells. A trailing run of identical erased cells is
  written as `EL` with its background. SGR is minimal. The cursor moves by
  `CR`, `CRLF`, `CUF` or `CUP`.
* **Plain scroll fast path.** History grew by `k < rows`, and the base's top
  `k` rows are exactly the new history lines (compared by their VT
  encoding). The diff sends `CUP bottom` plus `k`×`LF`, so the client
  scrolls its own rows into its scrollback. Then it diffs against the
  shifted screen. A log tail costs about one line of bytes per update.
* **Anything else structural** (history beyond the fast path, size change,
  alt-screen switch):
  1. Clear with `CSI H CSI J`. Using ED 0 instead of ED 2 means neither
     xterm.js nor alacritty pushes the screen into scrollback.
  2. Replay history lines, each followed by CRLF. A soft-wrapped line of the
     same width continues by autowrap, triggered with `" \r"` so a
     bottom-row scroll erases with the default background.
  3. Send `rows-1` LFs.
  4. Switch the alt screen if needed.
  5. Repaint from blank.
* **Full.** `RIS`, then the newest 1000 history lines, the alt screen, every
  cell, the title, cursor shape, position and visibility, and autowrap.
* **Client invariant.** After every payload the client has a full scroll
  region, origin/insert modes off, G0 ASCII, SGR reset and no open
  hyperlink.

## Tests

`cargo test -p civet-term` (debug about 30 s, `--release` about 3 s): 38
tests.

* `tests/diff_prop.rs` is the core property test (proptest). Random and
  realistic op streams go into terminal A, fed in random splits: text with
  CJK, emoji, ZWJ/skin tones, combining marks and VS16, box drawing, every
  SGR form (16/256/truecolor, underline styles and colors), cursor motion,
  EL/ED/ECH/DCH/ICH/IL/DL/SU/SD/REP, scroll regions, IND/RI/NEL,
  DECSC/DECRC, insert mode, charsets, DECALN, every mirrored mode,
  alt-screen, DECSCUSR, titles and the title stack, OSC 8 links, RIS,
  DECSTR, raw random bytes, vim-like full-screen redraws, scrolling
  colored logs with wrapping, and resizes. At random checkpoints a client
  B either applies `encode_diff(B's state → now)` continuously, is rebuilt
  from `encode_full(older)` and then given `encode_diff(older → now)`, or is
  rebuilt from `encode_full(now)`. The test asserts that B's grid (chars,
  zero-width, fg/bg/underline color, flags, wide flags, hyperlink), cursor
  (position, visibility, shape), modes, title and alt screen equal A's.
  While A was never resized it also checks that B's scrollback holds exactly
  A's history lines, rendered cell for cell. The suite runs 512 + 64 cases;
  a one-off stress run of 20 000 + 2 000 cases passed twice. Mutating the
  encoder (dropping some painted runs) makes it fail immediately.
* `tests/realistic.rs` checks a vim session (alt screen, modes, 30 page
  redraws, cursor moves), a colored log (fast path, at most 200 B per line),
  a 5000-line burst coalesced into one update (replay capped at
  `MAX_REPLAY_LINES`), a progress bar, a scroll region inside the alt screen,
  wrapping with wide chars, a resize sequence, attributes, `encode_cells`
  undoing a prediction, the keystroke diff size, and the snapshot ring.
* `tests/scrollback.rs` checks `seq 1 100000` (contiguous, no gaps or
  duplicates), a single 300 000-line feed, RIS and `CSI 3J` inside the same
  chunk, alt screen not reaching history, wrapped lines and SGR, `Before` and
  `Range` paging plus eviction and clamping, Full `history_start`, and ED 2.
* `tests/behavior.rs` checks state numbering, CPR, DA, XTVERSION, DECRQM,
  OSC 10/11 queries, OSC 9 (but not the ConEmu `9;4` progress form), 777,
  133, 7, BEL, the title stack and RIS, alt-screen reporting, `?2026`,
  garbage input and `Send`.

## Benchmark

`cargo run --release -p civet-term --example bench` on an Apple M5 Pro,
200x60, release profile (thin LTO):

| Operation | Time | Payload |
|---|---|---|
| `feed`, colored log, 64 KiB reads (32 MB) | **75 MB/s** | |
| `feed`, plain text lines, 64 KiB reads | **75 MB/s** | |
| `feed` of one keystroke + snapshot, dense screen | 69 µs | |
| `encode_diff`, a few changed cells | **1.7 µs** | 39 B |
| `encode_diff`, cursor move only | 37 ns | |
| `encode_diff`, every row changed (vim page) | 18 µs | 3.4 KB |
| `encode_diff`, 1-line scroll (fast path) | 23 µs | 46 B |
| `encode_diff`, 25-line scroll (replay path) | 40 µs | 3.1 KB |
| `encode_full`, dense colored screen | **63 µs** | 21.6 KB |
| `encode_full`, dense screen + 1000 history lines | 68 µs | 152 KB |

All numbers are within budget: diff of a 200x60 screen under 1 ms (worst
case measured 40 µs), few-cell diff under 100 µs, full repaint under 1 ms,
and feed at least 50 MB/s.
