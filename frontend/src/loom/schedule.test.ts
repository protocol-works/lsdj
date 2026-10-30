import { describe, expect, it } from 'vitest'

import { fixtureNode, seedState } from './fixture'
import { loomReducer, nodeById, type DeckRuntime, type LoomAction, type LoomState } from './loomState'
import { cutPlan, CUT_LEAD_SECONDS, deckSeam, planDeck } from './schedule'
import { barSeconds, holdRegion, loopRegion, nodeEndSeconds, nodeStartSeconds } from './timing'

const BAR = barSeconds(124)
const node = (state: LoomState, id: string) => nodeById(state.loom.nodes, id)!
const onA = (patch: Partial<DeckRuntime>, state = seedState()): LoomState => ({
  ...state,
  decks: { ...state.decks, a: { ...state.decks.a, nodeId: 'n2', ...patch } },
})
const reduce = (state: LoomState, ...actions: LoomAction[]) => actions.reduce(loomReducer, state)

describe('planDeck', () => {
  it('is idle when the deck has no node', () => {
    expect(planDeck(seedState(), 'a')).toEqual({ kind: 'idle' })
  })

  it('holds the last four bars at an untied seam', () => {
    const state = onA({})
    expect(planDeck(state, 'a')).toEqual({ kind: 'hold', loop: holdRegion(node(state, 'n2')) })
  })

  it('queues a ready tie at the seam and keeps the hold armed under it', () => {
    const state = onA({ queue: ['n5'] })
    const n2 = node(state, 'n2')
    const plan = planDeck(state, 'a')
    expect(plan).toEqual({
      kind: 'queue',
      nodeId: 'n5',
      file: 'n5.wav',
      startSeconds: nodeStartSeconds(node(state, 'n5')),
      switchAt: nodeEndSeconds(n2),
      loop: holdRegion(n2),
    })
    // The engine swaps before it folds: the hold ends exactly at the seam.
    if (plan.kind === 'queue') expect(plan.loop.end).toBe(plan.switchAt)
  })

  it('holds while the tie is still rendering, then queues it once it lands', () => {
    const woven = reduce(onA({}), {
      type: 'weave',
      parentId: 'n2',
      prompt: 'acid',
      takes: 1,
      bars: 16,
      bpm: 124,
      loras: [],
      sa3: {},
      seeds: [1],
      ids: ['w1'],
      takeGroup: 'g',
      auto: false,
      now: 0,
    }, { type: 'tie', deck: 'a', id: 'w1' })
    expect(planDeck(woven, 'a').kind).toBe('hold')

    const ready = loomReducer(woven, { type: 'rendered', id: 'w1', file: 'w1.wav', durationSeconds: 20 * BAR })
    expect(planDeck(ready, 'a')).toMatchObject({ kind: 'queue', nodeId: 'w1', file: 'w1.wav' })
  })

  it('holds on a failed or missing tie', () => {
    const failed = reduce(onA({ queue: ['n5'] }), { type: 'render_failed', id: 'n5', error: 'boom' })
    expect(planDeck(failed, 'a').kind).toBe('hold')
    const missing = reduce(onA({ queue: ['n5'] }), { type: 'file_missing', file: 'n5.wav' })
    expect(planDeck(missing, 'a').kind).toBe('hold')
  })

  it('takes the first ready child on autopilot, and only on autopilot', () => {
    expect(planDeck(onA({ auto: true }), 'a')).toMatchObject({ kind: 'queue', nodeId: 'n3' })
    expect(planDeck(onA({ auto: false }), 'a').kind).toBe('hold')
    // A leaf on autopilot has nothing to take: it holds.
    expect(planDeck(onA({ nodeId: 'n4', auto: true }), 'a').kind).toBe('hold')
  })

  it('prefers the tie over the autopilot child', () => {
    expect(planDeck(onA({ auto: true, queue: ['n7'] }), 'a')).toMatchObject({ kind: 'queue', nodeId: 'n7' })
  })

  it('runs the loom loop over a ready tie', () => {
    const state = onA({ queue: ['n5'], loopBars: 8, loopStartBar: 4 })
    expect(planDeck(state, 'a')).toEqual({ kind: 'loop', loop: loopRegion(node(state, 'n2'), 4, 8) })
  })

  it('lets a loop the plan did not place win over everything', () => {
    const userLoop = { start: 10, end: 12 }
    const state = onA({ queue: ['n5'], loopBars: 8, loopStartBar: 4 })
    expect(planDeck(state, 'a', userLoop)).toEqual({ kind: 'loop', loop: userLoop })
  })

  it('clamps the seam and the hold to a clip shorter than its nominal bars', () => {
    const short = fixtureNode('s1', null, 'short', 16, 124, { durationSeconds: 15.5 * BAR })
    const state = reduce(seedState(), { type: 'loom_loaded', loom: { ...seedState().loom, nodes: [short, fixtureNode('s2', 's1', 'next', 16, 124)] } })
    const tied = { ...state, decks: { ...state.decks, a: { ...state.decks.a, nodeId: 's1', queue: ['s2'] } } }
    const plan = planDeck(tied, 'a')
    expect(plan).toMatchObject({ kind: 'queue', switchAt: 15.5 * BAR })
    if (plan.kind === 'queue') expect(plan.loop).toEqual({ start: 11.5 * BAR, end: 15.5 * BAR })
  })

  it('never leaves a deck with a node without a loop (R2)', () => {
    const base = seedState()
    for (const n of base.loom.nodes) {
      for (const queue of [[], ['n5'], ['n3', 'n4']]) {
        for (const auto of [false, true]) {
          for (const loopBars of [null, 4, 16] as const) {
            const plan = planDeck(onA({ nodeId: n.id, queue, auto, loopBars, loopStartBar: n.in }, base), 'a')
            expect(plan.kind).not.toBe('idle')
            if (plan.kind !== 'idle') expect(plan.loop.end).toBeLessThanOrEqual(n.durationSeconds!)
          }
        }
      }
    }
  })
})

