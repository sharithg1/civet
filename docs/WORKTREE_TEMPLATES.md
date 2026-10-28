# Worktree templates

Per-project settings applied to every new worktree, so `pnpm install`,
copying `.env` files and picking a dev-server port stop being manual steps.
A template has four optional fields:

| Field | What it does |
|---|---|
| `setup` | A script run in a **Setup** tab of the new worktree. Plain scripts run with `sh -e` (stop at the first failing command, so the tab's exit code says whether setup worked); a script starting with `#!` runs with that interpreter. |
| `copy` | Paths, relative to the main checkout, copied into the new worktree before anything starts: files or directories, `*` and `?` match within one path component (`apps/*/.env`; `*` skips dotfiles unless the pattern starts with `.`). Files the worktree already has are kept; `..`, absolute paths and `.git` are refused, and nothing is read or written through a symlinked directory of either checkout (a symlink itself is copied as a link). |
| `agent` | Command line of the first agent, e.g. `claude --model opus` or `codex --full-auto`. |
| `prompt` | Default first prompt: prefills the New worktree dialog's first task, and is put (plus a blank line) before a handoff's brief. |

## Where it is configured, and precedence

1. **Settings → Projects → Worktree setup…** in the app. Saved on the host
   (`<state>/worktree-templates.json`), not in the app, so `civet handoff`
   on that host uses it too. Personal: not committed.
2. **`.civet/worktree.toml`** in the repository's main checkout, committed
   and shared with the team:

   ```toml
   setup = """
   pnpm install
   cp "$CIVET_MAIN_CHECKOUT/.env.development" .env.development
   """
   copy = [".env", "apps/*/.env.local"]   # or one string
   agent = "claude --model opus"
   prompt = "Read CONTRIBUTING.md before you start."
   ```

**Field by field, the first source that sets a field wins**: a field saved
in Settings overrides the same field of the repository file, the other
fields still come from the file. Why this order: the file holds team
defaults, Settings is where one person adjusts them without editing a
committed file. In Settings, a field left empty falls back to the file (its
value is shown as the placeholder); through the protocol an empty string /
list is "set to nothing" and also overrides the file.

The repository file is read from the **main checkout** (not the new
worktree), so uncommitted edits to it apply right away and every branch gets
the same template. Unknown keys are ignored; a file that does not parse is
reported (Settings, a warning toast, `civet handoff` stderr) and skipped.

The app's own "Save as default for <project>" on-create steps (stored in the
app) still decide the dialog's steps when saved; the template's `agent` is
the default only for projects without saved steps.

## What happens on create

New worktree dialog (when the project has a `setup` or `copy`, a **Setup**
row shows them; untick it to skip) and `civet handoff` (`--no-setup` skips):

1. `CreateWorktree`.
2. `SetupWorktree`: copy the `copy` paths, assign the worktree's
   `CIVET_PORT`, start the setup script in a pane titled "Setup".
3. The agent(s) start (in parallel with the setup script; they see the
   copied files).

The Setup tab sits left of the agent's tab. When the script exits the pane
stays, with `[process exited with code N]`; a non-zero exit from a setup the
app started also raises an error toast that jumps to the tab. Copy problems
(a listed file missing from the main checkout) come back as warnings.

Environment of the setup script: `CIVET_WORKTREE` (the new worktree),
`CIVET_MAIN_CHECKOUT`, `CIVET_BRANCH`, `CIVET_PORT`, cwd = the worktree.

## `CIVET_PORT`

Every worktree gets a TCP port on the host, set as `CIVET_PORT` in **all**
of its panes (not just setup), stable for the worktree's lifetime
(`<state>/worktree-ports.json`) and released when it is removed through
Civet. Ports are handed out in blocks of ten from 41000 (41000, 41010, …
49990): `CIVET_PORT` … `CIVET_PORT + 9` belong to that worktree, and a block
is only given out if no other worktree has it and all ten ports are free at
that moment. Use it in scripts or tell agents about it, e.g.
`pnpm dev --port $CIVET_PORT`.

## Protocol

Appended within protocol 1 (clients check
`WORKTREE_TEMPLATE_MIN_DAEMON_VERSION` = 0.4.2 first):
`GetWorktreeTemplate{repo}` / `SetWorktreeTemplate{repo, template}` →
`WorktreeTemplate(WorktreeTemplateInfo{repo, host, filePath, file,
fileError, effective})`; `SetupWorktree{worktree, size}` →
`WorktreeSetUp(WorktreeSetup{template, copied, warnings, port,
setupPane})`. Desktop commands: `worktree_template`,
`set_worktree_template`, `setup_worktree`.

## Not done (yet)

* The agent does not wait for the setup script to finish.
* Settings cannot set a field to "nothing" over the repository file (the
  protocol can; write an empty value there).
* A setup started by `civet handoff` shows its failure in the Setup tab
  only (no toast: the app did not start it).
