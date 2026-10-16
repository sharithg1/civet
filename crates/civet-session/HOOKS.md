# Agent status hooks (`$CIVET_STATUS_FILE`)

Every Civet pane gets these environment variables:

| Variable | Value |
|---|---|
| `CIVET_PANE_ID` | the pane id (decimal) |
| `CIVET_STATUS_FILE` | `<state_dir>/status/<pane>.status`, created empty at spawn |
| `CIVET_SHIM_DIR` | `<state_dir>/hooks/bin` (agent PATH shims, see below) |
| `PATH` | `$CIVET_SHIM_DIR` first, then the daemon's PATH |
| `TERM_PROGRAM` | `civet` (and `LC_TERMINAL=civet` if the daemon had one), so agents do not emit another terminal's private escape codes |

`civetd` polls that file (mtime + length, every 250 ms). Whenever it changes,
the whole file is parsed with `civet_proto::parse_status_file` and the pane's
agent status is set with source `Hook`. The first hook report turns any pane
(also a plain shell pane) into an agent pane.

Once a hook has reported `running` or `needs_input` (a full lifecycle, like
Claude Code's),
the output/idle heuristics stop applying to the pane, with two fallbacks:

* `running` with no output for 30 s → `idle` (an interrupted turn, e.g.
  Esc in Claude Code, fires no `Stop` hook);
* `needs_input`, then user input followed by an output burst within 2 s →
  `running` (the prompt was answered; the next hook confirms it).

Hooks that only report the end of a turn (Codex `notify` → `done`) keep the
`running` heuristics. OSC 9 / 777 notifications are ignored in hooked panes.

## File format

Either plain text (first line is the status, the rest is a message):

```sh
printf 'needs_input\nAllow Bash(rm -rf build)?\n' > "$CIVET_STATUS_FILE"
```

or a JSON object:

```sh
printf '{"status":"done","message":"Tests pass"}' > "$CIVET_STATUS_FILE"
```

Status words are case-insensitive, `-`/`_`/spaces are ignored:

| Status | Accepted words |
|---|---|
| Running | `running`, `working`, `busy` |
| NeedsInput | `needs_input`, `needsInput`, `waiting`, `input`, `permission` |
| Done | `done`, `finished`, `complete`, `completed`, `stop`, `stopped` |
| Idle | `idle` |
| Unknown | `unknown` |

Write the file in one go (or write a temp file and `mv` it over). Always
guard on the variable so the hook is a no-op outside Civet:
`[ -n "$CIVET_STATUS_FILE" ] && ...`.

## The generated helper script

At startup the daemon writes `<state_dir>/hooks/civet-status-hook.sh`
(`~/.civet/hooks/civet-status-hook.sh` by default):

```
civet-status-hook.sh <running|needs_input|done|idle> [soft | --codex-notify JSON]
```

It writes `$CIVET_STATUS_FILE` atomically and always exits 0, so it can
never block or fail an agent. It does nothing when `CIVET_STATUS_FILE` is
unset.

* Without a mode it reads the hook event JSON from stdin (Claude Code). For
  `needs_input` the event's **top-level** `"message"` string becomes the
  status message. It is extracted by a small POSIX awk JSON scanner
  embedded in the script (handles escaped quotes/backslashes, `\n`, nested
  objects and arrays; scans at most 64 KiB; no jq/python needed).
  Notification events whose top-level `notification_type` is `idle_prompt`
  (the "waiting for your input" reminder after a finished turn) or
  `auth_success` are ignored.
* `soft`: do not replace a pending `needs_input` (used for `PreToolUse`, so
  a parallel subagent's tool call cannot hide a permission prompt; only a
  later `PostToolUse`, `UserPromptSubmit` or `Stop` ends it).
* `--codex-notify JSON`: Codex `notify` mode; the event JSON is the next
  argument (stdin is not read) and its `last-assistant-message` becomes the
  message.

## PATH shims: Claude Code and Codex (automatic)

Nothing to do, and your `~/.claude` / `~/.codex` configuration is never
modified. The daemon writes `<state_dir>/hooks/bin/claude` and
`<state_dir>/hooks/bin/codex` and puts that directory first on every pane's
`PATH`. Each shim finds the real program later on `PATH` (skipping the shim
directory and itself; exit 127 if there is none) and execs it:

* `claude [args]` → `claude --settings <state_dir>/hooks/claude-settings.json [args]`,
  unless the arguments already contain `--settings`. Claude Code merges
  these settings with the user's own, so existing hooks keep working.
* `codex [args]` → `codex -c 'notify=["<hook script>","done","--codex-notify"]' [args]`,
  unless the arguments already set `notify` or `$CODEX_HOME/config.toml`
  (default `~/.codex/config.toml`) has its own `notify` line (yours wins).
* Outside a Civet pane (`CIVET_STATUS_FILE` unset) both pass through
  unchanged.

So `claude` typed in a shell pane, `FOO=1 claude`, `env X=1 claude`,
`cd sub && claude` and launch-dialog commands all report status. Login
files often prepend to `PATH` (e.g. `~/.local/bin`, where Claude Code's
installer puts `claude`), so the shim dir is put back in front after them:

| Pane shell | How the shim dir stays first |
|---|---|
| any, with a command (`$SHELL -lc …`) | the command is prefixed with `PATH="$CIVET_SHIM_DIR:$PATH"; ` |
| zsh | `ZDOTDIR=<state_dir>/hooks/zsh`; its `.zshenv` restores your `ZDOTDIR` (from `CIVET_USER_ZDOTDIR`), sources your `.zshenv`, and adds a `precmd` hook; zsh then reads your other startup files as usual |
| bash | `PROMPT_COMMAND` re-prepends the shim dir before each prompt (an existing `PROMPT_COMMAND` from the daemon's environment is kept) |
| fish | `fish -l -C '…'` (runs after `config.fish`) |
| sh, dash, ksh, csh | `PATH` from the environment only |

Not covered: launchers that put their own directory first, e.g. `npx
claude` / `npx @anthropic-ai/claude-code` (npx prepends `node_modules/.bin`),
and absolute paths such as `/opt/bin/claude` typed in a shell. Launch
requests (argv form) with an explicit path to `claude`, or with the agent
hint `claude` on another program, get `--settings …` appended to their
argv instead. Otherwise wire the hooks manually (below).

The generated Claude settings register:

| Claude Code hook | Status |
|---|---|
| `UserPromptSubmit` | running (`prompt` mode: the prompt becomes the message, used as the session label) |
| `PreToolUse` (matcher `*`) | running, `soft` (never replaces a pending needs_input) |
| `PostToolUse` (matcher `*`) | running (e.g. after a permission was granted) |
| `Notification` | needs_input (message = notification text; idle/auth notifications ignored) |
| `Stop` | done |

To wire it manually instead (for example for `npx claude`), add the same
hooks to `~/.claude/settings.json` or the project's `.claude/settings.json`:

```json
{
  "hooks": {
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "~/.civet/hooks/civet-status-hook.sh running prompt" }] }],
    "PreToolUse":  [{ "matcher": "*", "hooks": [{ "type": "command", "command": "~/.civet/hooks/civet-status-hook.sh running soft" }] }],
    "PostToolUse": [{ "matcher": "*", "hooks": [{ "type": "command", "command": "~/.civet/hooks/civet-status-hook.sh running" }] }],
    "Notification": [{ "hooks": [{ "type": "command", "command": "~/.civet/hooks/civet-status-hook.sh needs_input" }] }],
    "Stop": [{ "hooks": [{ "type": "command", "command": "~/.civet/hooks/civet-status-hook.sh done" }] }]
  }
}
```

The script is a no-op outside Civet panes, so a global installation is safe.

## `civet` command

When the daemon names its binary (`SessionConfig::cli_program`; `civetd`
always does), the shim dir also holds `civet`, which runs that binary with
the daemon's `CIVET_STATE_DIR`. Agents use it for `civet handoff` (see
[docs/HANDOFF.md](../../docs/HANDOFF.md)).

## Codex (more signals, manual)

The daemon does not modify Codex's configuration. The `codex` PATH shim
(above) adds a `notify` override, so a finished turn reports `done` with
the last assistant message. On top of that, Codex panes (program basename
`codex`) get the heuristics: output activity → running, BEL followed by 1 s
of quiet → needs input, no output for `idle_after` (8 s) → done if the run
lasted at least 10 s, else idle; process exit → done.

Better signals, from simplest to most complete:

1. **Terminal notifications (OSC 9).** In `~/.codex/config.toml`:

   ```toml
   [tui]
   notifications = true
   ```

   Codex then emits desktop notifications as OSC 9 escape codes, which the
   daemon maps to `done`, or to `needs_input` when the text looks like a
   question or approval request (`permission`, `approve`, `allow`, `waiting`,
   `input`, `confirm`, or a trailing `?`).

2. **`notify` program (turn complete → done).** Codex runs the `notify`
   command after each turn with the event JSON appended as the last
   argument. In `~/.codex/config.toml`:

   ```toml
   notify = ["sh", "-c", "[ -n \"$CIVET_STATUS_FILE\" ] && printf 'done\\n' > \"$CIVET_STATUS_FILE\"; exit 0"]
   ```

   (`sh -c` receives the JSON as `$0` and ignores it.) With your own
   `notify` line the shim leaves Codex alone; to keep both, call your
   program and `~/.civet/hooks/civet-status-hook.sh done --codex-notify
   "$json"` from a small wrapper script.

3. **Codex hooks (`hooks.json`, recent Codex versions).** Codex supports
   Claude-style lifecycle hooks in `$CODEX_HOME/hooks.json` (default
   `~/.codex/hooks.json`) and asks you to trust them on first use:

   ```json
   {
     "hooks": {
       "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "~/.civet/hooks/civet-status-hook.sh running prompt" }] }],
       "PreToolUse": [{ "matcher": "*", "hooks": [{ "type": "command", "command": "~/.civet/hooks/civet-status-hook.sh running" }] }],
       "PermissionRequest": [{ "matcher": "*", "hooks": [{ "type": "command", "command": "~/.civet/hooks/civet-status-hook.sh needs_input" }] }],
       "PostToolUse": [{ "matcher": "*", "hooks": [{ "type": "command", "command": "~/.civet/hooks/civet-status-hook.sh running" }] }],
       "Stop": [{ "hooks": [{ "type": "command", "command": "~/.civet/hooks/civet-status-hook.sh done" }] }]
     }
   }
   ```

## Any other CLI

Anything that can run a shell command on an event can report status:

```sh
[ -n "$CIVET_STATUS_FILE" ] && printf 'done\nBuild finished\n' > "$CIVET_STATUS_FILE"
```

Programs can also print escape codes instead of writing the file:

| Escape code | Status |
|---|---|
| `ESC ] 9 ; <text> BEL` | done, or needs_input if `<text>` looks like a question |
| `ESC ] 777 ; notify ; <title> ; <text> BEL` | same, using `<text>` |
| `ESC ] 133 ; C BEL` (command started) | running |
| `ESC ] 133 ; D BEL` / `ESC ] 133 ; A BEL` (finished / prompt) | idle (non-agent panes only) |

A CLI that writes the hook file once becomes an agent pane automatically.
Without hooks, agent panes going quiet for `idle_after` after a run of at
least 10 s are reported `done` (shorter runs `idle`). Set `agent` in
`SpawnPane` / `LaunchAgent` (any non-empty string, e.g. the CLI name) to
enable the activity heuristics for a CLI that is not in the built-in
list (`claude`, `codex`, `gemini`, `aider`, `opencode`, `amp`,
`cursor-agent`, `goose`).

## Precedence

1. Process exit → done (final; afterwards only client overrides apply).
2. Hook file → as written; after a hook `running`/`needs_input`, heuristics are off for
   the pane except the stall (30 s → idle) and answered-prompt (→ running)
   fallbacks.
3. Escape codes (OSC 9 / 777 / 133); OSC 9 / 777 only in panes without hooks.
4. Heuristics (agent panes only).
5. `SetAgentStatus` from a client applies at any time.

The daemon emits `Event::AgentStatus` only when the status actually changes.
