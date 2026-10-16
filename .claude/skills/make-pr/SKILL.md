---
name: make-pr
description: Open a pull request for the current work in the Civet repo — branch, run the checks CI runs, commit, push, and `gh pr create` with a title, body and exactly one release-notes label. Use when the user asks to make, open, create or raise a PR, or to "ship" / "send up" a change for review.
---

# Make a pull request

Release notes are generated from PR labels (`.github/release.yml`), so the
label you pick decides which section of the next release the change lands
in. Every PR gets exactly one label from the table in step 5.

## 1. Get on a branch

```sh
git status --short --branch
```

If you are on `main`, create a branch first: `git switch -c <short-slug>`
(e.g. `pane-reattach-race`, or `civet/issues/<n>` when it fixes issue `<n>`).
Never push to `main`. If the branch already has an open PR
(`gh pr view --json url,state`), push to it instead of opening another.

## 2. Run the checks CI runs

Run only the groups that match what changed, and fix failures before going on:

| Changed | Run |
| --- | --- |
| any Rust | `cargo fmt --all --check` and `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` |
| Rust behaviour | `cargo nextest run --workspace --all-features --locked` (falls back to `cargo test` if nextest is missing) |
| `crates/civet-desktop/ui` | in that dir: `pnpm typecheck`, `pnpm lint`, `pnpm test` |
| versions | `scripts/check-version.sh` |

If a check can't be run here (no toolchain, too slow, needs a real
Claude/Codex session), say so under **Testing** in the PR body.

## 3. Commit

Stage only the files that belong to this change; look at `git status`
before staging and leave unrelated edits out. Commit subjects follow the
repo's style: imperative, with an area prefix when it helps
(`Desktop: …`, `Handoff: …`, `e2e: …`), no trailing period, ≤ 72 chars.

## 4. Push

```sh
git push -u origin HEAD
```

## 5. Pick one label

| Label | Use for | Release notes section |
| --- | --- | --- |
| `breaking-change` | Users must do something on upgrade: changed CLI flags, protocol/state-dir format, removed features | Breaking changes |
| `enhancement` | New user-visible feature or behaviour | Features |
| `bug` | Fixes something that was broken | Fixes |
| `performance` | Faster or lighter, same behaviour | Performance |
| `documentation` | Docs/README only | Documentation |
| `dependencies` | Dependency or toolchain bumps | Dependencies |
| `maintenance` | Refactors, CI, build, tests, tooling — nothing a user would notice | Maintenance |
| `skip-changelog` | Release commits, reverts of unreleased work, noise | (left out) |

If a PR fits several, pick the one a user cares about most:
`breaking-change` > `enhancement` > `bug` > `performance` > the rest. If a
PR genuinely needs two sections, it is probably two PRs — mention that to
the user rather than adding two labels.

## 6. Open the PR

The title is the line that appears in the release notes, so write it for a
user reading them: what changed, not how (`Desktop: resizable sidebar`, not
`Refactor Sidebar.tsx`).

```sh
gh pr create --base main --label <label> --title "<title>" --body-file - <<'EOF'
Closes #<n>.

## What
- <user-visible change, one bullet each>

## Testing
- <checks run and their result>
- Not done: <anything skipped, and why>

## Notes
- <follow-ups, upgrade steps, risks — drop this section if empty>
EOF
```

Drop the `Closes` line if there's no issue. Add the PR-description
attribution line from your instructions at the end of the body if you have one.

If `gh` says the label doesn't exist, create the whole set (safe to re-run):

```sh
gh label create breaking-change --color b60205 --description "Requires action on upgrade" --force
gh label create performance     --color fbca04 --description "Faster or lighter, same behaviour" --force
gh label create dependencies    --color 0366d6 --description "Dependency or toolchain bumps" --force
gh label create maintenance     --color c5def5 --description "Refactors, CI, build, tests, tooling" --force
gh label create skip-changelog  --color ededed --description "Leave out of release notes" --force
```

## 7. Report

Reply with the PR URL, the label you chose and why, and anything listed
under "Not done".
