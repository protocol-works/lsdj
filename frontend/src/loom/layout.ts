/** Loom layout (plan §5, from the mockup's layout pass): left to right is
 * musical time, down is alternatives. A node is as wide as its trimmed body,
 * a child starts where its parent's out-point is, a node shares a row with
 * its first child, and rows are handed out depth-first — with a take group's
 * members on adjacent rows so its bracket encloses only takes. */

import { kids, pathTo, type LoomNode } from './loomState'

export const LAYOUT = { gap: 18, row: 84, padX: 190, padY: 52 }
export const PX_MIN = 6
export const PX_MAX = 48
export const PX_DEFAULT = 20
/** What fit leaves for the left pad, the weave slot right of the leaves (236)
 * and a margin — the mockup's numbers. */
const FIT_RESERVED_PX = LAYOUT.padX + 236 + 40

export type NodeBox = { x: number; y: number; w: number; row: number }

export type LoomLayout = {
  boxes: Map<string, NodeBox>
  edges: { from: string; to: string }[]
  /** The content extent (right-most node edge, bottom of the last row); the
   * view adds its own room for the weave slot. */
  width: number
  height: number
}

const bodyBars = (n: LoomNode) => n.out - n.in

export function layoutLoom(nodes: LoomNode[], pxPerBar: number, opts = LAYOUT): LoomLayout {
  const boxes = new Map<string, NodeBox>()
  const edges: { from: string; to: string }[] = []
  let rows = 0

  const place = (node: LoomNode, row: number, x: number) => {
    const span = bodyBars(node) * pxPerBar
    boxes.set(node.id, { x, y: opts.padY + row * opts.row, w: Math.max(24, span - opts.gap), row })
    const children = kids(nodes, node.id)
    const rowOf = new Map<string, number>()
    children.forEach((child, i) => {
      // Reserve rows for a whole take group at its first member, so the
      // takes stay adjacent and their own subtrees grow below the group.
      if (!rowOf.has(child.id)) {
        const group = child.takeGroup ? children.filter((c) => c.takeGroup === child.takeGroup) : [child]
        group.forEach((c, j) => rowOf.set(c.id, i === 0 && j === 0 ? row : rows++))
      }
      edges.push({ from: node.id, to: child.id })
      // Time, not the drawn width: the child starts at the parent's out-point
      // even when a tiny node is drawn wider than its bars.
      place(child, rowOf.get(child.id)!, x + span)
    })
  }

  nodes
    .filter((n) => n.parentId === null)
    .forEach((root, i) => {
      if (i) rows++ // a blank row between roots
      place(root, rows++, opts.padX)
    })

  let width = opts.padX
  for (const box of boxes.values()) width = Math.max(width, box.x + box.w)
  return { boxes, edges, width, height: opts.padY + rows * opts.row }
}

/** Whole pixels per bar inside the zoom range. */
export const clampPx = (px: number) =>
  Number.isFinite(px) ? Math.max(PX_MIN, Math.min(PX_MAX, Math.round(px))) : PX_DEFAULT

/** The zoom that fits the longest root-to-leaf run into the viewport. */
export function fitPxPerBar(nodes: LoomNode[], viewportWidth: number): number {
  let bars = 0
  for (const n of nodes) bars = Math.max(bars, pathTo(nodes, n.id).reduce((sum, p) => sum + bodyBars(p), 0))
  return bars > 0 ? clampPx((viewportWidth - FIT_RESERVED_PX) / bars) : PX_DEFAULT
}

/** Zoom by `factor` keeping `anchorBar` (the bar under the cursor, measured
 * from the left pad) still on screen: add `scrollDelta` to the scroll offset. */
export function zoomAnchored(px: number, factor: number, anchorBar: number) {
  const next = clampPx(px * factor)
  return { px: next, scrollDelta: anchorBar * (next - px) }
}
