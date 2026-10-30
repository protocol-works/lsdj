import { describe, expect, it } from 'vitest'

import { seedState } from './fixture'
import { loomReducer, type LoomState } from './loomState'
import { renderAheadCandidate } from './renderAhead'

const onDeckA = (id: string, state: LoomState = seedState()) =>
  loomReducer(state, { type: 'landed', deck: 'a', id })

describe('renderAheadCandidate', () => {
  it("grows behind the deck's node when it is a ready leaf without a LoRA stack", () => {
    expect(renderAheadCandidate(onDeckA('n6'), 'a', new Set())?.id).toBe('n6')
  })

  it('offers nothing for a deck with no node', () => {
    expect(renderAheadCandidate(seedState(), 'a', new Set())).toBeNull()
  })

  it('skips a LoRA node', () => {
    expect(renderAheadCandidate(onDeckA('n4'), 'a', new Set())).toBeNull()
  })

  it('skips a node with children', () => {
    expect(renderAheadCandidate(onDeckA('n2'), 'a', new Set())).toBeNull()
  })

  it('skips a node whose render is in flight', () => {
    expect(renderAheadCandidate(onDeckA('n6'), 'a', new Set(['n6']))).toBeNull()
  })

  it('skips a node that is still rendering', () => {
    const state = onDeckA('n6')
    const rendering = {
      ...state,
      loom: {
        ...state.loom,
        nodes: state.loom.nodes.map((n) => (n.id === 'n6' ? { ...n, status: 'rendering' as const } : n)),
      },
    }
    expect(renderAheadCandidate(rendering, 'a', new Set())).toBeNull()
  })
})
