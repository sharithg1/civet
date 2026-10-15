# Agent status hooks (`$CIVET_STATUS_FILE`)

Every Civet pane gets two environment variables:

| Variable | Value |
|---|---|
| `CIVET_PANE_ID` | the pane id (decimal) |
| `CIVET_STATUS_FILE` | `<state_dir>/status/<pane>.status`, created empty at spawn |

`civetd` polls that file (mtime + length, every 250 ms). Whenever it changes,
the whole file is parsed with `civet_proto::parse_status_file` and the pane's
agent status is set with source `Hook`. Once a pane has received a hook
status, the output/idle heuristics stop applying to it (escape codes, process
exit and client overrides still do).

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
civet-status-hook.sh <running|needs_input|done|idle>
```

It reads the hook event JSON from stdin (if any), uses its `"message"` field
as the status message, writes `$CIVET_STATUS_FILE` atomically and always
exits 0, so it can never block or fail an agent. It does nothing when
`CIVET_STATUS_FILE` is unset. Claude Code `Notification` events with
`"notification_type": "idle_prompt"` (the "Claude is waiting for your input"
reminder after a finished turn) are ignored so they do not turn `done` into
`needs_input`.

## Claude Code (automatic)

Nothing to do. When a pane runs `claude` (program basename `claude`, or
`agent: "claude"` in the spawn/launch request) and the arguments do not
already contain `--settings`, the daemon appends

```
--settings ~/.civet/hooks/claude-settings.json
```

Claude Code merges these settings with the user's own, so existing hooks keep
working. The generated file registers:

| Claude Code hook | Status |
|---|---|
| `UserPromptSubmit` | running |
| `PreToolUse` (matcher `*`) | running |
| `PostToolUse` (matcher `*`) | running (e.g. after a permission was granted) |
| `Notification` | needs_input (message = notification text) |
| `Stop` | done |

To wire it manually instead (for example in a shell pane where you start
`claude` yourself with your own `--settings`), add the same hooks to
`~/.claude/settings.json` or the project's `.claude/settings.json`:

```json
{
  "hooks": {
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "~/.civet/hooks/civet-status-hook.sh running" }] }],
    "PreToolUse":  [{ "matcher": "*", "hooks": [{ "type": "command", "command": "~/.civet/hooks/civet-status-hook.sh running" }] }],
    "PostToolUse": [{ "matcher": "*", "hooks": [{ "type": "command", "command": "~/.civet/hooks/civet-status-hook.sh running" }] }],
    "Notification": [{ "hooks": [{ "type": "command", "command": "~/.civet/hooks/civet-status-hook.sh needs_input" }] }],
    "Stop": [{ "hooks": [{ "type": "command", "command": "~/.civet/hooks/civet-status-hook.sh done" }] }]
  }
}
```

The script is a no-op outside Civet panes, so a global installation is safe.

## Codex (manual)

The daemon does not modify Codex's configuration. Without any setup, Codex
panes (program basename `codex`) still get the heuristics: output activity →
running, BEL followed by 1 s of quiet → needs input, no output for
`idle_after` (8 s) → idle, process exit → done.

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

   (`sh -c` receives the JSON as `$0` and ignores it.) This replaces any
   existing `notify` setting; if you already use one, call both from a small
   wrapper script.

3. **Codex hooks (`hooks.json`, recent Codex versions).** Codex supports
   Claude-style lifecycle hooks in `$CODEX_HOME/hooks.json` (default
   `~/.codex/hooks.json`) and asks you to trust them on first use:

   ```json
   {
     "hooks": {
       "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "~/.civet/hooks/civet-status-hook.sh running" }] }],
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

Set `agent` in `SpawnPane` / `LaunchAgent` (any non-empty string, e.g. the CLI
name) to enable the activity heuristics for a CLI that is not in the built-in
list (`claude`, `codex`, `gemini`, `aider`, `opencode`, `amp`,
`cursor-agent`, `goose`).

## Precedence

1. Process exit → done (final; afterwards only client overrides apply).
2. Hook file → as written; disables heuristics for the pane from then on.
3. Escape codes (OSC 9 / 777 / 133).
4. Heuristics (agent panes only).
5. `SetAgentStatus` from a client applies at any time.

The daemon emits `Event::AgentStatus` only when the status actually changes.
