import { act, fireEvent, render, renderHook, screen, within } from '@testing-library/react'
import type { ReactNode } from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { AudioEngineProvider } from '../audio/AudioEngineProvider'
import type { AudioEngine, DeckId } from '../audio/types'
import { createControlBus, type ControlBus } from '../control/bus'
import { ControlBusProvider } from '../control/ControlBusProvider'
import type { DeckControls, TrackState } from '../deck/useDeck'
import type { ChannelControls } from '../mixer/MixerStrip'
import { fixtureNode, seedLoom } from './fixture'
import { LoomBooth, type LoomBoothProps } from './LoomBooth'
import { auditionSongClip, listLooms, readLoom, saveLoom } from './loomLibrary'
import { initialLoomState, kids, nodeById, type Loom, type LoomState } from './loomState'
import { CUT_LEAD_SECONDS, planDeck } from './schedule'
import { barSeconds, holdRegion, loopRegion, nodeEndSeconds, nodeStartSeconds } from './timing'
import { useLoom } from './useLoom'

const invokeMock = vi.hoisted(() => vi.fn<(cmd: string, args?: unknown) => Promise<unknown>>())
vi.mock('../audio/nativeEngine', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../audio/nativeEngine')>()),
  invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
  subscribeLibraryChanged: () => () => {},
}))
vi.mock('./loomLibrary', () => ({
  listLooms: vi.fn(),
  readLoom: vi.fn(),
  saveLoom: vi.fn(),
  deleteLoom: vi.fn(),
  auditionSongClip: vi.fn(async () => {}),
}))

const BAR = barSeconds(124)
const engine = { auditionStop: vi.fn(), getMasterLevel: () => 0 } as unknown as AudioEngine

function fakeDeck(): DeckControls {
  return {
    track: null,
    fx: { kind: null, amount: 0 },
    loadTrack: vi.fn(async () => true),
    queueTrack: vi.fn(async () => true),
    clearQueuedTrack: vi.fn(),
    setTrackLoopRegion: vi.fn(),
    seekTrack: vi.fn(),
    loopExit: vi.fn(),
    beatLoop: vi.fn(),
    play: vi.fn(async () => {}),
    stop: vi.fn(),
    setFx: vi.fn(),
    setFxAmount: vi.fn(),
  } as unknown as DeckControls
}

const baseTrack = (): TrackState => ({
  loadId: 0,
  title: '',
  duration: 60,
  position: 0,
  playing: true,
  ended: false,
  bpm: 124,
  grid: null,
  rate: 1,
  cues: [],
  loop: null,
  pendingLoopIn: null,
  queued: null,
})

/** Let the boot's and the gestures' promise chains settle. */
const flush = () =>
  act(async () => {
    for (let i = 0; i < 10; i++) await Promise.resolve()
  })
/** Time passes between deck polls (the engine snapshot settles). */
const later = (ms = 250) => act(() => void vi.advanceTimersByTime(ms))

function wrapper({ children }: { children: ReactNode }) {
  return <AudioEngineProvider engine={engine}>{children}</AudioEngineProvider>
}

function libraryHas(loom: Loom | null) {
  vi.mocked(listLooms).mockResolvedValue(loom ? [{ id: loom.id, name: loom.name, updatedAt: 0, nodes: loom.nodes.length }] : [])
  vi.mocked(readLoom).mockResolvedValue(loom)
}

async function mount({ loom = seedLoom() as Loom | null, renderAhead = false } = {}) {
  libraryHas(loom)
  const decks = { a: fakeDeck(), b: fakeDeck() }
  const props = { deckA: decks.a, deckB: decks.b, renderAhead }
  const hook = renderHook((p: typeof props) => useLoom(p), { initialProps: props, wrapper })
  await flush()
  const key = (deck: DeckId) => (deck === 'a' ? 'deckA' : 'deckB')
  /** One 4 Hz poll: a fresh deck object (as useDeck's is), the same methods. */
  const poll = (deck: DeckId, patch: Partial<TrackState>) => {
    const current = props[key(deck)]
    props[key(deck)] = { ...current, track: { ...(current.track ?? baseTrack()), ...patch } }
    hook.rerender({ ...props })
  }
  const state = (): LoomState => hook.result.current.state
  const node = (id: string) => nodeById(state().loom.nodes, id)!
  /** Tie onto an empty deck (it loads), then the poll that sees the load. */
  const landOn = async (deck: DeckId, id: string, playing = true) => {
    act(() => hook.result.current.tie(deck, id))
    await flush()
    const loadId = (props[key(deck)].track?.loadId ?? 0) + 1
    poll(deck, { loadId, position: nodeStartSeconds(node(id)), playing, loop: null, queued: null })
  }
  return { ...hook, decks, poll, state, node, landOn }
}

