import { describe, expect, it } from 'vitest'

import { fixtureNode, seedLoom, seedState } from './fixture'
import {
  initialLoomState,
  isOnAir,
  kids,
  loomReducer,
  parseLoom,
  pathTo,
  persistable,
  rootOf,
  takeSiblings,
  type LoomAction,
  type LoomNode,
  type LoomState,
} from './loomState'
import { barSeconds } from './timing'

const run = (state: LoomState, ...actions: LoomAction[]) => actions.reduce(loomReducer, state)
const node = (state: LoomState, id: string) => state.loom.nodes.find((n) => n.id === id)

const weave = (parentId: string | null, ids: string[], extra: Partial<Extract<LoomAction, { type: 'weave' }>> = {}): LoomAction => ({
  type: 'weave',
  parentId,
  prompt: 'dub techno, deeper',
  takes: ids.length,
  bars: 16,
  bpm: 100,
  loras: [],
  sa3: { cfg: 3 },
  seeds: ids.map((_, i) => 40 + i),
  ids,
  takeGroup: 'g1',
  auto: false,
  now: 7,
  ...extra,
})

/** The fixture with three fresh takes woven after n7. */
const withTakes = () => run(seedState(), weave('n7', ['t1', 't2', 't3']))

describe('weave', () => {
  it('creates N rendering takes sharing a take group', () => {
    const takes = withTakes().loom.nodes.filter((n) => n.parentId === 'n7')
    expect(takes.map((n) => [n.id, n.seed, n.status, n.takeGroup])).toEqual([
      ['t1', 40, 'rendering', 'g1'],
      ['t2', 41, 'rendering', 'g1'],
      ['t3', 42, 'rendering', 'g1'],
    ])
    expect(takes[0]).toMatchObject({ in: 0, out: 16, tailSeconds: 0, file: null, durationSeconds: null, createdAt: 7 })
  })

  it('takes the tempo from the parent, not the action', () => {
    expect(node(withTakes(), 't1')?.bpm).toBe(124)
  })

  it('gives a single take no group and a root the action tempo', () => {
    const state = run(seedState(), weave(null, ['solo'], { auto: true }))
    expect(node(state, 'solo')).toMatchObject({ takeGroup: null, bpm: 100, auto: true, parentId: null })
  })

  it('ignores a weave from a parent that is gone', () => {
    const state = seedState()
    expect(loomReducer(state, weave('gone', ['x']))).toBe(state)
  })
})

describe('render results', () => {
  it('sets a child tail from the clip surplus over its nominal body', () => {
    const body = 16 * barSeconds(124)
    const state = run(withTakes(), { type: 'rendered', id: 't1', file: 't1.wav', durationSeconds: body + 7.5 })
    expect(node(state, 't1')).toMatchObject({ file: 't1.wav', status: 'ready', in: 0 })
    expect(node(state, 't1')?.tailSeconds).toBeCloseTo(7.5)
  })

  it('gives a clip shorter than its body no tail', () => {
    const state = run(withTakes(), { type: 'rendered', id: 't1', file: 't1.wav', durationSeconds: 20 })
    expect(node(state, 't1')).toMatchObject({ tailSeconds: 0, durationSeconds: 20 })
  })

  it('gives a root no tail', () => {
    const state = run(seedState(), weave(null, ['root']), { type: 'rendered', id: 'root', file: 'r.wav', durationSeconds: 60 })
    expect(node(state, 'root')?.tailSeconds).toBe(0)
  })

  it('marks a failed render with its error, cleared by a later render', () => {
    const failed = run(withTakes(), { type: 'render_failed', id: 't2', error: '502: timed out' })
    expect(node(failed, 't2')).toMatchObject({ status: 'failed', error: '502: timed out' })
    const retried = run(failed, { type: 'rendered', id: 't2', file: 't2.wav', durationSeconds: 40 })
    expect(node(retried, 't2')?.error).toBeUndefined()
  })

  it('retry puts a failed render back to rendering and ignores any other node', () => {
    const failed = run(withTakes(), { type: 'render_failed', id: 't2', error: '502: timed out' })
    const retried = run(failed, { type: 'retry', id: 't2' })
    expect(node(retried, 't2')).toMatchObject({ status: 'rendering', error: undefined })
    expect(loomReducer(retried, { type: 'retry', id: 't1' })).toBe(retried)
  })

  it('learns a key by file and keeps the same state when nothing changes', () => {
    const state = run(seedState(), { type: 'key_known', file: 'n5.wav', key: '11B' })
    expect(node(state, 'n5')?.key).toBe('11B')
    expect(loomReducer(state, { type: 'key_known', file: 'n5.wav', key: '11B' })).toBe(state)
  })

  it('marks a hand-deleted file missing', () => {
    const state = run(seedState(), { type: 'file_missing', file: 'n6.wav' })
    expect(node(state, 'n6')?.status).toBe('missing')
    expect(node(state, 'n5')?.status).toBe('ready')
  })
})

