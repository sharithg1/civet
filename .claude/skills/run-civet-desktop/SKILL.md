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
| `right <files\|changes\|branch\|ports>` | Switch the right panel tab. |
| `click-file` / `dblclick-file <name>` | A row in the Files tree (exact entry name, e.g. `Cargo.toml`). |
| `rclick-file` / `cmd-click-file` / `shift-click-file <name>` | Right-click (opens the context menu), ⌘-click or ⇧-click a Files row. |
| `drag-file <from> -> <to>` | Drag a Files row onto another row (a folder, or a file: its folder) with the mouse. |
| `menu <text>` | Click an item of the open context menu (text prefix, e.g. `menu New File`). |
| `type <text>` | Type into the focused element (e.g. the inline name field). |
| `click-change` / `dblclick-change <name>` | A row in the Changes list (file name). |
| `click-tab` / `dblclick-tab` / `close-tab <label>` | An editor tab in the title bar. |
| `tabs` | Print the editor tab strip: `*` = italic (preview tab), `<` = active. |
| `ss [name]` | Screenshot to `$SHOTS/<name>.png`. |
| `text [selector]` | Print the visible text of every match (body by default, first 3000 chars). |
| `eval <js>` | Evaluate an expression in the page and print it. |
| `key <combo>` | `page.keyboard.press` (e.g. `Escape`). Not for ⌥ shortcuts — use `opt`. |
| `opt <key>` | An ⌥ shortcut: `opt t` terminal tab, `opt c` Claude tab, `opt 2` second tab, `opt e` right panel. |
| `wait [ms]` | Sleep (default 300). Every other command already waits 150 ms after. |

Add a command to `driver.mjs` when you need a new interaction; keep it small.

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