beforeEach(() => {
  vi.useFakeTimers()
  vi.clearAllMocks()
  // A render job stays in flight (its source clip never arrives); anything
  // else unmocked fails the way a missing shell does.
  invokeMock.mockImplementation((cmd) =>
    cmd === 'song_source_clip' ? new Promise(() => {}) : Promise.reject(new Error(`no shell: ${cmd}`)),
  )
  vi.mocked(saveLoom).mockImplementation(async (id, name) => ({ id, name, updatedAt: 0, nodes: 0 }))
})
afterEach(() => {
  vi.clearAllTimers()
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

describe('useLoom boot and autosave', () => {
  it('opens the newest loom without rewriting it', async () => {
    const m = await mount()
    expect(readLoom).toHaveBeenCalledWith('loom-fixture')
    expect(m.state().loom.name).toBe('Late dub')
    later(1000)
    expect(saveLoom).not.toHaveBeenCalled()
    expect(m.result.current.saved).toBe('saved')
  })

  it('starts a fresh loom with the default name when there is none', async () => {
    const m = await mount({ loom: null })
    expect(m.state().loom.name).toBe('Untitled loom')
    expect(m.state().loom.id).toMatch(/^[a-z0-9][a-z0-9-]{7,39}$/)
  })

  it('autosaves once per burst of changes, 700 ms after the last', async () => {
    const m = await mount()
    act(() => m.result.current.rename('First'))
    later(500)
    act(() => m.result.current.rename('Second'))
    later(699)
    expect(saveLoom).not.toHaveBeenCalled()
    expect(m.result.current.saved).toBe('saving')
    later(1)
    expect(saveLoom).toHaveBeenCalledTimes(1)
    expect(saveLoom).toHaveBeenCalledWith('loom-fixture', 'Second', expect.objectContaining({ name: 'Second' }))
    await flush()
    expect(m.result.current.saved).toBe('saved')
  })

  it('says a failed save once, not on every retry', async () => {
    vi.mocked(saveLoom).mockRejectedValue(new Error('disk full'))
    const m = await mount()
    act(() => m.result.current.rename('First'))
    later(700)
    await flush()
    expect(m.result.current.status).toBe('Autosave failed: disk full')
    later(6000)
    expect(m.result.current.status).toBeNull()
    act(() => m.result.current.rename('Second'))
    later(700)
    await flush()
    expect(saveLoom).toHaveBeenCalledTimes(2)
    expect(m.result.current.status).toBeNull()
  })

  it('reopens a loom saved mid-render with both decks stopped and no ghost job, and →A resumes it (R5)', async () => {
    const saved = JSON.parse(JSON.stringify(seedLoom()))
    const ghost = (id: string, status: string) => ({
      ...fixtureNode(id, 'n7', 'still rendering', 16, 124, { file: null, durationSeconds: null }),
      status,
    })
    saved.nodes.push(ghost('g1', 'rendering'), ghost('g2', 'queued'))
    const m = await mount({ loom: saved })
    expect(m.state().decks).toEqual(initialLoomState(seedLoom()).decks)
    expect([m.node('g1').status, m.node('g2').status]).toEqual(['failed', 'failed'])
    // A ghost left rendering would have asked the shell for n7's tail.
    expect(invokeMock).not.toHaveBeenCalledWith('song_source_clip', expect.anything())
    expect(m.decks.a.loadTrack).not.toHaveBeenCalled()

    await m.landOn('a', 'n2')
    expect(m.decks.a.loadTrack).toHaveBeenCalledWith({ kind: 'song', name: 'n2.wav' }, m.node('n2').prompt)
    act(() => m.result.current.tie('a', 'n5'))
    expect(m.decks.a.queueTrack).toHaveBeenCalledWith(
      { kind: 'song', name: 'n5.wav' },
      m.node('n5').prompt,
      nodeStartSeconds(m.node('n5')),
      nodeEndSeconds(m.node('n2')),
    )
  })
})

describe('useLoom shuttle', () => {
  it('holds an untied seam with one loop placement however often the deck polls', async () => {
    const m = await mount()
    await m.landOn('a', 'n2')
    const hold = holdRegion(m.node('n2'))
    expect(m.state().decks.a.nodeId).toBe('n2')
    expect(m.decks.a.setTrackLoopRegion).toHaveBeenCalledTimes(1)
    expect(m.decks.a.setTrackLoopRegion).toHaveBeenCalledWith(hold.start, hold.end)
    later()
    m.poll('a', { loop: hold, position: 3 * BAR })
    later()
    m.poll('a', { loop: hold, position: 3 * BAR + 0.25 })
    expect(m.decks.a.setTrackLoopRegion).toHaveBeenCalledTimes(1)
  })

  it('ties on a stopped deck by loading the node at its in-point', async () => {
    const m = await mount()
    act(() => m.result.current.tie('a', 'n3'))
    await flush()
    expect(m.decks.a.loadTrack).toHaveBeenCalledWith({ kind: 'song', name: 'n3.wav' }, m.node('n3').prompt)
    expect(m.decks.a.seekTrack).toHaveBeenCalledWith(nodeStartSeconds(m.node('n3')))
    m.poll('a', { loadId: 1, playing: false, position: nodeStartSeconds(m.node('n3')) })
    expect(m.state().decks.a).toMatchObject({ nodeId: 'n3', queue: [] })
  })

  it('loads a tie at once on a deck that has a node but is stopped', async () => {
    const m = await mount()
    await m.landOn('a', 'n2', false)
    act(() => m.result.current.tie('a', 'n5'))
    await flush()
    expect(m.decks.a.loadTrack).toHaveBeenLastCalledWith({ kind: 'song', name: 'n5.wav' }, m.node('n5').prompt)
    expect(m.decks.a.queueTrack).not.toHaveBeenCalled()
  })

  it('queues a ready tie once, at the seam, and keeps the hold under it', async () => {
    const m = await mount()
    await m.landOn('a', 'n2')
    act(() => m.result.current.tie('a', 'n5'))
    m.poll('a', { position: 3 * BAR })
    m.poll('a', { position: 3 * BAR + 0.25 })
    expect(m.decks.a.queueTrack).toHaveBeenCalledTimes(1)
    expect(m.decks.a.queueTrack).toHaveBeenCalledWith(
      { kind: 'song', name: 'n5.wav' },
      m.node('n5').prompt,
      nodeStartSeconds(m.node('n5')),
      nodeEndSeconds(m.node('n2')),
    )
    expect(m.decks.a.setTrackLoopRegion).toHaveBeenCalledTimes(1)
    expect(m.decks.a.clearQueuedTrack).not.toHaveBeenCalled()
  })

  it('queues the seam at the clip end when the clip is shorter than its bars', async () => {
    const loom: Loom = {
      ...seedLoom(),
      nodes: [
        fixtureNode('s1', null, 'short root', 16, 124, { durationSeconds: 15.5 * BAR }),
        fixtureNode('s2', 's1', 'next', 16, 124),
      ],
    }
    const m = await mount({ loom })
    await m.landOn('a', 's1')
    act(() => m.result.current.tie('a', 's2'))
    expect(m.decks.a.queueTrack).toHaveBeenCalledWith({ kind: 'song', name: 's2.wav' }, 'next', expect.any(Number), 15.5 * BAR)
  })

  it('drops the queued swap when the tie is undone', async () => {
    const m = await mount()
    await m.landOn('a', 'n2')
    act(() => m.result.current.tie('a', 'n5'))
    act(() => m.result.current.untie('a', 'n5'))
    expect(m.decks.a.clearQueuedTrack).toHaveBeenCalledTimes(1)
  })

  it('clears a queue request that resolves after the tie was undone', async () => {
    const m = await mount()
    await m.landOn('a', 'n2')
    let arrive = () => {}
    vi.mocked(m.decks.a.queueTrack).mockImplementationOnce(
      () => new Promise<boolean>((resolve) => (arrive = () => resolve(true))),
    )
    act(() => m.result.current.tie('a', 'n5'))
    act(() => m.result.current.untie('a', 'n5'))
    expect(m.decks.a.clearQueuedTrack).toHaveBeenCalledTimes(1)
    // The shell decoded n5 after the untie: the engine holds a stale queue.
    arrive()
    await flush()
    expect(m.decks.a.clearQueuedTrack).toHaveBeenCalledTimes(2)
  })

  it('clears an armed swap when the booth unmounts (Theme → DJ)', async () => {
    const m = await mount()
    await m.landOn('a', 'n2')
    act(() => m.result.current.tie('a', 'n5'))
    expect(m.decks.a.queueTrack).toHaveBeenCalledTimes(1)
    m.unmount()
    expect(m.decks.a.clearQueuedTrack).toHaveBeenCalledTimes(1)
    expect(m.decks.b.clearQueuedTrack).not.toHaveBeenCalled()
  })

  it('lands the swapped-in node and grows one auto child behind it', async () => {
    const m = await mount({ renderAhead: true })
    await m.landOn('a', 'n5')
    expect(kids(m.state().loom.nodes, 'n5')).toHaveLength(1)
    act(() => m.result.current.tie('a', 'n6'))
    m.poll('a', { loadId: 2, position: nodeStartSeconds(m.node('n6')), loop: null, queued: null })
    expect(m.state().decks.a).toMatchObject({ nodeId: 'n6', queue: [], history: ['n5'] })
    m.poll('a', { position: nodeStartSeconds(m.node('n6')) + 0.25 })
    const grown = kids(m.state().loom.nodes, 'n6')
    expect(grown).toHaveLength(1)
    expect(grown[0]).toMatchObject({ auto: true, status: 'rendering', prompt: m.node('n6').prompt, bars: 16 })
  })

  it('grows nothing behind a LoRA node', async () => {
    const m = await mount({ renderAhead: true })
    await m.landOn('a', 'n3')
    act(() => m.result.current.tie('a', 'n4'))
    m.poll('a', { loadId: 2, position: nodeStartSeconds(m.node('n4')) })
    expect(m.state().decks.a.nodeId).toBe('n4')
    expect(kids(m.state().loom.nodes, 'n4')).toHaveLength(0)
  })

  it('grows nothing with render ahead off', async () => {
    const m = await mount({ renderAhead: false })
    await m.landOn('a', 'n6')
    expect(kids(m.state().loom.nodes, 'n6')).toHaveLength(0)
  })

  it('re-arms the hold once after a seek drops the loop', async () => {
    const m = await mount()
    await m.landOn('a', 'n2')
    const hold = holdRegion(m.node('n2'))
    later()
    m.poll('a', { loop: hold })
    later()
    m.poll('a', { loop: null, position: 5 * BAR })
    expect(m.decks.a.setTrackLoopRegion).toHaveBeenCalledTimes(2)
    expect(m.decks.a.setTrackLoopRegion).toHaveBeenLastCalledWith(hold.start, hold.end)
    // The echo before the engine snapshot catches up does not re-arm again.
    m.poll('a', { loop: null })
    expect(m.decks.a.setTrackLoopRegion).toHaveBeenCalledTimes(2)
  })

  it('leaves a loop it did not place alone until the DJ releases it', async () => {
    const m = await mount()
    await m.landOn('a', 'n2')
    act(() => m.result.current.tie('a', 'n5'))
    later()
    m.poll('a', { loop: { start: 3 * BAR, end: 4 * BAR } })
    // A loop wins over a tie: the swap is withdrawn, nothing is placed.
    expect(m.decks.a.clearQueuedTrack).toHaveBeenCalledTimes(1)
    expect(m.decks.a.setTrackLoopRegion).toHaveBeenCalledTimes(1)
    later()
    m.poll('a', { loop: null })
    expect(m.decks.a.queueTrack).toHaveBeenCalledTimes(2)
    expect(m.decks.a.setTrackLoopRegion).toHaveBeenCalledTimes(2)
  })

  it('cuts at the queue front on a bar line and keeps the cut through the next poll', async () => {
    const m = await mount()
    await m.landOn('a', 'n2')
    const start = nodeStartSeconds(m.node('n2'))
    m.poll('a', { position: start + 2.5 * BAR - CUT_LEAD_SECONDS })
    act(() => m.result.current.tie('a', 'n7'))
    act(() => m.result.current.cut('a', 'n5'))
    expect(m.state().decks.a.queue).toEqual(['n5', 'n7'])
    expect(m.decks.a.queueTrack).toHaveBeenLastCalledWith(
      { kind: 'song', name: 'n5.wav' },
      m.node('n5').prompt,
      nodeStartSeconds(m.node('n5')),
      start + 3 * BAR,
    )
    m.poll('a', { position: start + 2.5 * BAR - CUT_LEAD_SECONDS + 0.25 })
    expect(m.decks.a.queueTrack).toHaveBeenCalledTimes(2)
    m.poll('a', { loadId: 2, position: nodeStartSeconds(m.node('n5')) })
    expect(m.state().decks.a).toMatchObject({ nodeId: 'n5', queue: ['n7'] })
  })

  it('refuses to discard the node a deck is on, and says why', async () => {
    const m = await mount()
    await m.landOn('a', 'n6')
    act(() => m.result.current.discard('n6'))
    expect(m.node('n6')).toBeTruthy()
    expect(m.result.current.status).toBe('A node a deck is on or has tied, or one with children, stays')
  })

  it('keeps holding when a tied render fails, marks the node failed and never queues it (R3)', async () => {
    const backendDown = vi.fn(async () => ({ ok: false, status: 502, json: async () => ({ detail: 'sa3 worker exited' }) }))
    vi.stubGlobal('fetch', backendDown)
    const m = await mount()
    await m.landOn('a', 'n2')
    act(() =>
      m.result.current.weave(null, { prompt: 'acid techno', takes: 1, bars: 16, bpm: 124, loras: [], sa3: {}, seed: 7, tieTo: 'a' }),
    )
    const woven = m.state().loom.nodes.at(-1)!
    expect(m.state().decks.a.queue).toEqual([woven.id])
    await flush()
    expect(backendDown).toHaveBeenCalledTimes(1)
    expect(m.node(woven.id)).toMatchObject({ status: 'failed', error: 'sa3 worker exited' })
    m.poll('a', { position: 3 * BAR })
    expect(planDeck(m.state(), 'a')).toEqual({ kind: 'hold', loop: holdRegion(m.node('n2')) })
    expect(m.state().decks.a.nodeId).toBe('n2')
    expect(m.decks.a.queueTrack).not.toHaveBeenCalled()
    expect(m.decks.a.setTrackLoopRegion).toHaveBeenCalledTimes(1)
    expect(m.decks.a.stop).not.toHaveBeenCalled()
  })

  it('greys a hand-deleted song as missing, never loads or queues it, and plays the rest (R4)', async () => {
    const onDisk = seedLoom().nodes.filter((n) => n.file !== 'n5.wav').map((n) => ({ file: n.file }))
    invokeMock.mockImplementation(async (cmd) => {
      if (cmd === 'list_generated_songs') return onDisk
      throw new Error(`no shell: ${cmd}`)
    })
    const m = await mount()
    expect(m.state().loom.nodes.filter((n) => n.status !== 'ready').map((n) => n.id)).toEqual(['n5'])
    act(() => m.result.current.tie('b', 'n5'))
    await flush()
    expect(m.decks.b.loadTrack).not.toHaveBeenCalled()
    expect(m.result.current.status).toBe(`“${m.node('n5').prompt}” has not rendered yet`)

    await m.landOn('a', 'n2')
    act(() => m.result.current.tie('a', 'n5'))
    m.poll('a', { position: 3 * BAR })
    expect(m.decks.a.queueTrack).not.toHaveBeenCalled()
    act(() => m.result.current.untie('a', 'n5'))
    act(() => m.result.current.tie('a', 'n7'))
    expect(m.decks.a.queueTrack).toHaveBeenCalledTimes(1)
    expect(m.decks.a.queueTrack).toHaveBeenCalledWith(
      { kind: 'song', name: 'n7.wav' },
      m.node('n7').prompt,
      nodeStartSeconds(m.node('n7')),
      nodeEndSeconds(m.node('n2')),
    )
  })
})

describe('useLoom cue', () => {
  it('cues a node in the phones from the join, and stops it', async () => {
    const m = await mount()
    act(() => m.result.current.trim('n5', 2, 16))
    act(() => m.result.current.cue('n5', null))
    // The join: the kept tail's last bars before the in-point.
    expect(auditionSongClip).toHaveBeenCalledWith('n5.wav', expect.closeTo(2 * BAR, 6))
    expect(m.state().decks.a.cue).toEqual({ nodeId: 'n5', fromBar: -2 })
    act(() => m.result.current.cue(null, null))
    expect(engine.auditionStop).toHaveBeenCalledTimes(2)
    expect(m.state().decks.a.cue).toBeNull()
  })
})

describe('useLoom song drop', () => {
  /** The shell's two answers a dropped song needs: its registry row and its length. */
  const libraryHasSong = () =>
    invokeMock.mockImplementation(async (cmd) => {
      if (cmd === 'list_generated_songs') {
        return [{ file: 'dub.wav', title: 'Dub', prompt: 'warm dub, 120 BPM', model: 'track', bpm: 120, key: '8A' }]
      }
      if (cmd === 'library_clip_peaks') return { min: [], max: [], duration: 33 }
      throw new Error(`no shell: ${cmd}`)
    })

  it('drops a song on a node as its ready next thread at the song\'s own tempo, and selects it', async () => {
    const m = await mount()
    libraryHasSong()
    act(() => m.result.current.dropSong('dub.wav', 'n7'))
    await flush()
    const added = m.state().loom.nodes.filter((n) => n.file === 'dub.wav')
    expect(added).toHaveLength(1)
    expect(added[0]).toMatchObject({ parentId: 'n7', prompt: 'warm dub', bpm: 120, bars: 16, tailSeconds: 0, status: 'ready', key: '8A' })
    expect(m.state().selected).toBe(added[0].id)
    expect(m.result.current.status).toBe('“warm dub” follows “breakbeat jungle, chopped amens, reese bass”')
  })

  it('finds the child a node already has rather than adding the song twice', async () => {
    const m = await mount()
    libraryHasSong()
    act(() => m.result.current.dropSong('dub.wav', 'n7'))
    await flush()
    act(() => m.result.current.dropSong('dub.wav', 'n7'))
    await flush()
    expect(m.state().loom.nodes.filter((n) => n.file === 'dub.wav')).toHaveLength(1)
  })

  it('drops a song on the canvas as a new root', async () => {
    const m = await mount()
    libraryHasSong()
    act(() => m.result.current.dropSong('dub.wav'))
    await flush()
    expect(m.state().loom.nodes.find((n) => n.file === 'dub.wav')).toMatchObject({ parentId: null })
    expect(m.result.current.status).toBe('“warm dub” starts a thread')
  })
})

const channel = (): ChannelControls => ({
  volume: 0.8,
  eq: { low: 0.5, mid: 0.5, high: 0.5 },
  cue: false,
  trim: { mode: 'auto', db: 0 },
  onSetVolume: vi.fn(),
  onSetEqBand: vi.fn(),
  onSetCue: vi.fn(),
  onSetTrimDb: vi.fn(),
  onEnableAutoTrim: vi.fn(),
  getLevel: () => 0,
})

describe('LoomBooth', () => {
  function mountBooth(bus: ControlBus, barSlot: HTMLElement | null = null) {
    libraryHas(seedLoom())
    const props: LoomBoothProps = {
      deckA: fakeDeck(),
      deckB: fakeDeck(),
      channels: { a: channel(), b: channel() },
      crossfade: 0.5,
      onCrossfade: vi.fn(),
      cueMix: 0.5,
      onCueMix: vi.fn(),
      getPhaseOffset: () => null,
      onSyncA: vi.fn(),
      onSyncB: vi.fn(),
      tempoRange: 0.08,
      renderAhead: false,
      performanceVisuals: false,
      barSlot,
      mediaOpen: false,
      onToggleExplorer: vi.fn(),
    }
    const tree = (p: LoomBoothProps) => (
      <AudioEngineProvider engine={engine}>
        <ControlBusProvider bus={bus}>
          <LoomBooth {...p} />
        </ControlBusProvider>
      </AudioEngineProvider>
    )
    const view = render(tree(props))
    const pollA = (patch: Partial<TrackState>) => {
      props.deckA = { ...props.deckA, track: { ...(props.deckA.track ?? baseTrack()), ...patch } }
      view.rerender(tree({ ...props }))
    }
    return { props, pollA, deckA: props.deckA }
  }

  it('ties a node dropped on a deck panel and runs the FLX4 beat loop as a four-bar loom loop', async () => {
    const bus = createControlBus()
    const booth = mountBooth(bus)
    await flush()
    fireEvent.drop(screen.getByRole('group', { name: 'Deck a' }), { dataTransfer: { getData: () => 'loom:n2' } })
    await flush()
    expect(booth.deckA.loadTrack).toHaveBeenCalledWith({ kind: 'song', name: 'n2.wav' }, 'dub techno, rolling bassline')
    const n2 = seedLoom().nodes.find((n) => n.id === 'n2')!
    const position = nodeStartSeconds(n2) + 2.5 * BAR
    booth.pollA({ loadId: 1, position })

    act(() => bus.publish({ kind: 'track_beat_loop', deck: 'a', beats: 4 }))
    const loop = loopRegion(n2, 2, 4)
    expect(booth.deckA.setTrackLoopRegion).toHaveBeenLastCalledWith(loop.start, loop.end)
    expect(booth.deckA.beatLoop).not.toHaveBeenCalled()
    const deckPanel = screen.getByRole('group', { name: 'Deck a' })
    expect(within(deckPanel).getByRole('radio', { name: '4' })).toHaveAttribute('aria-checked', 'true')

    act(() => bus.publish({ kind: 'track_loop_double', deck: 'a' }))
    const eight = loopRegion(n2, 2, 8)
    expect(booth.deckA.setTrackLoopRegion).toHaveBeenLastCalledWith(eight.start, eight.end)

    act(() => bus.publish({ kind: 'track_beat_loop', deck: 'a', beats: 4 }))
    const hold = holdRegion(n2)
    expect(booth.deckA.setTrackLoopRegion).toHaveBeenLastCalledWith(hold.start, hold.end)
  })

  it('lets a loom status raised after a bounce line replace it in the bar', async () => {
    const bar = document.body.appendChild(document.createElement('div'))
    const booth = mountBooth(createControlBus(), bar)
    await flush()
    const deckA = screen.getByRole('group', { name: 'Deck a' })
    fireEvent.drop(deckA, { dataTransfer: { getData: () => 'loom:n2' } })
    await flush()
    const n2 = seedLoom().nodes.find((n) => n.id === 'n2')!
    booth.pollA({ loadId: 1, position: nodeStartSeconds(n2) + 2 * BAR })
    fireEvent.click(within(deckA).getByRole('button', { name: 'Bounce what this deck played to songs' }))
    await flush()
    expect(within(bar).getByRole('status')).toHaveTextContent('The bounce failed: no shell: stitch_songs')

    vi.mocked(booth.props.deckB.loadTrack).mockResolvedValueOnce(false)
    fireEvent.drop(screen.getByRole('group', { name: 'Deck b' }), { dataTransfer: { getData: () => 'loom:n3' } })
    await flush()
    expect(within(bar).getByRole('status')).toHaveTextContent('Deck B could not load')
    bar.remove()
  })

  it('shows the deck status line: the node, bars to an untied seam, the bar it is on', async () => {
    const booth = mountBooth(createControlBus())
    await flush()
    fireEvent.drop(screen.getByRole('group', { name: 'Deck a' }), { dataTransfer: { getData: () => 'loom:n2' } })
    await flush()
    const n2 = seedLoom().nodes.find((n) => n.id === 'n2')!
    booth.pollA({ loadId: 1, position: nodeEndSeconds(n2) - 3.5 * BAR })
    const deck = screen.getByRole('group', { name: 'Deck a' })
    expect(deck).toHaveTextContent('dub techno, rolling bassline')
    expect(deck).toHaveTextContent('4 → HOLD')
    expect(deck).toHaveTextContent('bar 13 / 16')
  })
})