describe('trim', () => {
  const trim = (id: string, inBar: number, outBar: number): LoomAction => ({ type: 'trim', id, in: inBar, out: outBar })

  it('snaps to whole bars', () => {
    expect(node(run(seedState(), trim('n7', 2.4, 11.6)), 'n7')).toMatchObject({ in: 2, out: 12 })
  })

  it('stops a left drag four bars before the out-point', () => {
    expect(node(run(seedState(), trim('n7', 15, 16)), 'n7')).toMatchObject({ in: 12, out: 16 })
  })

  it('stops a right drag four bars after the in-point without moving it', () => {
    const state = run(seedState(), trim('n7', 8, 16), trim('n7', 8, 9))
    expect(node(state, 'n7')).toMatchObject({ in: 8, out: 12 })
  })

  it('clamps to the clip bounds', () => {
    expect(node(run(seedState(), trim('n7', -3, 16)), 'n7')).toMatchObject({ in: 0, out: 16 })
    expect(node(run(seedState(), trim('n7', 0, 40)), 'n7')).toMatchObject({ in: 0, out: 16 })
  })

  it('refuses to move the out-point of a node with children', () => {
    const state = seedState()
    expect(loomReducer(state, trim('n5', 0, 12))).toBe(state)
  })

  it('still moves the in-point of a node with children', () => {
    expect(node(run(seedState(), trim('n5', 4, 16)), 'n5')).toMatchObject({ in: 4, out: 16 })
  })

  it('ignores a non-finite trim', () => {
    const state = seedState()
    expect(loomReducer(state, trim('n7', NaN, 8))).toBe(state)
  })
})

describe('keep and discard', () => {
  it('keep discards the other takes and drops the kept one from its group', () => {
    const state = run(withTakes(), { type: 'keep', id: 't2' })
    expect(kids(state.loom.nodes, 'n7').map((n) => n.id)).toEqual(['t2'])
    expect(node(state, 't2')?.takeGroup).toBeNull()
  })

  it('keep spares a take a deck is on, one queued and one with children, as ordinary nodes', () => {
    const state = run(
      withTakes(),
      weave('t3', ['t4'], { takes: 1 }),
      { type: 'landed', deck: 'a', id: 't1' },
      { type: 'keep', id: 't2' },
    )
    expect(kids(state.loom.nodes, 'n7').map((n) => n.id)).toEqual(['t1', 't2', 't3'])
    expect(kids(state.loom.nodes, 'n7').map((n) => n.takeGroup)).toEqual([null, null, null])

    const queued = run(withTakes(), { type: 'tie', deck: 'b', id: 't3' }, { type: 'keep', id: 't1' })
    expect(kids(queued.loom.nodes, 'n7').map((n) => n.id)).toEqual(['t1', 't3'])
  })

  it('discard removes a leaf and any cue or selection on it', () => {
    const state = run(
      seedState(),
      { type: 'cue', deck: 'a', id: 'n7', fromBar: 2 },
      { type: 'select', id: 'n7' },
      { type: 'discard', id: 'n7' },
    )
    expect(node(state, 'n7')).toBeUndefined()
    expect(state.decks.a.cue).toBeNull()
    expect(state.selected).toBeNull()
  })

  it('discard refuses a node a deck is on or has queued', () => {
    const onAir = run(seedState(), { type: 'landed', deck: 'a', id: 'n7' })
    expect(loomReducer(onAir, { type: 'discard', id: 'n7' })).toBe(onAir)
    const queued = run(seedState(), { type: 'tie', deck: 'b', id: 'n6' })
    expect(loomReducer(queued, { type: 'discard', id: 'n6' })).toBe(queued)
  })

  it('discard refuses a parent', () => {
    const state = seedState()
    expect(loomReducer(state, { type: 'discard', id: 'n5' })).toBe(state)
  })
})

