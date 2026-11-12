---
name: run-civet-desktop
description: Run, start, drive or screenshot the Civet desktop app's UI (crates/civet-desktop/ui) against its mock backend in headless Chrome — connect a host, open worktrees, click files / changes / tabs, read the tab strip, take screenshots. Use to verify a desktop UI change in the running app, not just in unit tests.
---

# Run the Civet desktop UI

The desktop UI is a React + Vite app. In a plain browser it runs on the
**mock backend** (`src/backend.mock.ts`: a fake host with repos, worktrees,
agents, files and git changes), so no Rust, Tauri or civetd is needed to
exercise UI changes. `driver.mjs` loads it in headless Chrome via
`playwright-core` and runs a script of commands from stdin.

Paths below are relative to the repo root. Verified on macOS.

## Prerequisites

- Node from nvm (no `node` on the default PATH):
  `export PATH="$HOME/.nvm/versions/node/v24.18.0/bin:$PATH"` — `pnpm` lives there too.
- Google Chrome in `/Applications` (the driver uses `channel: 'chrome'`, so
  no Playwright browser download).

## Setup (once per worktree)

```sh
export PATH="$HOME/.nvm/versions/node/v24.18.0/bin:$PATH"
(cd crates/civet-desktop/ui && pnpm install --frozen-lockfile)
npm install --prefix .claude/skills/run-civet-desktop   # playwright-core for the driver
```

## Run (agent path)

1. Start Vite in the background (Bash `run_in_background: true`). Use a port
   other than 1420: that one is Tauri's `devUrl` and a `tauri dev` may hold it.

   ```sh
   cd crates/civet-desktop/ui && PATH="$HOME/.nvm/versions/node/v24.18.0/bin:$PATH" pnpm exec vite --port 5199 --strictPort
   ```

2. Wait for it (up to ~30 s; stop if it never answers), then pipe a script to the driver. Screenshots go to `$SHOTS`
   (default `./shots`) — point it at your scratchpad.

   ```sh
   curl -sf --retry 30 --retry-connrefused --retry-delay 1 -o /dev/null localhost:5199 || { echo "Vite is not up"; exit 1; }
   SHOTS=<scratchpad>/shots node .claude/skills/run-civet-desktop/driver.mjs <<'EOF'
   connect devbox
   right files
   click-file README.md
   tabs
   dblclick-tab README.md
   worktree Implement QUIC resume
   right changes
   wait 500
   click-change session.rs
   tabs
   ss changes
   EOF
   ```

   Output: `tabs ["Claude 1","session.rs*<"]`, `screenshot <path>`. Read the
   PNG to look at it. Exit code 1 and `FAILED <line>: <reason>` on the first
   step that fails; page errors are printed at the end and also make it exit 1.

3. Stop Vite: `pkill -f "vite --port 5199"`.

### Commands (one per line, `#` comments allowed)

