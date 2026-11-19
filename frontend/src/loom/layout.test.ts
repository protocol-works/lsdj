import { describe, expect, it } from 'vitest'

import { fixtureNode, seedLoom } from './fixture'
import { fitPxPerBar, LAYOUT, layoutLoom, zoomAnchored } from './layout'

const rows = (layout: ReturnType<typeof layoutLoom>) =>
  Object.fromEntries([...layout.boxes].map(([id, box]) => [id, box.row]))

describe('layoutLoom', () => {
  it('starts a child at its parent’s end', () => {
    const { boxes } = layoutLoom(seedLoom().nodes, 20)
    const root = boxes.get('r1')!
    expect(root).toMatchObject({ x: LAYOUT.padX, w: 16 * 20 - LAYOUT.gap })
    expect(boxes.get('n2')!.x).toBe(root.x + root.w + LAYOUT.gap)
    expect(boxes.get('n3')!.x).toBe(LAYOUT.padX + 32 * 20)
  })

  it('hands out rows depth-first, a node sharing its first child’s row, with a blank row between roots', () => {
    const layout = layoutLoom(seedLoom().nodes, 20)
    expect(rows(layout)).toEqual({ r1: 0, n2: 0, n3: 0, n4: 0, n5: 1, n6: 1, n7: 2, r2: 4, n9: 4, n10: 4, r3: 6 })
    expect(layout.boxes.get('n7')!.y).toBe(LAYOUT.padY + 2 * LAYOUT.row)
    expect(layout.height).toBe(LAYOUT.padY + 7 * LAYOUT.row)
  })

  it('lists an edge from each parent to each child', () => {
    const { edges } = layoutLoom(seedLoom().nodes, 20)
    expect(edges).toHaveLength(8)
    expect(edges).toContainEqual({ from: 'n2', to: 'n5' })
  })

  it('gives take siblings adjacent rows even when a take has grown a branch', () => {
    const take = (id: string) => fixtureNode(id, 'n5', 'take', 16, 124, { takeGroup: 'g' })
    const nodes = [
      ...seedLoom().nodes,
      take('t1'),
      take('t2'),
      take('t3'),
      fixtureNode('c1', 't1', 'on', 16, 124),
      fixtureNode('c2', 't1', 'branch', 16, 124),
    ]
    const r = rows(layoutLoom(nodes, 20))
    expect([r.t1, r.t2, r.t3]).toEqual([r.n6 + 1, r.n6 + 2, r.n6 + 3])
    expect(r.c1).toBe(r.t1)
    expect(r.c2).toBe(r.t3 + 1)
    expect(r.n7).toBe(r.c2 + 1)
  })

  it('narrows a trimmed node and closes the tree up behind it', () => {
    const nodes = seedLoom().nodes.map((n) => (n.id === 'r1' ? { ...n, in: 4 } : n))
    const { boxes } = layoutLoom(nodes, 20)
    expect(boxes.get('r1')!.w).toBe(12 * 20 - LAYOUT.gap)
    expect(boxes.get('n2')!.x).toBe(LAYOUT.padX + 12 * 20)
  })

  it('draws a tiny node at least 24 px wide without shifting its children in time', () => {
    const nodes = [fixtureNode('a', null, 'a', 4, 120), fixtureNode('b', 'a', 'b', 4, 120)]
    const { boxes } = layoutLoom(nodes, 6)
    expect(boxes.get('a')!.w).toBe(24)
    expect(boxes.get('b')!.x).toBe(LAYOUT.padX + 4 * 6)
  })

  it('lays out an empty loom as just the padding', () => {
    expect(layoutLoom([], 20)).toEqual({ boxes: new Map(), edges: [], width: LAYOUT.padX, height: LAYOUT.padY })
  })

  it('reports the right-most edge as the width', () => {
    const { width, boxes } = layoutLoom(seedLoom().nodes, 20)
    const n4 = boxes.get('n4')!
    expect(width).toBe(n4.x + n4.w)
  })
})

describe('zoom', () => {
  it('fits the longest root-to-leaf run (72 bars) into the viewport', () => {
    expect(fitPxPerBar(seedLoom().nodes, 1000)).toBe(7)
  })

  it('clamps fit to 6–48 px per bar', () => {
    expect(fitPxPerBar(seedLoom().nodes, 100000)).toBe(48)
    expect(fitPxPerBar(seedLoom().nodes, 300)).toBe(6)
    expect(fitPxPerBar([], 1000)).toBe(20)
  })

  it('zooms about an anchor bar, returning the scroll that keeps it still', () => {
    expect(zoomAnchored(20, 1.25, 10)).toEqual({ px: 25, scrollDelta: 50 })
    expect(zoomAnchored(20, 0.8, 10)).toEqual({ px: 16, scrollDelta: -40 })
  })

  it('stops zooming at the range ends', () => {
    expect(zoomAnchored(48, 1.25, 10)).toEqual({ px: 48, scrollDelta: 0 })
    expect(zoomAnchored(6, 0.8, 10)).toEqual({ px: 6, scrollDelta: 0 })
  })
})