describe('the shuttle', () => {
  const onN2 = () => run(seedState(), { type: 'landed', deck: 'a', id: 'n2' })

  it('tie appends once', () => {
    const state = run(onN2(), { type: 'tie', deck: 'a', id: 'n7' }, { type: 'tie', deck: 'a', id: 'n7' })
    expect(state.decks.a.queue).toEqual(['n7'])
  })

  it('tie reaches a downstream node through the tree', () => {
    const state = run(onN2(), { type: 'tie', deck: 'a', id: 'n4' })
    expect(state.decks.a.queue).toEqual(['n3', 'n4'])
  })

  it('tie queues a return or a jump on its own', () => {
    const state = run(onN2(), { type: 'tie', deck: 'a', id: 'r1' }, { type: 'tie', deck: 'a', id: 'n10' })
    expect(state.decks.a.queue).toEqual(['r1', 'n10'])
  })

  it('tie to the node a deck is on, with nothing queued, changes nothing', () => {
    const state = onN2()
    expect(loomReducer(state, { type: 'tie', deck: 'a', id: 'n2' })).toBe(state)
  })

  it('tie front moves a queued node to the front', () => {
    const state = run(
      onN2(),
      { type: 'tie', deck: 'a', id: 'n7' },
      { type: 'tie', deck: 'a', id: 'n5' },
      { type: 'tie', deck: 'a', id: 'n5', front: true },
    )
    expect(state.decks.a.queue).toEqual(['n5', 'n7'])
  })

  it('tie ignores a node that is gone', () => {
    const state = onN2()
    expect(loomReducer(state, { type: 'tie', deck: 'a', id: 'gone' })).toBe(state)
  })

  it('untie removes the node from the queue', () => {
    const state = run(onN2(), { type: 'tie', deck: 'a', id: 'n4' }, { type: 'untie', deck: 'a', id: 'n4' })
    expect(state.decks.a.queue).toEqual(['n3'])
  })

  it('landed advances history, pops the queue front and drops the loop', () => {
    const state = run(
      onN2(),
      { type: 'tie', deck: 'a', id: 'n4' },
      { type: 'set_loop', deck: 'a', bars: 8, startBar: 4 },
      { type: 'landed', deck: 'a', id: 'n3' },
    )
    expect(state.decks.a).toMatchObject({ nodeId: 'n3', history: ['n2'], queue: ['n4'], loopBars: null, loopStartBar: null })
  })

  it('landed off-queue (a load) leaves the queue alone', () => {
    const state = run(onN2(), { type: 'tie', deck: 'a', id: 'n7' }, { type: 'landed', deck: 'a', id: 'n5' })
    expect(state.decks.a.queue).toEqual(['n7'])
  })

  it('sets autopilot and loops per deck', () => {
    const state = run(seedState(), { type: 'set_auto', deck: 'b', on: true }, { type: 'set_loop', deck: 'b', bars: 16, startBar: 0 })
    expect(state.decks.b).toMatchObject({ auto: true, loopBars: 16, loopStartBar: 0 })
    expect(state.decks.a).toMatchObject({ auto: false, loopBars: null })
  })

  it('isOnAir sees the node a deck is on and its queue', () => {
    const state = run(onN2(), { type: 'tie', deck: 'a', id: 'n7' })
    expect([isOnAir(state, 'n2'), isOnAir(state, 'n7'), isOnAir(state, 'n5')]).toEqual([true, true, false])
  })
})

describe('cue', () => {
  it('defaults to the join, four bars before the in-point', () => {
    const state = run(
      seedState(),
      { type: 'trim', id: 'n5', in: 6, out: 16 },
      { type: 'cue', deck: 'a', id: 'n5', fromBar: null },
    )
    expect(state.decks.a.cue).toEqual({ nodeId: 'n5', fromBar: 2 })
  })

  it('cues from a clicked bar and replaces the other deck’s cue', () => {
    const state = run(
      seedState(),
      { type: 'cue', deck: 'a', id: 'n5', fromBar: null },
      { type: 'cue', deck: 'b', id: 'n6', fromBar: 3 },
    )
    expect(state.decks.a.cue).toBeNull()
    expect(state.decks.b.cue).toEqual({ nodeId: 'n6', fromBar: 3 })
  })

  it('stops on a null id', () => {
    const state = run(seedState(), { type: 'cue', deck: 'a', id: 'n5', fromBar: 1 }, { type: 'cue', deck: 'a', id: null, fromBar: null })
    expect(state.decks.a.cue).toBeNull()
  })
})