| Command | Does |
|---|---|
| `connect [host]` | Connect a mock host (default `devbox`; a name containing `fail` fails, `udp-blocked` falls back to SSH). Waits for "1 host connected". The `main` worktree of `civet` opens. |
| `worktree <text>` | Click a sidebar worktree by its text (`Implement QUIC resume`, `scrollback-paging`…). |
| `hover-worktree <text>` | Hover a sidebar worktree row (ahead/behind and spend are in its tooltip). |
| `right <files\|changes\|branch\|ports>` | Switch the right panel tab. |
| `click-file` / `dblclick-file <name>` | A row in the Files tree (exact entry name, e.g. `Cargo.toml`). |
| `rclick-file` / `cmd-click-file` / `shift-click-file <name>` | Right-click (opens the context menu), ⌘-click or ⇧-click a Files row. |
| `drag-file <from> -> <to>` | Drag a Files row onto another row (a folder, or a file: its folder) with the mouse. |
| `drag-separator <label> <dx>` / `dblclick-separator <label>` | Drag a panel resize handle by its label (`Resize sidebar`, `Resize right panel`) `dx` px (negative = left), or double-click it (reset). |
| `menu <text>` | Click an item of the open context menu (text prefix, e.g. `menu New File`). |
| `type <text>` | Type into the focused element (e.g. the inline name field). |
| `click-change` / `dblclick-change <name>` | A row in the Changes list (file name). |
| `meta-click-change` / `shift-click-change` / `hover-change <name>` | ⌘- / ⇧-click a Changes row (multi-select), or hover it to show its buttons. |
| `click-button <name>` | A button by its exact accessible name (icon buttons: the tooltip label, e.g. `Stage all`, `More git actions`). |
| `menu-item <text>` | An item of the open dropdown / context menu, by the start of its text. |
| `commit-message <text>` | Type into the Changes tab's commit message box. |
| `click-tab` / `dblclick-tab` / `close-tab <label>` | An editor tab in the title bar. |
| `tabs` | Print the editor tab strip: `*` = italic (preview tab), `<` = active. |
| `ss [name]` | Screenshot to `$SHOTS/<name>.png`. |
| `text [selector]` | Print the visible text of every match (body by default, first 3000 chars). |
| `eval <js>` | Evaluate an expression in the page and print it. |
| `key <combo>` | `page.keyboard.press` (e.g. `Escape`). Not for ⌥ shortcuts — use `opt`. |
| `opt <key>` | An ⌥ shortcut: `opt t` terminal tab, `opt c` Claude tab, `opt 2` second tab, `opt e` right panel. |
| `wait [ms]` | Sleep (default 300). Every other command already waits 150 ms after. |

Add a command to `driver.mjs` when you need a new interaction; keep it small.

## UI catalog (all screens, for a design review)

`tour.txt` walks every screen, panel, menu and dialog (~60: connect, sidebar,
tabs, splits, find, history, switcher, Files / Changes / Activity / Branch /
Ports, worktree states, host stats, every dialog, every settings page, hidden
panels, narrow window, connection drop, toasts, old daemons, About). Each
`ss name -- description` line also lands in `manifest.json`.

```sh
.claude/skills/run-civet-desktop/catalog.sh            # → docs/ui-review/
.claude/skills/run-civet-desktop/catalog.sh <dir> 1100x720
```

It writes `<dir>/screenshots/*.png` + `manifest.json` and `<dir>/README.md`
(index, capture notes, mock artifacts to ignore, and a review brief asking for
bad UI / improvements / redundancies in `REVIEW.md`). ~22 MB at 1400×900.
Port 5231 must be free (PORT=…). Add a screen: add steps and an
`ss NN-name -- what it shows` line to `tour.txt`, then re-run.

## Design review (screenshot tour, before/after)

`shoot.sh` runs `tour.txt` at each window size, at 2× scale. With `--base <ref>` it also runs
the tour against `<ref>` in a temporary git worktree and writes a before |
after composite (changed pixels tinted magenta) for each screen that changed.

```sh
.claude/skills/run-civet-desktop/shoot.sh <scratchpad>/review --base main
# options: --sizes "1400x900 1100x720" (default)  --tour <file>
# ports: PORT (5231, this checkout), BASE_PORT (5232, the base ref)
```

Output: `<out>/after/<size>/NN-name.png`, and with `--base` also
`<out>/before/<size>/` and `<out>/compare/<size>/` plus a `same` /
`N.NN% changed` line per screen. Read the PNGs to review them.

- The tour runs with `KEEP_GOING=1`: a step that fails (UI that doesn't exist
  on the base ref yet) is logged as `FAILED` and the tour goes on, with a 5 s
  timeout instead of 30 s. Screens after a failed step may show a different state.
- Small diffs (well under 1%) on `05-agent-worktree` / `06-changes` are mock
  noise: the agent terminal and change counts are live. The "Agent finished"
  toast also shows up in later screens.
