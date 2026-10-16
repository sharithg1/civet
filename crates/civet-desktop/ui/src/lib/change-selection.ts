// Multi-selection in the Changes list: ⌘/Ctrl-click toggles a row, ⇧-click
// selects the range from the last plainly clicked (or toggled) row. A
// selection stays within one group (Staged, Changes…): clicking a row of
// another group starts over there.

import type { ChangeSide } from '@/state/changes'

export type ChangeSelection = { side: ChangeSide; paths: string[]; anchor: string } | null

/**
 * The selection after clicking `path` in group `side`, whose rows are
 * `order` (top to bottom).
 */
export function selectChange(
  sel: ChangeSelection,
  side: ChangeSide,
  order: string[],
  path: string,
  mod: { toggle?: boolean; range?: boolean } = {}
): ChangeSelection {
  const same = sel?.side === side
  if (mod.range && same && order.includes(sel.anchor)) {
    const a = order.indexOf(sel.anchor)
    const b = order.indexOf(path)
    const [from, to] = a < b ? [a, b] : [b, a]
    return { side, paths: order.slice(from, to + 1), anchor: sel.anchor }
  }
  if (mod.toggle && same) {
    const paths = sel.paths.includes(path)
      ? sel.paths.filter((p) => p !== path)
      : order.filter((p) => p === path || sel.paths.includes(p))
    return paths.length ? { side, paths, anchor: path } : null
  }
  return { side, paths: [path], anchor: path }
}

/** The selected paths of `side` that are still listed. */
export function selectedPaths(sel: ChangeSelection, side: ChangeSide, order: string[]): string[] {
  if (sel?.side !== side) return []
  return order.filter((p) => sel.paths.includes(p))
}