describe('cutPlan', () => {
  it('hands over on the next bar line, entering the target at its in-point', () => {
    const state = seedState()
    const n2 = node(state, 'n2')
    const n7 = node(state, 'n7')
    // Early in bar 3: bar 4 is far enough out for the clip to be queued.
    const position = nodeStartSeconds(n2) + 2.5 * BAR - CUT_LEAD_SECONDS
    expect(cutPlan(n2, position, n7)).toEqual({
      startSeconds: nodeStartSeconds(n7),
      switchAt: nodeStartSeconds(n2) + 3 * BAR,
    })
  })

  it('skips a bar line too close to make', () => {
    const n2 = node(seedState(), 'n2')
    const position = nodeStartSeconds(n2) + 3 * BAR - 0.5
    expect(cutPlan(n2, position, n2).switchAt).toBeCloseTo(nodeStartSeconds(n2) + Math.ceil(3 + 1.5 / BAR) * BAR)
  })

  it('never lands past the seam', () => {
    const n2 = node(seedState(), 'n2')
    expect(cutPlan(n2, nodeEndSeconds(n2) - 0.5, n2).switchAt).toBe(nodeEndSeconds(n2))
    const short = fixtureNode('s1', null, 'short', 16, 124, { durationSeconds: 15.5 * BAR })
    expect(cutPlan(short, 15.2 * BAR, short).switchAt).toBe(15.5 * BAR)
  })
})

describe('deckSeam', () => {
  it('says hold within reach of an untied seam and counts the bars left', () => {
    const state = onA({})
    const n2 = node(state, 'n2')
    expect(deckSeam(state, 'a', n2, nodeEndSeconds(n2) - 3.5 * BAR, {})).toMatchObject({ bars: 4, next: null, hold: true })
  })

  it('holds when a tied render will miss the seam', () => {
    const woven = reduce(onA({}), {
      type: 'weave',
      parentId: 'n2',
      prompt: 'acid',
      takes: 1,
      bars: 16,
      bpm: 124,
      loras: [{ name: 'acid', strength: 1 }],
      sa3: {},
      seeds: [1],
      ids: ['w1'],
      takeGroup: 'g',
      auto: false,
      now: 0,
    }, { type: 'tie', deck: 'a', id: 'w1' })
    const n2 = node(woven, 'n2')
    const seam = deckSeam(woven, 'a', n2, nodeEndSeconds(n2) - 10, {})
    expect(seam.hold).toBe(true)
    expect(seam.late).toBeGreaterThan(100)
  })
})
