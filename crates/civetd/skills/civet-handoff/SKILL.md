---
name: civet-handoff
description: Hand a task off to a new agent in a fresh git worktree via Civet, so it runs in parallel without touching the current branch. Use when the user asks to hand off, delegate, spin off or "send to a new worktree/agent" a side task (e.g. "hand this flaky test off", "spin up an agent to bump that dep"), and only when running inside a Civet pane (CIVET_PANE_ID is set).
---

# Hand off a task to a new Civet worktree

Civet can create a new git worktree of this repo and start a new agent
(Claude Code or Codex) in it with a prompt. The new pane shows up in the
Civet desktop app next to this one. Use `civet handoff`, which is on `PATH`
in every Civet pane.

## 1. Check you are in Civet

```sh
printf '%s\n' "${CIVET_PANE_ID:-not-in-civet}"
```

If it prints `not-in-civet`, stop and tell the user that handoff only works
from an agent running inside a Civet pane.

## 2. Write the task brief

The new agent does **not** see this conversation. Write a self-contained
brief to a temp file (e.g. `"${TMPDIR:-/tmp}/civet-handoff-$$.md"`), in
Markdown, with:

- **First line**: a short imperative title, e.g. `# Fix the flaky ports test`
  (it becomes the pane title and the branch name).
- **Goal**: what to do and why, in a few sentences.
- **Context**: what you already know: relevant files (repo-relative paths,
  functions, line numbers), error messages, commands that reproduce the
  problem, decisions the user already made.
- **Constraints**: what not to change, conventions to follow, how to test.
- **Done when**: concrete acceptance criteria (tests pass, behavior X,
  commit on the branch, open a PR or not: say what the user asked for).

Keep it as long as it needs to be and no longer. Do not paste secrets.

## 3. Hand off

```sh
civet handoff --agent <claude|codex> --prompt-file "<brief file>"
```

- `--agent`: `claude` if you are Claude Code, `codex` if you are Codex,
  unless the user asked for the other one.
- Optional: `--title "<pane title>"` (default: the brief's first line),
  `--branch <name>` (default: a slug of the title, `-2`, `-3`… if taken),
  `--base <rev>` (default: this worktree's `HEAD`),
  `--carry-changes` (copy this worktree's uncommitted changes, including
  untracked files, into the new one; only when the user asks for it or the
  task depends on them).

If `civet` is not found, use `"$CIVET_SHIM_DIR/civet"`. If the command fails
with a permission or sandbox error (it connects to the Civet daemon's unix
socket in `~/.civet`), rerun it with the permissions needed to leave the
sandbox.

On success it prints JSON like:

```json
{"repo":"…","worktree":"/path/to/repo-worktrees/fix-the-flaky-ports-test",
 "branch":"fix-the-flaky-ports-test","base":"3f2c…","pane":12,
 "title":"Fix the flaky ports test","agent":"claude","carriedChanges":false}
```

## 4. Report back

Tell the user, briefly: the branch, the worktree path and the pane id, and
that the new agent is working in the Civet app. Then delete the temp brief
file and continue with the original work. Do not wait for the new agent.

On failure, show the error message as is. Common ones: not inside a Civet
pane; no Civet daemon reachable (the app is not connected to this host);
`--carry-changes` patch did not apply.
