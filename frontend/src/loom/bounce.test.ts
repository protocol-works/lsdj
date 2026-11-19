import i18n from 'i18next'
import { describe, expect, it } from 'vitest'

import { deckPath, nodePath, pathParts } from './bounce'
import { seedState } from './fixture'
import { loomReducer, type LoomState } from './loomState'
import { barSeconds, TAIL_BARS } from './timing'

const bar = barSeconds(124)
const tail = TAIL_BARS * bar
const t = i18n.t.bind(i18n)

describe('pathParts', () => {
  it('bounces a node from its root, trims applied', () => {
    const state = loomReducer(seedState(), { type: 'trim', id: 'n4', in: 2, out: 10 })
    const ids = nodePath(state, 'n4')
    expect(ids).toEqual(['r1', 'n2', 'n3', 'n4'])
    const bounce = pathParts(state, ids, t)!
    expect(bounce.parts.map((part) => part.name)).toEqual(['r1.wav', 'n2.wav', 'n3.wav', 'n4.wav'])
    expect(bounce.parts[0].startSeconds).toBe(0)
    expect(bounce.parts[0].endSeconds).toBeCloseTo(16 * bar)
    expect(bounce.parts[1].startSeconds).toBeCloseTo(tail)
    expect(bounce.parts[2].endSeconds).toBeCloseTo(tail + 24 * bar)
    expect(bounce.parts[3].startSeconds).toBeCloseTo(tail + 2 * bar)
    expect(bounce.parts[3].endSeconds).toBeCloseTo(tail + 10 * bar)
    expect(bounce.title).toBe('Late dub · acid techno, 303 squelch, peak time')
    expect(bounce.prompt).toBe(
      'ambient dub, tape delay, submerged → dub techno, rolling bassline → ' +
        'deep minimal techno, hypnotic, sub pulse → acid techno, 303 squelch, peak time',
    )
  })

  it('bounces what a deck played up to its playhead, dropping nodes gone since', () => {
    const seeded = seedState()
    const state: LoomState = {
      ...seeded,
      decks: { ...seeded.decks, a: { ...seeded.decks.a, history: ['r1', 'gone'], nodeId: 'n2' } },
    }
    const ids = deckPath(state, 'a')
    expect(ids).toEqual(['r1', 'gone', 'n2'])
    const bounce = pathParts(state, ids, t, tail + 3 * bar)!
    expect(bounce.parts.map((part) => part.name)).toEqual(['r1.wav', 'n2.wav'])
    expect(bounce.parts[1].endSeconds).toBeCloseTo(tail + 3 * bar)
    // A playhead still in the kept tail leaves the current node nothing to add.
    expect(pathParts(state, ids, t, tail / 2)!.parts.map((part) => part.name)).toEqual(['r1.wav'])
    expect(deckPath(state, 'b')).toEqual([])
  })

  it('skips unrendered nodes and names an untitled loom', () => {
    const seeded = seedState()
    const state: LoomState = {
      ...seeded,
      loom: {
        ...seeded.loom,
        name: '',
        nodes: seeded.loom.nodes.map((node) => (node.id === 'n2' ? { ...node, file: null, status: 'rendering' } : node)),
      },
    }
    const bounce = pathParts(state, ['r1', 'n2'], t)!
    expect(bounce.parts.map((part) => part.name)).toEqual(['r1.wav'])
    expect(bounce.title).toBe('Untitled loom · ambient dub, tape delay, submerged')
    expect(pathParts(state, ['n2'], t)).toBeNull()
  })
})
