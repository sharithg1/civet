# Handoff: from one agent to a new worktree + agent

An agent (Claude Code or Codex) running in a Civet pane can hand a side task
off: Civet creates a new git worktree of the same repo, starts a new agent
pane in it with a task brief as its first prompt, and the desktop app shows
both like any other worktree and pane (it follows the daemon's
`WorktreesChanged` / `PaneCreated` events; no refresh needed).

## Install the skill

Run once on the host where your agents run (the dev box for an ssh host,
your Mac for "This Mac"), from any Civet pane:

```sh
civet install-skills            # ~/.claude/skills/civet-handoff and ~/.codex/skills/civet-handoff
civet install-skills --claude   # only Claude Code
civet install-skills --codex    # only Codex (${CODEX_HOME:-~/.codex})
```

It writes the same `SKILL.md` (source:
[`crates/civetd/skills/civet-handoff/SKILL.md`](../crates/civetd/skills/civet-handoff/SKILL.md))
for both agents and prints the paths. Rerun it after upgrading Civet to pick
up a newer skill. Nothing is installed automatically: like the status hooks,
Civet does not write to `~/.claude` or `~/.codex` unless you ask.

## Use it

In a Claude Code or Codex session inside a Civet pane, ask for it:

> hand off fixing the flaky `udp_failover` test to a new agent

The skill has the agent write a self-contained brief (goal, context, relevant
files, constraints, "done when"), since the new session does not share the
conversation, then run `civet handoff` and report the branch, worktree and
pane.

## `civet handoff`

`civet` is a PATH shim every pane gets (`<state>/hooks/bin/civet`): it runs
the daemon's own `civetd` binary with that daemon's `CIVET_STATE_DIR`, so the
command reaches the daemon that runs the pane, also on a remote host where
only `civetd` is installed.

```sh
civet handoff --prompt-file brief.md [--agent claude|codex] [--title T]
              [--branch B] [--base REV] [--carry-changes] [--from DIR]
              [-- CMD ARGS…]
civet handoff --prompt "…"            # or --prompt-file - (stdin)
```

* **repo / base**: the git checkout of the current directory (`--from`); the
  new branch starts at its `HEAD` (`--base` overrides). The repo is
  registered with the daemon if it was not yet.
* **branch**: `--branch`, else a slug of the title (`fix-the-flaky-test`),
  with `-2`, `-3`… appended while that branch or worktree directory exists.
  An explicit `--branch` that exists is an error.
* **title**: `--title`, else the brief's first line (Markdown `#` stripped,
  60 characters max). It names the pane in the sidebar.
* **agent**: `claude` (default) or `codex`, started as `<agent> <brief>` in
  the new worktree through the usual shims (status hooks included).
  `-- CMD ARGS…` runs another command with the brief appended as its last
  argument.
* **--carry-changes**: applies the current checkout's uncommitted changes
  (tracked and untracked, not ignored; computed with `git diff`, so the
  index and stash are untouched) to the new worktree before the agent
  starts. If the patch does not apply, the new worktree is removed again.
* Output: one JSON line, e.g.
  `{"repo":"…","worktree":"/src/proj-worktrees/fix-the-flaky-test","branch":"fix-the-flaky-test","base":"3f2c…","pane":12,"title":"Fix the flaky test","agent":"claude","carriedChanges":false}`.

Errors (exit 1, message on stderr):

* `not running inside a Civet pane (CIVET_PANE_ID is not set)`: run it from
  an agent in a Civet pane (`--outside-pane` targets the daemon of
  `$CIVET_STATE_DIR` / `~/.civet` anyway).
* `no Civet daemon is reachable for <dir>`: no `daemon.json` or the socket
  does not answer. `civet handoff` never starts a daemon.
* git errors (not a checkout, no commits, branch exists, patch failed).

Sandboxes: the command connects to `<state>/civetd.sock` and reads
`<state>/daemon.json`. An agent sandbox that blocks unix sockets or reads
outside the workspace (e.g. Codex `workspace-write`) needs it run with
escalated permissions; the skill tells the agent so.

## Decisions

| Question (issue #7) | Answer |
|---|---|
| How does the skill reach the daemon? | A `civet` shim in the pane shim dir execs the daemon's binary (`civetd handoff`), which talks to `<state>/civetd.sock` with the key from `daemon.json`. No `civet-cli` on the host needed. |
| Where do the skill files live? | Embedded in `civetd`; `civet install-skills` writes them on request (no automatic writes to `~/.claude` / `~/.codex`). |
| Carry uncommitted changes? | Optional, `--carry-changes`. |
| Focus the new worktree in the app? | No: it appears in the sidebar (events), and its agent raises the usual status notifications. The current pane keeps focus. |