describe('looms', () => {
  it('add_root_song floors its bars so the nominal end never passes the clip', () => {
    const state = run(seedState(), {
      type: 'add_root_song',
      id: 'song',
      file: 'dropped.wav',
      title: 'Dropped',
      bpm: 120,
      durationSeconds: 31.9, // 15.95 bars of 2 s
      prompt: null,
      now: 3,
    })
    expect(node(state, 'song')).toMatchObject({
      bars: 15,
      out: 15,
      prompt: 'Dropped',
      status: 'ready',
      tailSeconds: 0,
      durationSeconds: 31.9,
      parentId: null,
    })
  })

  it('add_root_song keeps a very short song at the minimum body', () => {
    const state = run(seedState(), {
      type: 'add_root_song',
      id: 'blip',
      file: 'blip.wav',
      title: 'Blip',
      bpm: 120,
      durationSeconds: 3,
      prompt: 'a blip',
      now: 0,
    })
    expect(node(state, 'blip')).toMatchObject({ bars: 4, prompt: 'a blip' })
  })

  it('loom_loaded replaces the loom and resets both decks', () => {
    const playing = run(seedState(), { type: 'landed', deck: 'a', id: 'n2' }, { type: 'select', id: 'n2' })
    const loom = { ...seedLoom(), id: 'other' }
    const state = loomReducer(playing, { type: 'loom_loaded', loom })
    expect(state).toEqual(initialLoomState(loom))
  })

  it('loom_new starts an empty loom', () => {
    const state = loomReducer(seedState(), { type: 'loom_new', id: 'fresh-loom', now: 9, name: 'Untitled' })
    expect(state.loom).toEqual({ version: 1, id: 'fresh-loom', name: 'Untitled', nodes: [], createdAt: 9, updatedAt: 9 })
    expect(state.decks.a.nodeId).toBeNull()
  })

  it('rename trims, and refuses an empty name', () => {
    const state = run(seedState(), { type: 'rename', name: '  Sunrise set ' })
    expect(state.loom.name).toBe('Sunrise set')
    expect(loomReducer(state, { type: 'rename', name: '   ' })).toBe(state)
  })

  it('persistable strips runtime and bumps updatedAt', () => {
    const saved = persistable(run(seedState(), { type: 'landed', deck: 'a', id: 'n2' }), 1234)
    expect(saved).toEqual({ ...seedLoom(), updatedAt: 1234 })
    expect(saved).not.toHaveProperty('decks')
  })
})

describe('selectors', () => {
  const nodes = seedLoom().nodes

  it('walk the tree and tolerate missing ids', () => {
    expect(kids(nodes, 'n2').map((n) => n.id)).toEqual(['n3', 'n5', 'n7'])
    expect(pathTo(nodes, 'n4').map((n) => n.id)).toEqual(['r1', 'n2', 'n3', 'n4'])
    expect(rootOf(nodes, 'n10')?.id).toBe('r2')
    expect(pathTo(nodes, 'gone')).toEqual([])
    expect(rootOf(nodes, 'gone')).toBeNull()
  })

  it('takeSiblings lists the other takes only', () => {
    const state = withTakes()
    expect(takeSiblings(state.loom.nodes, 't2').map((n) => n.id)).toEqual(['t1', 't3'])
    expect(takeSiblings(state.loom.nodes, 'n2')).toEqual([])
  })
})

describe('parseLoom', () => {
  const json = (value: unknown) => JSON.parse(JSON.stringify(value))

  it('accepts the fixture as saved', () => {
    expect(parseLoom(json(seedLoom()))).toEqual(seedLoom())
  })

  it('rejects a child whose parent is missing', () => {
    const loom = seedLoom()
    loom.nodes = loom.nodes.filter((n) => n.id !== 'n5')
    expect(parseLoom(json(loom))).toBeNull()
  })

  it('rejects a parent listed after its child (which also rules out cycles)', () => {
    const loom = seedLoom()
    loom.nodes = [loom.nodes[1], loom.nodes[0], ...loom.nodes.slice(2)]
    expect(parseLoom(json(loom))).toBeNull()
  })

  it.each<[string, Partial<LoomNode> | Record<string, unknown>]>([
    ['an in-point at the out-point', { in: 8, out: 8 }],
    ['an out-point past the bars', { out: 17 }],
    ['an unknown status', { status: 'ghost' }],
    ['a non-finite number', { bpm: Infinity }],
    ['a duplicate id', { id: 'r1' }],
    ['a malformed LoRA', { loras: [{ name: 'x' }] }],
  ])('rejects %s', (_, patch) => {
    const loom = seedLoom()
    loom.nodes[1] = { ...loom.nodes[1], ...patch } as LoomNode
    expect(parseLoom(loom)).toBeNull()
  })

  it('rejects another version or a non-object', () => {
    expect(parseLoom({ ...seedLoom(), version: 2 })).toBeNull()
    expect(parseLoom('loom')).toBeNull()
    expect(parseLoom(null)).toBeNull()
  })

  it('drops unknown fields', () => {
    const loom = json(seedLoom())
    loom.extra = 1
    loom.nodes[0].ghost = true
    const parsed = parseLoom(loom)
    expect(parsed).not.toHaveProperty('extra')
    expect(parsed?.nodes[0]).not.toHaveProperty('ghost')
  })

  it.each(['rendering', 'queued'])('reopens a node saved %s as failed', (status) => {
    const loom = json(seedLoom())
    loom.nodes.push({ ...fixtureNode('ghost', 'n7', 'still rendering', 16, 124, { file: null, durationSeconds: null }), status })
    expect(parseLoom(loom)?.nodes.at(-1)?.status).toBe('failed')
  })
})
