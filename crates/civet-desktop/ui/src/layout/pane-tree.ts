// Split-tree model for terminal panes inside a tab.
// Ported (as a pure data model) from Orca: src/renderer/src/lib/pane-manager/
// pane-tree-ops.ts and pane-split-ratio.ts. Orca mutates DOM nodes directly;
// here the tree is immutable data rendered by components/SplitLayout.tsx.

import type { PaneId } from '../types'

export type SplitDirection = 'row' | 'column'

export type PaneNode =
  | { type: 'leaf'; pane: PaneId }
  | { type: 'split'; dir: SplitDirection; ratio: number; a: PaneNode; b: PaneNode }

export const MIN_RATIO = 0.1
export const MAX_RATIO = 0.9

export const leaf = (pane: PaneId): PaneNode => ({ type: 'leaf', pane })

export function leaves(node: PaneNode): PaneId[] {
  return node.type === 'leaf' ? [node.pane] : [...leaves(node.a), ...leaves(node.b)]
}

export function contains(node: PaneNode, pane: PaneId): boolean {
  return leaves(node).includes(pane)
}

/** Replace the `target` leaf with a split of [target, newPane] (new pane right / below). */
export function splitLeaf(
  node: PaneNode,
  target: PaneId,
  newPane: PaneId,
  dir: SplitDirection
): PaneNode {
  if (node.type === 'leaf') {
    return node.pane === target
      ? { type: 'split', dir, ratio: 0.5, a: node, b: leaf(newPane) }
      : node
  }
  return { ...node, a: splitLeaf(node.a, target, newPane, dir), b: splitLeaf(node.b, target, newPane, dir) }
}

/** Remove a leaf; its sibling takes the parent's place. Returns null if the tree is now empty. */
export function removeLeaf(node: PaneNode, pane: PaneId): PaneNode | null {
  if (node.type === 'leaf') return node.pane === pane ? null : node
  const a = removeLeaf(node.a, pane)
  const b = removeLeaf(node.b, pane)
  if (a === null) return b
  if (b === null) return a
  return a === node.a && b === node.b ? node : { ...node, a, b }
}

export type NodePath = ('a' | 'b')[]

export function setRatio(node: PaneNode, path: NodePath, ratio: number): PaneNode {
  if (node.type === 'leaf') return node
  if (path.length === 0) {
    return { ...node, ratio: Math.min(MAX_RATIO, Math.max(MIN_RATIO, ratio)) }
  }
  const [head, ...rest] = path
  return head === 'a'
    ? { ...node, a: setRatio(node.a, rest, ratio) }
    : { ...node, b: setRatio(node.b, rest, ratio) }
}

/** Neighbouring leaf in reading order (for focus moves after a close). */
export function siblingLeaf(node: PaneNode, pane: PaneId): PaneId | null {
  const all = leaves(node)
  const i = all.indexOf(pane)
  if (i < 0) return null
  return all[i + 1] ?? all[i - 1] ?? null
}