- Add a screen by adding steps + an `ss NN-name` line to `tour.txt`; keep the
  names numbered and stable so before/after pairs line up.
- Driver env for ad-hoc runs: `SCALE` (default 2), `VIEWPORT` (`1400x900`),
  `KEEP_GOING=1`. The app has one (dark) theme, so there's no theme variant.

Design-review commands (in addition to the table above):

| Command | Does |
|---|---|
| `ss-el <name> <selector>` | Screenshot one element: `aside` = sidebar, `aside >> nth=1` = right panel. Clips popovers that stick out. |
| `viewport <WxH>` | Resize the window mid-script. |
| `hover-button <name>` | Hover a button by accessible name (tooltip after ~900 ms `wait`). |
| `unhover` | Move the mouse to the corner (drop hover / tooltip). |
| `rclick <text>` / `click-text <text>` / `hover-text <text>` | Right-click / click / hover any element by exact visible text (project rows, the host row). |
| `click <selector>` | Click the first match of a CSS selector (`click .xterm` focuses the terminal). |
| `ss <name> -- <desc>` / `ss-el <name> <sel> -- <desc>` | As above, recording the description in `manifest.json`. |
| `js <expr>` | Run JS in the page with the app's modules in scope: `store` (state/store.ts), `prompts` (state/prompts.ts), `s` (current state). Opens anything directly: `js store.useStore.setState({ settings: 'hosts' })`, `js prompts.openSendPrompt()`, `js store.pushToast({...}, 0)`. |
| `connect-try <host>` | Fill and submit the connect form without waiting (error states, e.g. `fail-box`). |
| `fresh` | Clear localStorage (saved hosts, prefs) and reload: back to the connect screen. |
| `goto <path>` | Load a URL relative to the start URL (`goto /?about` is the About window). |
| `toasts off` / `toasts on` | Hide / show toast notifications. |
| `scroll <dy> <selector>` | Mouse wheel over an element. |

## Run (human path)

`cd crates/civet-desktop/ui && pnpm dev`, then open `http://localhost:1420/?mock`.
The real app (`pnpm tauri dev`) needs the Rust toolchain and a daemon; not covered here.

## Test

```sh
cd crates/civet-desktop/ui && pnpm typecheck && pnpm lint && pnpm test
```

## Gotchas

- The `main` worktree of the mock `civet` repo has **no git changes**; use
  `Implement QUIC resume` (quic-resume) for the Changes tab. It also has a
  running mock agent tab (`Claude 1`).
- Git actions in the Changes tab change per-worktree mock state (`MockGit`
  in `backend.mock.ts`). `quic-resume` is ahead of its upstream; a `fetch`
  makes it behind by one (so `push` is rejected until a `pull`); branches
  other than `quic-resume` / `fix-flaky-e2e` have no upstream (Publish).
  `fix-flaky-e2e` has a conflict. Connect `pre-git-box` for a civetd too old
  for git actions (controls greyed out).
- The right panel's Files / Changes / Branch / Ports buttons are also
  `role="tab"`; editor tabs are the ones with `data-tab-id`. Don't select
  `[role="tab"]` alone.
- `key Alt+t` does nothing on macOS: the app takes ⌥+key only when the
  event's `key` is non-ASCII (what Option composes, e.g. `†`), and Playwright
  sends a plain `t`. `opt t` dispatches the Mac-shaped event instead.
- Mock change counts grow while you watch (`growingLines`), so don't assert
  on `+N -M` numbers.
- `node node_modules/.bin/vite` fails (`SyntaxError: missing ) after argument
  list`): pnpm's `.bin` entries are shell scripts. Use `pnpm exec vite`.
- Without the nvm PATH export, `pnpm`/`node` are "command not found".

## Troubleshooting

- `page.goto: net::ERR_CONNECTION_REFUSED` — Vite isn't up yet (or died); run
  the `curl` wait first, check the Vite task output.
