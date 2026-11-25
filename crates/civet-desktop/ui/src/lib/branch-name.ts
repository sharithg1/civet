// Branch names made from a task, so "New worktree" needs only the task: the
// same slug `civet handoff` makes from its title (civetd/src/handoff.rs `slug`).

/** Longest generated branch name (handoff's MAX_SLUG). */
const MAX_SLUG = 48

/**
 * A task's branch name: lowercase ASCII words joined by `-`, whole words up
 * to {@link MAX_SLUG} characters; empty when the task has no ASCII word.
 */
export function branchSlug(task: string): string {
  let out = ''
  for (const raw of task.split(/[^A-Za-z0-9]+/)) {
    if (!raw) continue
    const word = raw.toLowerCase()
    const sep = out ? 1 : 0
    if (out.length + sep + word.length > MAX_SLUG) {
      if (!out) out = word.slice(0, MAX_SLUG)
      break
    }
    out += (sep ? '-' : '') + word
  }
  return out
}

/** `stem`, else `stem-2`, `stem-3`… : the first that `taken` lacks. */
export function freeBranch(stem: string, taken: ReadonlySet<string>): string {
  if (!taken.has(stem)) return stem
  let n = 2
  while (taken.has(`${stem}-${n}`)) n++
  return `${stem}-${n}`
}
