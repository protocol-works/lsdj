/** useDeck behaviour over the native (Tauri) transport, with fake timers and
 * a fake audio engine. The deck is "open" the instant it mounts — there is no
 * socket handshake. Status arrives as `sidecar://status` events (a captured
 * `event.listen` callback), model PCM arrives over a Tauri `Channel`, and
 * control is forwarded as `core.invoke` deck_* commands. This harness mocks
 * `globalThis.__TAURI__` for all three. The real audio graph stays on the e2e
 * script. */

import { act, renderHook } from '@testing-library/react'
import type { ReactNode } from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { AudioEngineProvider } from '../audio/AudioEngineProvider'
import { updateDeckSettings } from '../persistence'
import type {
  AudioEngine,
  DeckChannel,
  LoadedTrack,
  TrackSource,
} from '../audio/types'
import type { DeckSnap, InterfaceState } from '../audio/nativeEngine'
import { useDeck } from './useDeck'

/** The captured native transport: the latest `sidecar://status` callback, the
 * latest PCM `Channel` instance, and the `core.invoke` spy recording every
 * native command. The deck (deck 'a' = index 0) is the only one under test. */
type NativeChannel = { onmessage: ((buffer: ArrayBuffer) => void) | null }

/** The shipped mixer boot values (`DeckMixerSetting::default()` in Rust): the
 * shell hydrates the store to these before the webview exists (phase C), so
 * the harness store starts here too. */
const hydratedMixer = () => ({
  volume: 0.8,
  eq: { low: 0.5, mid: 0.5, high: 0.5 },
  fx: { kind: null, amount: 0 } as DeckSnap['fx'],
  trimDb: 0,
  cue: false,
})

const native: {
  invoke: ReturnType<typeof vi.fn>
  statusCb: ((e: { payload: { deck: number; json: string } }) => void) | null
  storeCb: ((e: { payload: InterfaceState }) => void) | null
  pcmChannel: NativeChannel | null
  /** The harness store's transport for deck 'a' (index 0): the Rust store owns
   * `playing` (ADR-0020), so the mock must too — deck_play/deck_stop write it and
   * echo `store://changed`, and every fired snapshot carries the current value. */
  storePlaying: boolean
  /** The harness store's live beat analysis for deck 'a' (ADR-0025): the Rust
   * store holds the last published value, so every snapshot carries it. */
  storeAnalysis: DeckSnap['analysis']
  /** The harness store's mixer for deck 'a' (ADR-0020 phase C): the channel's
   * mixer methods write it and echo — like the real commands — so a fired
   * snapshot never carries stale mixer values the gate-free adoption would
   * take for an external move. */
  storeMixer: ReturnType<typeof hydratedMixer>
  /** The harness store's hot-cue bank + track identity for deck 'a' (phase D):
   * set_deck_track opens/drops the bank with the identity, set_deck_cue_point
   * mutates one pad — the pads light only through this echo. */
  storeCues: (number | null)[]
  storeTrackKey: string | null
} = {
  invoke: vi.fn(),
  statusCb: null,
  storeCb: null,
  pcmChannel: null,
  storePlaying: false,
  storeAnalysis: { bpm: null, confidence: 0, liveBeat: null, originFrames: 0 },
  storeMixer: hydratedMixer(),
  storeCues: [],
  storeTrackKey: null,
}

function installNativeTauri() {
  native.statusCb = null
  native.storeCb = null
  native.pcmChannel = null
  native.storePlaying = false
  native.storeAnalysis = { bpm: null, confidence: 0, liveBeat: null, originFrames: 0 }
  native.storeMixer = hydratedMixer()
  native.storeCues = []
  native.storeTrackKey = null
  native.invoke = vi.fn((cmd: string, args?: unknown) => {
    // The Rust deck_play/deck_stop commands write the store's transport and the
    // store echoes a snapshot — with the real dedupe (a no-change mutation emits
    // nothing). The webview's button only lights through this round-trip.
    if (cmd === 'deck_play' && !native.storePlaying) fireStore({ playing: true })
    if (cmd === 'deck_stop' && native.storePlaying) fireStore({ playing: false })
    // The mixer commands write the Rust store and echo too (phase C) — the
    // gate-free adoption must always see the value it just wrote, never a
    // stale one riding a later snapshot.
    const a = (args ?? {}) as {
      gain?: number
      band?: 'low' | 'mid' | 'high'
      value?: number
      db?: number
      on?: boolean
      kind?: DeckSnap['fx']['kind']
      amount?: number
    }
    if (cmd === 'set_volume' && a.gain !== undefined) fireStore({ volume: a.gain })
    if (cmd === 'set_eq' && a.band && a.value !== undefined) {
      fireStore({ eq: { ...native.storeMixer.eq, [a.band]: a.value } })
    }
    if (cmd === 'set_trim' && a.db !== undefined) fireStore({ trimDb: a.db })
    if (cmd === 'set_cue' && a.on !== undefined) fireStore({ cue: a.on })
    // set_fx parks the amount at the kind's rest (the Rust store semantic).
    if (cmd === 'set_fx' && a.kind) {
      fireStore({ fx: { kind: a.kind, amount: a.kind === 'filter' ? 0.5 : 0 } })
    }
    if (cmd === 'clear_fx') fireStore({ fx: { kind: null, amount: 0 } })
    if (cmd === 'set_fx_amount' && a.amount !== undefined) {
      fireStore({ fx: { ...native.storeMixer.fx, amount: a.amount } })
    }
    // The store owns the hot cues (phase D): the bank lives and dies with the
    // track identity, and the pads light only through the echo.
    const cueArgs = (args ?? {}) as {
      index?: number
      seconds?: number | null
      track?: { title: string } | null
      deck?: number
    }
    if (cmd === 'set_deck_track' && cueArgs.deck === 0) {
      const key = cueArgs.track ? cueArgs.track.title : null
      if (key === null) {
        native.storeCues = []
      } else if (key !== native.storeTrackKey) {
        native.storeCues = Array<number | null>(8).fill(null)
      }
      native.storeTrackKey = key
      fireStore({ cues: [...native.storeCues] })
    }
    if (cmd === 'set_deck_cue_point' && cueArgs.index !== undefined) {
      if (cueArgs.index < native.storeCues.length) {
        const next = [...native.storeCues]
        next[cueArgs.index] = cueArgs.seconds ?? null
        native.storeCues = next
      }
      fireStore({ cues: [...native.storeCues] })
    }
    // app_info feeds getApiBaseUrl(); null port → '' (relative fetches).
    return cmd === 'app_info'
      ? Promise.resolve({ generationPort: null })
      : Promise.resolve(undefined)
  })
  class Channel {
    onmessage: ((buffer: ArrayBuffer) => void) | null = null
    constructor() {
      native.pcmChannel = this
    }
  }
  const listen = vi.fn(
    (event: string, cb: (e: { payload: unknown }) => void) => {
      if (event === 'sidecar://status')
        native.statusCb = cb as unknown as typeof native.statusCb
      if (event === 'store://changed')
        native.storeCb = cb as unknown as typeof native.storeCb
      return Promise.resolve(() => {})
    },
  )
  vi.stubGlobal('__TAURI__', {
    core: { invoke: native.invoke, Channel },
    event: { listen },
  })
  // /api/models — the deck_info fetch is irrelevant to these tests; keep it
  // benign so the deck-open effect never throws. Per-test stubs (stubFetchOk)
  // override this for the generated-pad suite.
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => ({ ok: false, status: 404, json: async () => ({}) })),
  )
}

/** The deck is open on mount (no handshake): serverOpen() is a no-op, kept so
 * existing call sites read clearly. serverEvent() fires the captured status
 * callback as that deck's status frame. The only deck under test is 'a'
 * (index 0); `socket(0)` reads like the old fake-WebSocket accessor. */
const socket = (deck: number) => ({
  serverOpen: () => {},
  serverEvent: (event: object) => {
    // The Rust status relay derives the transport (ADR-0020): a dying or
    // model-switching worker drops the store's `playing` before the event is
    // forwarded — emulate it, so the projection is what the tests exercise.
    const name = (event as { event?: string }).event
    if ((name === 'model_loading' || name === 'worker_died') && native.storePlaying) {
      fireStore({ playing: false })
    }
    native.statusCb?.({ payload: { deck, json: JSON.stringify(event) } })
  },
})

/** Fire the captured PCM Channel's onmessage with one raw f32 frame buffer —
 * the native replacement for the old socket PCM feed. */
function feedPcm(buffer: ArrayBuffer) {
  native.pcmChannel?.onmessage?.(buffer)
}

/** The harness store's deck snapshot: mixer fields ride `native.storeMixer`
 * (like `playing` rides `storePlaying`), the rest are the Rust defaults. */
function storeDeck(): DeckSnap {
  return {
    volume: native.storeMixer.volume,
    eq: { ...native.storeMixer.eq },
    trimDb: native.storeMixer.trimDb,
    cue: native.storeMixer.cue,
    onAir: true,
    fx: { ...native.storeMixer.fx },
    model: null,
    playing: false,
    mode: 'realtime',
    cues: [...native.storeCues],
    track: null,
    transport: null,
    loopLabels: [],
    styleTargets: [],
    styleSelected: [],
    cursor: { x: 0.5, y: 0.5 },
    primed: false,
    performance: { armed: false, key: 0, scale: 'major', mode: 'chord' },
    notes: null,
    drums: null,
    drumsStrength: 4,
    generation: { temperature: 1.1, topK: 50, cfgMusiccoca: 1.6, cfgNotes: 2.4 },
    analysis: { bpm: null, confidence: 0, liveBeat: null, originFrames: 0 },
    workerDied: false,
    switchingModel: false,
    shiftHeld: false,
  }
}

/** Fire a `store://changed` event with deck 'a' (index 0) carrying `mix`. Deck 0's
 * transport, analysis, and mixer ride the harness store: values in `mix` move
 * them, and every snapshot carries the current state — like the real
 * full-snapshot events, so unrelated churn never claims a playing deck stopped
 * (or hands the gate-free mixer adoption a stale value). */
function fireStore(mix: Partial<DeckSnap>) {
  if (mix.playing !== undefined) native.storePlaying = mix.playing
  if (mix.analysis !== undefined) native.storeAnalysis = mix.analysis
  if (mix.volume !== undefined) native.storeMixer.volume = mix.volume
  if (mix.eq !== undefined) native.storeMixer.eq = mix.eq
  if (mix.fx !== undefined) native.storeMixer.fx = mix.fx
  if (mix.trimDb !== undefined) native.storeMixer.trimDb = mix.trimDb
  if (mix.cue !== undefined) native.storeMixer.cue = mix.cue
  if (mix.cues !== undefined) native.storeCues = mix.cues
  const payload: InterfaceState = {
    decks: [
      {
        ...storeDeck(),
        playing: native.storePlaying,
        analysis: native.storeAnalysis,
        ...mix,
      },
      storeDeck(),
    ],
    crossfade: 0.5,
    cueMix: 0.5,
    recording: { active: false, path: null },
    mainDevice: '',
    cueDevice: '',
    recordingsFolder: '',
  }
  native.storeCb?.({ payload })
}

/** Publish a deck-0 live beat analysis the way the shell's analysis thread
 * does (ADR-0025): a store snapshot carrying the gated set. */
function fireAnalysis(analysis: Partial<DeckSnap['analysis']>) {
  fireStore({
    analysis: {
      bpm: null,
      confidence: 0,
      liveBeat: null,
      originFrames: 0,
      ...analysis,
    },
  })
}

/** The load reference the tests pass — the shell mock never reads it. */
const TEST_SOURCE: TrackSource = { kind: 'song', name: 'test-pressing.wav' }

/** A shell load verdict (ADR-0030). The default is honest silence: no tempo,
 * no grid, empty summaries — "silence in, honesty out" (M14). */
function loadedTrackDto(over: Partial<LoadedTrack> = {}): LoadedTrack {
  return {
    duration: 120,
    bpm: null,
    grid: null,
    bands: {
      low: new Float32Array(0),
      mid: new Float32Array(0),
      high: new Float32Array(0),
    },
    peaks: null,
    ...over,
  }
}

function makeFakeEngine(overrides: Partial<AudioEngine> = {}) {
  // Captured so tests can feed worklet stats into the deck (M20).
  const captured: { onStats: Parameters<AudioEngine['createDeckChannel']>[2] | null } = {
    onStats: null,
  }
  const channel: DeckChannel = {
    postPcm: vi.fn(),
    reset: vi.fn(),
    setVolume: vi.fn(),
    setEq: vi.fn(),
    setCue: vi.fn(),
    setFx: vi.fn(),
    setFxAmount: vi.fn(),
    // The shell blanks the published analysis on reset AND publishes the
    // blank snapshot (ADR-0025); mirror both, so a stale snapshot racing the
    // reset loses to the blank that follows — exactly the real convergence.
    resetAnalysis: vi.fn(() => {
      native.storeAnalysis = { bpm: null, confidence: 0, liveBeat: null, originFrames: 0 }
      fireStore({})
    }),
    setTrim: vi.fn(),
    setOnAir: vi.fn(),
    captureLoop: vi.fn(async () => true),
    loadGeneratedLoop: vi.fn(async () => true),
    playLoop: vi.fn(() => true),
    stopLoop: vi.fn(),
    stopOneShot: vi.fn(),
    clearLoop: vi.fn(),
    captureSample: vi.fn(async () => new Float32Array(2)),
    saveLoopSlot: vi.fn(async () => {}),
    saveGeneratedSample: vi.fn(async () => {}),
    stopLayer: vi.fn(),
    loadTrack: vi.fn(async () => loadedTrackDto()),
    playTrack: vi.fn(() => true),
    pauseTrack: vi.fn(),
    seekTrack: vi.fn(),
    setTrackLoop: vi.fn(),
    clearTrackLoop: vi.fn(),
    getTrackStatus: vi.fn(() => null),
    setTrackRate: vi.fn(),
    nudgeTrackPhase: vi.fn(),
    getTrackPeaks: vi.fn(() => null),
    unloadTrack: vi.fn(),
    getLevel: vi.fn(() => 0),
    dispose: vi.fn(),
  }
  const engine: AudioEngine = {
    // 0 keeps the engine's render clock and the played-frame domain consistent:
    // the de-branched useDeck anchors the live-beat origin at
    // getContextTime()·SR on each stream reset, and the M20 stats below report
    // playedFrames in the per-stream (origin-0) domain. A non-zero clock here
    // would offset the live-beat lattice by getContextTime() seconds and the
    // M20 beat-clock assertion would read a fractional beat (ADR-0014).
    getContextTime: vi.fn(() => 0),
    createDeckChannel: vi.fn(async (_deck, _initial, onStats) => {
      captured.onStats = onStats
      return channel
    }),
    resume: vi.fn(async () => {}),
    setCrossfade: vi.fn(),
    setCueMix: vi.fn(),
    auditionPlay: vi.fn(async () => {}),
    auditionStop: vi.fn(),
    listOutputDevices: vi.fn(async () => []),
    setMainDevice: vi.fn(async () => {}),
    setCueDevice: vi.fn(async () => {}),
    startRecording: vi.fn(async () => '/Downloads/lsdj-take.wav'),
    stopRecording: vi.fn(async () => {}),
    getMasterLevel: vi.fn(() => 0),
    getMasterGainReduction: vi.fn(() => 0),
    ...overrides,
  }
  return { engine, channel, captured }
}

function renderDeck(engine: AudioEngine) {
  const wrapper = ({ children }: { children: ReactNode }) => (
    <AudioEngineProvider engine={engine}>{children}</AudioEngineProvider>
  )
  return renderHook(() => useDeck('a'), { wrapper })
}

beforeEach(() => {
  vi.useFakeTimers()
  installNativeTauri()
})

afterEach(() => {
  vi.unstubAllGlobals()
  vi.useRealTimers()
  // Restore any vi.spyOn factories (e.g. the beat-tracker push spy) so they
  // never leak into the next test's real tracker.
  vi.restoreAllMocks()
})

/** The recorded native deck CONTROL commands in order — the native analogue of
 * the old `socket.sent`. Excludes the transport-setup invokes (`app_info` for
 * getApiBaseUrl, `subscribe_deck_pcm`/`unsubscribe_deck_pcm` for the PCM tee),
 * which the deck-open effect fires regardless of what the user does. */
function deckInvokes() {
  return native.invoke.mock.calls.filter(([cmd]) => (cmd as string).startsWith('deck_'))
}

/** The mixer command invokes (phase C: gestures are deck-indexed intents,
 * rAF-coalesced) — flush the frame first so pending ones land. */
function mixerInvokes(cmd: string) {
  act(() => void vi.advanceTimersByTime(20))
  return native.invoke.mock.calls.filter(([c]) => c === cmd)
}


describe('useDeck connection', () => {
  it('is open on mount with no handshake', () => {
    const { result } = renderDeck(makeFakeEngine().engine)
    // The native transport has no socket: the deck is operable at once.
    expect(result.current.state.connection).toBe('open')
  })

  it('surfaces a play() audio failure instead of swallowing it', async () => {
    const { engine } = makeFakeEngine({
      createDeckChannel: vi.fn(async () => {
        throw new Error('worklet failed to load')
      }),
    })
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())

    await act(() => result.current.play())
    expect(result.current.state.error).toBe('worklet failed to load')
    expect(result.current.state.playing).toBe(false)
    // No play command without audio.
    expect(native.invoke).not.toHaveBeenCalledWith('deck_play', { deck: 0 })
  })

  it('plays through the shared engine and resets stale buffer first', async () => {
    const { engine, channel } = makeFakeEngine()
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())

    await act(() => result.current.play())
    expect(engine.resume).toHaveBeenCalled()
    expect(channel.reset).toHaveBeenCalled()
    expect(deckInvokes()).toEqual([['deck_play', { deck: 0 }]])
    expect(result.current.state.playing).toBe(true)
  })

  it('flushes the ring buffer when a model switch starts', async () => {
    const { engine, channel } = makeFakeEngine()
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())
    await act(() => result.current.play())
    const resetsBefore = vi.mocked(channel.reset).mock.calls.length

    act(() => socket(0).serverEvent({ event: 'model_loading', model: 'mrt2_base' }))
    expect(vi.mocked(channel.reset).mock.calls.length).toBe(resetsBefore + 1)
    expect(result.current.state.switchingModel).toBe(true)
    expect(result.current.state.playing).toBe(false)
  })

  it('silences the ring buffer when the worker dies', async () => {
    const { engine, channel } = makeFakeEngine()
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())
    await act(() => result.current.play())
    const resetsBefore = vi.mocked(channel.reset).mock.calls.length

    act(() => socket(0).serverEvent({ event: 'worker_died', model: 'mrt2_small' }))
    expect(vi.mocked(channel.reset).mock.calls.length).toBe(resetsBefore + 1)
    expect(result.current.state.workerDied).toBe(true)
    // The Rust relay dropped the store's transport; the projection followed.
    expect(result.current.state.playing).toBe(false)
  })

  it('maps set_model and restart to native deck_set_model', () => {
    const { result } = renderDeck(makeFakeEngine().engine)
    act(() => socket(0).serverOpen())

    // The worker must report a model first, so restart carries one through.
    act(() => socket(0).serverEvent({ event: 'ready', deck: 'a', model: 'mrt2_base' }))
    act(() => result.current.setModel('mrt2_base'))
    act(() => result.current.restartWorker())
    // Both control gestures collapse to deck_set_model on the native path:
    // set_model with its target, restart re-using the current model.
    expect(deckInvokes()).toEqual([
      ['deck_set_model', { deck: 0, model: 'mrt2_base' }],
      ['deck_set_model', { deck: 0, model: 'mrt2_base' }],
    ])
  })

  it('adopts the shell-hydrated EQ and routes band changes as intents', async () => {
    const { engine } = makeFakeEngine()
    const { result } = renderDeck(engine)
    // The shell hydrated engine + store before the webview existed (phase C);
    // the first snapshot carries the persisted EQ.
    act(() => fireStore({ eq: { low: 0.2, mid: 0.5, high: 0.9 } }))
    expect(result.current.eq).toEqual({ low: 0.2, mid: 0.5, high: 0.9 })

    act(() => socket(0).serverOpen())
    await act(() => result.current.play())
    // The channel was built with the adopted EQ…
    expect(vi.mocked(engine.createDeckChannel).mock.calls[0][1]).toMatchObject({
      eq: { low: 0.2, mid: 0.5, high: 0.9 },
    })
    // …and live band moves cross as deck-indexed intents.
    act(() => result.current.setEqBand('low', 0))
    expect(mixerInvokes('set_eq').at(-1)?.[1]).toEqual({ deck: 0, band: 'low', value: 0 })
    expect(result.current.eq.low).toBe(0)
  })

  it('adopts the shell-hydrated volume', () => {
    const { result } = renderDeck(makeFakeEngine().engine)
    act(() => fireStore({ volume: 0.55 }))
    expect(result.current.volume).toBe(0.55)
  })

  it('primes off air and drops on air without flushing the prepped buffer', async () => {
    const { engine, channel } = makeFakeEngine()
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())

    await act(() => result.current.prime())
    expect(channel.setOnAir).toHaveBeenLastCalledWith(false)
    expect(result.current.primed).toBe(true)
    expect(deckInvokes()).toEqual([['deck_play', { deck: 0 }]])

    vi.mocked(channel.reset).mockClear()
    await act(() => result.current.play())
    expect(channel.setOnAir).toHaveBeenLastCalledWith(true)
    expect(result.current.primed).toBe(false)
    // The drop must not flush the prepped audio or re-send play.
    expect(channel.reset).not.toHaveBeenCalled()
    expect(deckInvokes()).toEqual([['deck_play', { deck: 0 }]])
  })

  it('stop while primed flushes and puts the channel back on air', async () => {
    const { engine, channel } = makeFakeEngine()
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())

    await act(() => result.current.prime())
    act(() => result.current.stop())
    expect(result.current.primed).toBe(false)
    expect(channel.reset).toHaveBeenCalled()
    expect(channel.setOnAir).toHaveBeenLastCalledWith(true)
  })

  it('adopts shell-hydrated FX, routes changes, and parks the knob on switch', async () => {
    const { engine } = makeFakeEngine()
    const { result } = renderDeck(engine)
    act(() => fireStore({ fx: { kind: 'dubEcho', amount: 0.6 } }))
    expect(result.current.fx).toEqual({ kind: 'dub_echo', amount: 0.6 })

    act(() => socket(0).serverOpen())
    await act(() => result.current.play())
    // The channel is built with the adopted effect…
    expect(vi.mocked(engine.createDeckChannel).mock.calls[0][1]).toMatchObject({
      fx: { kind: 'dub_echo', amount: 0.6 },
    })

    // …live knob moves cross as intents…
    act(() => result.current.setFxAmount(0.8))
    expect(mixerInvokes('set_fx_amount').at(-1)?.[1]).toEqual({ deck: 0, amount: 0.8 })

    // …and switching to the bipolar filter parks the knob at centre — ONE
    // discrete set_fx (the Rust side records kind + rest amount together).
    act(() => result.current.setFx('filter'))
    expect(result.current.fx).toEqual({ kind: 'filter', amount: 0.5 })
    expect(mixerInvokes('set_fx').at(-1)?.[1]).toEqual({ deck: 0, kind: 'filter' })
  })

  it('routes a cue toggle as an intent even before the channel exists', async () => {
    const { engine } = makeFakeEngine()
    const { result } = renderDeck(engine)

    // Toggled before the channel exists — the deck-indexed command reaches
    // the engine and the store regardless (phase C).
    act(() => result.current.setCue(true))
    expect(result.current.cue).toBe(true)
    expect(mixerInvokes('set_cue').at(-1)?.[1]).toEqual({ deck: 0, on: true })

    act(() => socket(0).serverOpen())
    await act(() => result.current.play())
    act(() => result.current.setCue(false))
    expect(mixerInvokes('set_cue').at(-1)?.[1]).toEqual({ deck: 0, on: false })
    expect(result.current.cue).toBe(false)
  })

  it('drops a malformed status frame without crashing', () => {
    const { result } = renderDeck(makeFakeEngine().engine)
    act(() => socket(0).serverOpen())

    // A malformed status line is dropped inside subscribeSidecarStatus, never
    // fatal; the deck stays operable.
    act(() => native.statusCb?.({ payload: { deck: 0, json: '{not json' } }))
    expect(result.current.state.connection).toBe('open')
  })
})

describe('useDeck beat readout (ADR-0025: the shell measures, the store carries)', () => {
  // The estimator, honesty gate, and anchor agreement live in the Rust shell
  // now (their behaviour is covered by `src-tauri/src/analysis/beat.rs` and
  // the corpus regression); the deck PROJECTS the published gated set. These
  // tests exercise that projection and the reset round-trip.

  it('mirrors the store-published gated BPM and blanks locally on stop', async () => {
    const { engine, channel } = makeFakeEngine()
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())
    await act(() => result.current.play())

    act(() => fireAnalysis({ bpm: 128, confidence: 0.62 }))
    expect(result.current.bpm).toBe(128)

    // Stop resets the shell's analysis (estimates never span streams) and
    // blanks the local mirror at once — the readout must not flash the dead
    // stream's number while the store round-trips.
    act(() => result.current.stop())
    expect(channel.resetAnalysis).toHaveBeenCalled()
    expect(result.current.bpm).toBeNull()
  })

  it('a blank published set reads as a blank readout', async () => {
    const { engine } = makeFakeEngine()
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())
    await act(() => result.current.play())

    act(() => fireAnalysis({ bpm: null, confidence: 0.2 }))
    expect(result.current.bpm).toBeNull()
  })

  it('ignores live analysis while a track is loaded (ADR-0013)', async () => {
    const { engine, channel } = makeFakeEngine()
    vi.mocked(channel.loadTrack).mockResolvedValue(
      loadedTrackDto({ duration: 24, bpm: 120 }),
    )
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())
    await act(async () => {
      await result.current.loadTrack(TEST_SOURCE, 'Gridded')
    })
    // A straggler publish for the parked stream must not disturb the
    // track's clock (its tempo is the load-time analysis).
    act(() => fireAnalysis({ bpm: 133 }))
    expect(result.current.bpm).toBeNull()
    expect(result.current.track!.bpm).toBe(120)
  })

  it('quantises a capture to whole beats when the gate is confident', async () => {
    const { engine, channel } = makeFakeEngine()
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())
    await act(() => result.current.play())

    act(() => fireAnalysis({ bpm: 128, confidence: 0.62 }))
    const bpm = result.current.bpm!

    await act(async () => result.current.toggleLoopPad(0))
    const seconds = vi.mocked(channel.captureLoop).mock.calls.at(-1)![1]
    const beats = (seconds * bpm) / 60
    expect(Math.abs(beats - Math.round(beats))).toBeLessThan(1e-6)
    expect(seconds).not.toBe(4) // 4 s is off-grid at 128 bpm
  })

  it('forgets the stream across a model switch', async () => {
    const { engine, channel } = makeFakeEngine()
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())
    await act(() => result.current.play())
    act(() => fireAnalysis({ bpm: 128, confidence: 0.62 }))
    expect(result.current.bpm).not.toBeNull()

    act(() => socket(0).serverEvent({ event: 'model_loading', model: 'mrt2_base' }))
    expect(channel.resetAnalysis).toHaveBeenCalled()
    expect(result.current.bpm).toBeNull()
  })
})

describe('useDeck trim (auto-gain)', () => {
  function streamConstant(amplitude: number, seconds: number) {
    const chunk = new Float32Array(1920 * 2).fill(amplitude)
    const chunks = Math.ceil((seconds * 48_000) / 1920)
    act(() => {
      for (let i = 0; i < chunks; i++) {
        feedPcm(chunk.slice().buffer)
      }
    })
  }

  it('auto-trims a quiet stream up toward the loudness target', async () => {
    const { engine } = makeFakeEngine()
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())
    await act(() => result.current.play())

    // Constant 0.075 → RMS 0.075, half the 0.15 target → +6 dB.
    streamConstant(0.075, 12)
    act(() => void vi.advanceTimersByTime(1_000))
    expect(result.current.trim.mode).toBe('auto')
    expect(result.current.trim.db).toBeCloseTo(6, 0)
    const trimArg = mixerInvokes('set_trim').at(-1)?.[1] as { deck: number; db: number }
    expect(trimArg.db).toBeCloseTo(6, 0)
  })

  it('holds the trim over silence instead of winding up', async () => {
    const { engine } = makeFakeEngine()
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())
    await act(() => result.current.play())

    streamConstant(0, 12)
    act(() => void vi.advanceTimersByTime(2_000))
    expect(result.current.trim.db).toBe(0)
    expect(mixerInvokes('set_trim')).toHaveLength(0)
  })

  it('a manual move takes over until AUTO re-engages', async () => {
    const { engine } = makeFakeEngine()
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())
    await act(() => result.current.play())

    act(() => result.current.setTrimDb(-3))
    expect(result.current.trim).toEqual({ mode: 'manual', db: -3 })
    expect(mixerInvokes('set_trim').at(-1)?.[1]).toEqual({ deck: 0, db: -3 })

    // Auto must not fight the manual value on the next tick.
    streamConstant(0.075, 12)
    act(() => void vi.advanceTimersByTime(1_000))
    expect(result.current.trim).toEqual({ mode: 'manual', db: -3 })

    act(() => result.current.enableAutoTrim())
    expect(result.current.trim.mode).toBe('auto')
    expect(result.current.trim.db).toBeCloseTo(6, 0)
  })

  it('seeds the shell-hydrated trim value without stealing the persisted mode', async () => {
    // The MODE stays webview-persisted (the auto tracker is TS); the VALUE
    // hydrates from the store — the shell wrote it before the webview
    // existed, so the FIRST snapshot the deck sees already carries it and
    // must seed without flipping the deck to manual (boot, not a gesture).
    updateDeckSettings('a', { trimMode: 'manual' })
    native.storeMixer.trimDb = -4.5
    const { engine } = makeFakeEngine()
    const { result } = renderDeck(engine)
    expect(result.current.trim.mode).toBe('manual')

    act(() => fireStore({}))
    expect(result.current.trim).toEqual({ mode: 'manual', db: -4.5 })

    act(() => socket(0).serverOpen())
    await act(() => result.current.play())
    expect(vi.mocked(engine.createDeckChannel).mock.calls[0][1]).toMatchObject({
      trimDb: -4.5,
    })
  })

  it('boot seeding keeps auto mode; a later external trim flips to manual', () => {
    native.storeMixer.trimDb = -3
    const { result } = renderDeck(makeFakeEngine().engine)

    // First snapshot = hydration: the value seeds, auto survives.
    act(() => fireStore({}))
    expect(result.current.trim).toEqual({ mode: 'auto', db: -3 })

    // A later differing trim is a deliberate external (MCP/MIDI) move.
    act(() => fireStore({ trimDb: 2 }))
    expect(result.current.trim).toEqual({ mode: 'manual', db: 2 })
  })
})

describe('useDeck freeze loops', () => {
  async function playingDeck(engine: AudioEngine) {
    const rendered = renderDeck(engine)
    act(() => socket(0).serverOpen())
    await act(() => rendered.result.current.play())
    return rendered
  }

  it('captures into an empty slot and freezes onto it in one press', async () => {
    const { engine, channel } = makeFakeEngine()
    const { result } = await playingDeck(engine)

    await act(async () => result.current.toggleLoopPad(1))
    expect(channel.captureLoop).toHaveBeenCalledWith(1, 4)
    expect(channel.playLoop).toHaveBeenCalledWith(1, false)
    expect(result.current.loop.slots[1].state).toBe('filled')
    expect(result.current.loop.active).toBe(1)
  })

  it('auto-saves the freeze to the samples library after the capture', async () => {
    const { engine, channel } = makeFakeEngine()
    const { result } = await playingDeck(engine)

    await act(async () => result.current.toggleLoopPad(1))
    // The freeze persists the EXACT slot buffer server-side (ADR-0022): the deck only
    // sends the slot coordinates and a loop's metadata, never the audio.
    expect(channel.saveLoopSlot).toHaveBeenCalledWith(
      1,
      expect.objectContaining({ model: 'freeze', oneShot: false }),
    )
  })

  it('returns to live on a second press, keeping the capture', async () => {
    const { engine, channel } = makeFakeEngine()
    const { result } = await playingDeck(engine)
    await act(async () => result.current.toggleLoopPad(0))

    await act(async () => result.current.toggleLoopPad(0))
    expect(channel.stopLoop).toHaveBeenCalled()
    expect(result.current.loop.active).toBeNull()
    expect(result.current.loop.slots[0].state).toBe('filled')
  })

  it('swaps onto a filled slot without recapturing', async () => {
    const { engine, channel } = makeFakeEngine()
    const { result } = await playingDeck(engine)
    await act(async () => result.current.toggleLoopPad(0))
    await act(async () => result.current.toggleLoopPad(1))
    vi.mocked(channel.captureLoop).mockClear()

    await act(async () => result.current.toggleLoopPad(0))
    expect(channel.captureLoop).not.toHaveBeenCalled()
    expect(channel.playLoop).toHaveBeenLastCalledWith(0, false)
    expect(result.current.loop.active).toBe(0)
  })

  it('refuses the press when too little has played to loop', async () => {
    const { engine, channel } = makeFakeEngine()
    vi.mocked(channel.captureLoop).mockResolvedValue(false)
    const { result } = await playingDeck(engine)

    await act(async () => result.current.toggleLoopPad(0))
    expect(channel.playLoop).not.toHaveBeenCalled()
    expect(result.current.loop.slots[0].state).toBe('empty')
    expect(result.current.loop.active).toBeNull()
  })

  it('is a safe no-op before the channel exists', () => {
    const { engine, channel } = makeFakeEngine()
    const { result } = renderDeck(engine)

    act(() => result.current.toggleLoopPad(0))
    expect(channel.captureLoop).not.toHaveBeenCalled()
    expect(result.current.loop.slots[0].state).toBe('empty')
  })

  it('stop() drops the loop but keeps the capture', async () => {
    const { engine, channel } = makeFakeEngine()
    const { result } = await playingDeck(engine)
    await act(async () => result.current.toggleLoopPad(2))

    act(() => result.current.stop())
    expect(channel.stopLoop).toHaveBeenCalled()
    expect(result.current.loop.active).toBeNull()
    expect(result.current.loop.slots[2].state).toBe('filled')
  })

  it('clears a slot, stopping it first when it is the active one', async () => {
    const { engine, channel } = makeFakeEngine()
    const { result } = await playingDeck(engine)
    await act(async () => result.current.toggleLoopPad(3))

    act(() => result.current.clearLoopPad(3))
    expect(channel.stopLoop).toHaveBeenCalled()
    expect(channel.clearLoop).toHaveBeenCalledWith(3)
    expect(result.current.loop.slots[3].state).toBe('empty')
    expect(result.current.loop.active).toBeNull()
  })

  it('a STOP during the capture round-trip wins over the stale capture', async () => {
    const { engine, channel } = makeFakeEngine()
    let finishCapture!: (captured: boolean) => void
    vi.mocked(channel.captureLoop).mockImplementation(
      () => new Promise((resolve) => (finishCapture = resolve)),
    )
    const { result } = await playingDeck(engine)

    act(() => result.current.toggleLoopPad(0))
    act(() => result.current.stop())
    await act(async () => finishCapture(true))

    expect(channel.playLoop).not.toHaveBeenCalled()
    expect(result.current.loop.slots[0].state).toBe('empty')
    expect(result.current.loop.active).toBeNull()
  })

  it('restores the persisted loop length and captures with it', async () => {
    updateDeckSettings('a', { loopSeconds: 8 })
    const { engine, channel } = makeFakeEngine()
    const { result } = await playingDeck(engine)
    expect(result.current.loop.seconds).toBe(8)

    await act(async () => result.current.toggleLoopPad(0))
    expect(channel.captureLoop).toHaveBeenCalledWith(0, 8)
  })

  it('routes a live length change into the next capture', async () => {
    updateDeckSettings('a', { loopSeconds: 4 })
    const { engine, channel } = makeFakeEngine()
    const { result } = await playingDeck(engine)

    act(() => result.current.setLoopSeconds(2))
    await act(async () => result.current.toggleLoopPad(0))
    expect(channel.captureLoop).toHaveBeenCalledWith(0, 2)
  })
})

describe('useDeck generated pads', () => {
  async function playingDeck(engine: AudioEngine) {
    const rendered = renderDeck(engine)
    act(() => socket(0).serverOpen())
    await act(() => rendered.result.current.play())
    return rendered
  }

  function stubFetchOk() {
    const fetchMock = vi.fn(async () => ({
      ok: true,
      arrayBuffer: async () => new ArrayBuffer(16),
      json: async () => ({}),
    }))
    vi.stubGlobal('fetch', fetchMock)
    return fetchMock
  }

  function requestBody(fetchMock: ReturnType<typeof vi.fn>) {
    const [, init] = fetchMock.mock.calls.at(-1)! as [string, { body: string }]
    return JSON.parse(init.body) as {
      prompt: string
      seconds: number
      kind: string
    }
  }

  /** The generation requests only — the deck-open effect's `/api/models` fetch
   * (native: no `hello` carries the model list) shares this `fetch` mock. */
  function generateCalls(fetchMock: ReturnType<typeof vi.fn>) {
    return fetchMock.mock.calls.filter(([url]) =>
      /\/api\/(generate|render)$/.test(url as string),
    )
  }

  it('generates an sfx one-shot into the first empty slot', async () => {
    const fetchMock = stubFetchOk()
    const { engine, channel } = makeFakeEngine()
    const { result } = await playingDeck(engine)

    act(() => result.current.generateToPad('  vinyl spinback  ', 'sfx', true))
    expect(result.current.loop.slots[0]).toEqual({
      state: 'pending',
      label: 'vinyl spinback',
      oneShot: true,
    })

    await act(async () => {})
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/generate',
      expect.objectContaining({ method: 'POST' }),
    )
    expect(requestBody(fetchMock)).toEqual({
      prompt: 'vinyl spinback',
      seconds: 4,
      kind: 'sfx',
    })
    expect(channel.loadGeneratedLoop).toHaveBeenCalledWith(
      0,
      expect.any(ArrayBuffer),
      true,
    )
    expect(result.current.loop.slots[0]).toEqual({
      state: 'filled',
      label: 'vinyl spinback',
      oneShot: true,
      // A deck-generated pad keeps the replace behaviour; only loaded samples layer.
      layer: false,
    })
    expect(result.current.loop.active).toBeNull()
  })

  it('rides a LoRA stack with per-adapter strengths on an SA3 pad generation (issue #66)', async () => {
    const fetchMock = stubFetchOk()
    const { engine } = makeFakeEngine()
    const { result } = await playingDeck(engine)

    act(() =>
      result.current.generateToPad('oud phrase', 'sfx', true, [
        { name: 'small/maqam', strength: 0.75 },
        { name: 'small/crackle', strength: 1 },
      ]),
    )
    await act(async () => {})
    expect(requestBody(fetchMock)).toEqual({
      prompt: 'oud phrase',
      seconds: 4,
      kind: 'sfx',
      loras: [
        { name: 'small/maqam', strength: 0.75 },
        { name: 'small/crackle', strength: 1 },
      ],
    })
  })

  it('omits the loras field when the stack is empty', async () => {
    const fetchMock = stubFetchOk()
    const { engine } = makeFakeEngine()
    const { result } = await playingDeck(engine)

    act(() => result.current.generateToPad('air horn', 'sfx', true, []))
    await act(async () => {})
    expect(requestBody(fetchMock)).toEqual({
      prompt: 'air horn',
      seconds: 4,
      kind: 'sfx',
    })
  })

  it('auto-saves a generated pad to the samples library (raw WAV + metadata)', async () => {
    stubFetchOk()
    const { engine, channel } = makeFakeEngine()
    const { result } = await playingDeck(engine)

    act(() => result.current.generateToPad('air horn', 'sfx', true))
    await act(async () => {})
    // The raw backend WAV is persisted (it carries the seam surplus), tagged with the
    // prompt/engine and the one-shot flag reload needs.
    expect(channel.saveGeneratedSample).toHaveBeenCalledWith(
      expect.any(ArrayBuffer),
      expect.objectContaining({ prompt: 'air horn', model: 'sfx', oneShot: true }),
    )
  })

  it('loads a saved sample into the first empty slot', async () => {
    const { engine, channel } = makeFakeEngine()
    const { result } = await playingDeck(engine)
    const wav = new ArrayBuffer(8)

    let ok = false
    await act(async () => {
      ok = await result.current.loadSampleToSlot(wav, false, 'break')
    })
    expect(ok).toBe(true)
    expect(channel.loadGeneratedLoop).toHaveBeenCalledWith(0, wav, false)
    expect(result.current.loop.slots[0]).toEqual({
      state: 'filled',
      label: 'break',
      oneShot: false,
      // A loaded sample loop is a layer (ADR-0022).
      layer: true,
    })
  })

  it('layers a loaded sample on press, stacks a second, and stops each independently', async () => {
    const { engine, channel } = makeFakeEngine()
    const { result } = await playingDeck(engine)
    // Two loaded-sample loops fill slots 0 and 1 (layer slots, not played yet).
    await act(async () => {
      await result.current.loadSampleToSlot(new ArrayBuffer(8), false, 'riff')
    })
    await act(async () => {
      await result.current.loadSampleToSlot(new ArrayBuffer(8), false, 'break')
    })

    // Press slot 0: it LAYERS (sum on top), never becomes the replacing `active`.
    act(() => result.current.toggleLoopPad(0))
    expect(channel.playLoop).toHaveBeenLastCalledWith(0, true)
    expect(result.current.loop.layering).toEqual([0])
    expect(result.current.loop.active).toBeNull()

    // Press slot 1: it stacks alongside slot 0.
    act(() => result.current.toggleLoopPad(1))
    expect(channel.playLoop).toHaveBeenLastCalledWith(1, true)
    expect(result.current.loop.layering).toEqual([0, 1])

    // Press slot 0 again: only that layer stops; slot 1 keeps playing.
    act(() => result.current.toggleLoopPad(0))
    expect(channel.stopLayer).toHaveBeenCalledWith(0)
    expect(result.current.loop.layering).toEqual([1])
  })

  it('refuses to load a sample when every slot is full', async () => {
    const { engine, channel } = makeFakeEngine()
    const { result } = await playingDeck(engine)
    for (const slot of [0, 1, 2, 3]) {
      await act(async () => result.current.toggleLoopPad(slot))
    }
    vi.mocked(channel.loadGeneratedLoop).mockClear()

    let ok = true
    await act(async () => {
      ok = await result.current.loadSampleToSlot(new ArrayBuffer(8), false, 'break')
    })
    expect(ok).toBe(false)
    expect(channel.loadGeneratedLoop).not.toHaveBeenCalled()
    expect(result.current.generateError).toBeTruthy()
  })

  it('shapes a loop by the locked tempo: whole bars, BPM in the prompt', async () => {
    const fetchMock = stubFetchOk()
    const { engine } = makeFakeEngine()
    const { result } = await playingDeck(engine)
    act(() => fireAnalysis({ bpm: 128, confidence: 0.62 }))
    const bpm = result.current.bpm!

    act(() => result.current.generateToPad('deep house groove', 'music', false))
    await act(async () => {})
    const body = requestBody(fetchMock)
    expect(body.kind).toBe('music')
    expect(body.prompt).toBe(`deep house groove, ${Math.round(bpm)} BPM`)
    // The request carries the seam surplus on top of a whole-bar length
    // that clears the model's quality floor.
    const bars = ((body.seconds - 0.03) * bpm) / 60 / 4
    expect(Math.abs(bars - Math.round(bars))).toBeLessThan(1e-6)
    expect(body.seconds).toBeGreaterThanOrEqual(7)
  })

  it('renders through the third Magenta engine', async () => {
    const fetchMock = stubFetchOk()
    const { engine, channel } = makeFakeEngine()
    const { result } = await playingDeck(engine)
    // A locked tempo is live, yet the Magenta request carries no BPM stamp.
    act(() => fireAnalysis({ bpm: 128, confidence: 0.62 }))

    act(() => result.current.generateToPad('dub chords', 'magenta', false))
    await act(async () => {})
    const [url] = fetchMock.mock.calls.at(-1)! as unknown as [string]
    expect(url).toBe('/api/render')
    const body = requestBody(fetchMock)
    // No kind field, no BPM stamp (Magenta ignores tempo text by
    // design), no sm-music quality floor — the picker's length plus
    // the seam surplus.
    expect(body).toEqual({ prompt: 'dub chords', seconds: 4.03 })
    expect(channel.loadGeneratedLoop).toHaveBeenCalledWith(
      0,
      expect.any(ArrayBuffer),
      false,
    )
  })

  it('an sfx-model loop keeps the picker length without the music floor', async () => {
    const fetchMock = stubFetchOk()
    const { engine } = makeFakeEngine()
    const { result } = await playingDeck(engine)

    act(() => result.current.generateToPad('crackle texture', 'sfx', false))
    await act(async () => {})
    const body = requestBody(fetchMock)
    expect(body.kind).toBe('sfx')
    expect(body.seconds).toBeCloseTo(4 + 0.03, 6)
  })

  it('floors the length and keeps the bare prompt while the gate is blank', async () => {
    const fetchMock = stubFetchOk()
    const { engine } = makeFakeEngine()
    const { result } = await playingDeck(engine)

    act(() => result.current.generateToPad('dub siren', 'music', false))
    await act(async () => {})
    const body = requestBody(fetchMock)
    expect(body.prompt).toBe('dub siren')
    // 4 s from the picker would come back garbled (the measured floor).
    expect(body.seconds).toBeCloseTo(7 + 0.03, 6)
  })

  it('fires a filled one-shot as an overlay, never as the active loop', async () => {
    stubFetchOk()
    const { engine, channel } = makeFakeEngine()
    const { result } = await playingDeck(engine)
    act(() => result.current.generateToPad('air horn', 'sfx', true))
    await act(async () => {})

    await act(async () => result.current.toggleLoopPad(0))
    expect(channel.playLoop).toHaveBeenCalledWith(0, false)
    expect(result.current.loop.active).toBeNull()
  })

  it('a clear during the flight wins over the result', async () => {
    let finish!: (response: unknown) => void
    vi.stubGlobal(
      'fetch',
      vi.fn(() => new Promise((resolve) => (finish = resolve))),
    )
    const { engine, channel } = makeFakeEngine()
    const { result } = await playingDeck(engine)

    act(() => result.current.generateToPad('riser', 'sfx', true))
    act(() => result.current.clearLoopPad(0))
    await act(async () =>
      finish({
        ok: true,
        arrayBuffer: async () => new ArrayBuffer(16),
        json: async () => ({}),
      }),
    )

    expect(channel.loadGeneratedLoop).not.toHaveBeenCalled()
    expect(result.current.loop.slots[0].state).toBe('empty')
  })

  it('reverts the slot and surfaces the backend detail on failure', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => ({
        ok: false,
        status: 503,
        json: async () => ({ detail: 'sa3_mlx checkout not found' }),
      })),
    )
    const { engine } = makeFakeEngine()
    const { result } = await playingDeck(engine)

    act(() => result.current.generateToPad('riser', 'sfx', true))
    await act(async () => {})
    expect(result.current.loop.slots[0].state).toBe('empty')
    expect(result.current.generateError).toBe('sa3_mlx checkout not found')

    // The next attempt starts clean.
    stubFetchOk()
    act(() => result.current.generateToPad('riser', 'sfx', true))
    expect(result.current.generateError).toBeNull()
  })

  it('a refused load is a failure, not a filled slot', async () => {
    stubFetchOk()
    const { engine, channel } = makeFakeEngine()
    // The engine declines the pad (false) — e.g. the deck is not Realtime.
    vi.mocked(channel.loadGeneratedLoop).mockResolvedValue(false)
    const { result } = await playingDeck(engine)

    act(() => result.current.generateToPad('riser', 'sfx', true))
    await act(async () => {})
    expect(result.current.loop.slots[0].state).toBe('empty')
    expect(result.current.generateError).toMatch(/could not load the generated pad/)
  })

  it('does nothing without an empty slot or a prompt', async () => {
    const fetchMock = stubFetchOk()
    const { engine, channel } = makeFakeEngine()
    const { result } = await playingDeck(engine)

    act(() => result.current.generateToPad('   ', 'sfx', true))
    expect(generateCalls(fetchMock)).toHaveLength(0)

    for (let slot = 0; slot < 4; slot++) {
      await act(async () => result.current.toggleLoopPad(slot))
    }
    vi.mocked(channel.captureLoop).mockClear()
    act(() => result.current.generateToPad('riser', 'sfx', true))
    expect(generateCalls(fetchMock)).toHaveLength(0)
  })

  it('stop() cuts a ringing one-shot', async () => {
    const { engine, channel } = makeFakeEngine()
    const { result } = await playingDeck(engine)

    act(() => result.current.stop())
    expect(channel.stopOneShot).toHaveBeenCalled()
  })

  it('creates the channel on demand: pads fill before the deck plays', async () => {
    stubFetchOk()
    const { engine, channel } = makeFakeEngine()
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())

    act(() => result.current.generateToPad('air horn', 'sfx', true))
    await act(async () => {})
    expect(engine.createDeckChannel).toHaveBeenCalled()
    expect(channel.loadGeneratedLoop).toHaveBeenCalled()
    expect(result.current.loop.slots[0].state).toBe('filled')
  })
})

describe('useDeck playback mode (M19)', () => {
  async function loadedDeck() {
    const { engine, channel } = makeFakeEngine()
    const rendered = renderDeck(engine)
    act(() => socket(0).serverOpen())
    let loaded = false
    await act(async () => {
      loaded = await rendered.result.current.loadTrack(TEST_SOURCE, 'Test Pressing')
    })
    expect(loaded).toBe(true)
    return { ...rendered, channel }
  }

  it('loadTrack parks the stream and enters playback with the offline verdict', async () => {
    const { result, channel } = await loadedDeck()
    expect(channel.loadTrack).toHaveBeenCalledWith(TEST_SOURCE, expect.any(Number))
    // The worker parks warm: a stop went over IPC (ADR-0013).
    expect(native.invoke).toHaveBeenCalledWith('deck_stop', { deck: 0 })
    expect(result.current.mode).toBe('playback')
    expect(result.current.track).toMatchObject({
      title: 'Test Pressing',
      duration: 120,
      position: 0,
      playing: false,
      ended: false,
      bpm: null, // silence in, honesty out (M14) — the shell's verdict
    })
  })

  it('a shell load refusal propagates its reason and leaves the deck live', async () => {
    const { engine, channel } = makeFakeEngine()
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())
    // The shell's rejected invoke carries the reason as a string (the Tauri
    // command error shape); nothing on the way up may swallow it (ADR-0030).
    vi.mocked(channel.loadTrack).mockRejectedValue('file is too large')
    await act(async () => {
      await expect(
        result.current.loadTrack(TEST_SOURCE, 'Oversize'),
      ).rejects.toBe('file is too large')
    })
    // The refused load changed nothing: the deck stays live, nothing parked.
    expect(result.current.mode).toBe('realtime')
    expect(result.current.track).toBeNull()
    expect(native.invoke).not.toHaveBeenCalledWith('deck_stop', { deck: 0 })
  })

  it('PLAY and STOP drive the track, not the worker', async () => {
    const { result, channel } = await loadedDeck()
    const sentBefore = deckInvokes().length
    await act(async () => result.current.play())
    expect(channel.playTrack).toHaveBeenCalled()
    act(() => result.current.stop())
    expect(channel.pauseTrack).toHaveBeenCalled()
    expect(deckInvokes()).toHaveLength(sentBefore)
  })

  it('CUE returns the track to the top, parked', async () => {
    const { result, channel } = await loadedDeck()
    await act(async () => result.current.prime())
    expect(channel.pauseTrack).toHaveBeenCalled()
    expect(channel.seekTrack).toHaveBeenCalledWith(0)
    expect(result.current.primed).toBe(false)
  })

  it('refuses an empty-pad capture — the worklet history holds the dead stream', async () => {
    const { result, channel } = await loadedDeck()
    act(() => result.current.toggleLoopPad(0))
    expect(channel.captureLoop).not.toHaveBeenCalled()
  })

  it('refuses a style sample for the same reason', async () => {
    const { result, channel } = await loadedDeck()
    let sample: Float32Array | null = new Float32Array(2)
    await act(async () => {
      sample = await result.current.captureStyleSample()
    })
    expect(sample).toBeNull()
    expect(channel.captureSample).not.toHaveBeenCalled()
  })

  it('drops a straggler PCM chunk while parked — the live meters never see it', async () => {
    // A parked (playback) deck must NOT feed a late model chunk into the live
    // visuals (the beat tracker moved shell-side, ADR-0025, and parks with the
    // sidecar; the loudness meter behind auto-gain is what the PCM-callback
    // guard still protects). Observable through auto-trim: a loud straggler
    // on a parked deck must leave the trim untouched, where the same feed on
    // a live deck moves it (the auto-gain suite's positive case).
    const { engine, channel } = makeFakeEngine()
    const rendered = renderDeck(engine)
    act(() => socket(0).serverOpen())
    await act(async () => {
      await rendered.result.current.loadTrack(TEST_SOURCE, 'Test Pressing')
    })
    expect(channel.loadTrack).toHaveBeenCalled()
    const trimBefore = rendered.result.current.trim.db

    const loud = new Float32Array(1920 * 2).fill(0.9)
    act(() => {
      for (let i = 0; i < 50; i++) feedPcm(loud.slice().buffer)
    })
    act(() => void vi.advanceTimersByTime(2_000))
    expect(rendered.result.current.trim.db).toBe(trimBefore)
  })

  it('follows the channel playhead while a track is loaded', async () => {
    const { result, channel } = await loadedDeck()
    vi.mocked(channel.getTrackStatus).mockReturnValue({
      position: 42.5,
      duration: 120,
      playing: true,
      ended: false,
      rate: 1,
      loop: null,
      contextTime: 100,
    })
    act(() => void vi.advanceTimersByTime(250))
    expect(result.current.track).toMatchObject({
      position: 42.5,
      playing: true,
    })
  })

  it('leavePlayback unloads the track and returns to realtime, parked staying parked', async () => {
    const { result, channel } = await loadedDeck()
    const sentBefore = deckInvokes().length
    act(() => result.current.leavePlayback())
    expect(channel.unloadTrack).toHaveBeenCalled()
    expect(result.current.mode).toBe('realtime')
    expect(result.current.track).toBeNull()
    // The track was parked, so the deck comes back stopped.
    expect(deckInvokes()).toHaveLength(sentBefore)
  })

  it('a primed deck loads a track parked — prep audio never hits the master', async () => {
    const { engine, channel } = makeFakeEngine()
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())
    await act(() => result.current.prime())
    expect(result.current.state.playing).toBe(true) // rolling, but off air

    await act(async () => {
      await result.current.loadTrack(TEST_SOURCE, 'Headphone Special')
    })
    expect(channel.playTrack).not.toHaveBeenCalled()
    expect(result.current.track).toMatchObject({ playing: false })
  })

  it('a streaming deck keeps playing through a track load', async () => {
    const { engine, channel } = makeFakeEngine()
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())
    await act(() => result.current.play())
    expect(result.current.state.playing).toBe(true)

    await act(async () => {
      await result.current.loadTrack(TEST_SOURCE, 'Hot Swap')
    })
    expect(channel.playTrack).toHaveBeenCalled()
    expect(result.current.track).toMatchObject({
      title: 'Hot Swap',
      playing: true,
    })
  })

  it('nudgeTrack seeks relative to the channel playhead, not the polled state', async () => {
    const { result, channel } = await loadedDeck()
    vi.mocked(channel.getTrackStatus).mockReturnValue({
      position: 10,
      duration: 120,
      playing: true,
      ended: false,
      rate: 1,
      loop: null,
      contextTime: 100,
    })
    act(() => result.current.nudgeTrack(2.5))
    expect(channel.seekTrack).toHaveBeenCalledWith(12.5)
  })

  it('a rolling track hands straight back to the stream on leaving', async () => {
    const { result, channel } = await loadedDeck()
    vi.mocked(channel.getTrackStatus).mockReturnValue({
      position: 10,
      duration: 120,
      playing: true,
      ended: false,
      rate: 1,
      loop: null,
      contextTime: 100,
    })
    const sentBefore = deckInvokes().length
    await act(async () => result.current.leavePlayback())
    expect(channel.unloadTrack).toHaveBeenCalled()
    expect(result.current.mode).toBe('realtime')
    expect(deckInvokes().slice(sentBefore)).toContainEqual(['deck_play', { deck: 0 }])
    expect(result.current.state.playing).toBe(true)
  })
})

describe('useDeck beat clocks (M20)', () => {
  // The gate's grace/breathing and the anchor agreement live shell-side now
  // (ADR-0025) — `analysis/beat.rs` carries those behaviours. What stays
  // webview-owned is the CLOCK: mapping the published anchor (pushed frames
  // since reset) through the published origin onto engine time, and blanking
  // on stale stats.
  const PERIOD_FRAMES = (60 / 128) * 48_000 // 22 500

  it('exposes the live beat clock at the speakers once gated and continuous', async () => {
    const { engine, captured } = makeFakeEngine()
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())
    await act(() => result.current.play())

    // The shell publishes the gated set: tempo, and an anchor on the beat
    // lattice (a whole number of periods from the stream start).
    act(() =>
      fireAnalysis({
        bpm: 128,
        confidence: 0.62,
        liveBeat: { anchorFrame: 20 * PERIOD_FRAMES, bpm: 128 },
        originFrames: 0,
      }),
    )
    expect(result.current.bpm).toBe(128)

    // No stats yet: the clock must stay blank rather than guess.
    expect(result.current.getLiveBeat()).toBeNull()

    act(() =>
      captured.onStats?.({
        underruns: 0,
        bufferedSeconds: 2,
        playing: true,
        playedFrames: 10 * 48_000,
        contextTime: 100,
      }),
    )
    const clock = result.current.getLiveBeat()
    expect(clock).not.toBeNull()
    expect(clock!.periodSeconds).toBeCloseTo(60 / 128, 2)
    // The reported beat sits on the click lattice: beats play at
    // contextTime + (k·period − played)/rate for integer k.
    const beatsFromStart = ((clock!.beatAtContext - 100) * 48_000 + 10 * 48_000) / 48_000 / (60 / 128)
    const gap = Math.abs(beatsFromStart - Math.round(beatsFromStart))
    expect(gap).toBeLessThanOrEqual(0.15)

    // Stale stats blank the clock — never a confident lie.
    act(() => void vi.advanceTimersByTime(3_000))
    expect(result.current.getLiveBeat()).toBeNull()
  })

  it('subtracts the published stream origin so the beat lands on the lattice', async () => {
    // The native engine reports a GLOBAL, never-reset render count, while the
    // shell's anchorFrame resets to 0 with the stream (ADR-0014/0025). The
    // shell captures the origin at each reset and publishes it with the
    // analysis; the clock math subtracts it so the two share a frame domain.
    // 5 s = 10.666… periods at 128 BPM — NOT a whole beat, so a dropped or
    // wrong-sign origin pushes the reported beat off the lattice.
    const ORIGIN_FRAMES = 5 * 48_000
    const { engine, captured } = makeFakeEngine()
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())
    await act(() => result.current.play())

    act(() =>
      fireAnalysis({
        bpm: 128,
        confidence: 0.62,
        liveBeat: { anchorFrame: 20 * PERIOD_FRAMES, bpm: 128 },
        originFrames: ORIGIN_FRAMES,
      }),
    )
    expect(result.current.bpm).toBe(128)

    // playedFrames is in the GLOBAL render domain: the per-stream position
    // (10 s) plus the never-reset origin.
    const perStreamPlayed = 10 * 48_000
    act(() =>
      captured.onStats?.({
        underruns: 0,
        bufferedSeconds: 2,
        playing: true,
        playedFrames: ORIGIN_FRAMES + perStreamPlayed,
        contextTime: 100,
      }),
    )
    const clock = result.current.getLiveBeat()
    expect(clock).not.toBeNull()
    expect(clock!.periodSeconds).toBeCloseTo(60 / 128, 2)
    // Reconstruct the played position in the per-stream domain (subtract the
    // origin ourselves) and assert the beat sits on the lattice. If production
    // dropped the origin subtraction, beatAtContext would be off by
    // ORIGIN_FRAMES — 10.666… periods — pushing the gap well past 0.15.
    const beatsFromStart =
      ((clock!.beatAtContext - 100) * 48_000 + perStreamPlayed) / 48_000 / (60 / 128)
    const gap = Math.abs(beatsFromStart - Math.round(beatsFromStart))
    expect(gap).toBeLessThanOrEqual(0.15)
  })

  it('derives the track beat clock from the grid, rate-aware', async () => {
    const { engine, channel } = makeFakeEngine()
    // The shell's offline verdict (ADR-0030): a refined grid, one number.
    vi.mocked(channel.loadTrack).mockResolvedValue(
      loadedTrackDto({
        duration: 24,
        bpm: 120,
        grid: { bpm: 120, firstBeatSeconds: 0 },
      }),
    )
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())
    await act(async () => {
      await result.current.loadTrack(TEST_SOURCE, 'Gridded')
    })
    const grid = result.current.track!.grid
    expect(grid).not.toBeNull()
    // One number: the readout BPM is the grid's refined verdict.
    expect(result.current.track!.bpm).toBe(grid!.bpm)

    vi.mocked(channel.getTrackStatus).mockReturnValue({
      position: 10,
      duration: 24,
      playing: true,
      ended: false,
      rate: 1.05,
      loop: null,
      contextTime: 200,
    })
    const clock = result.current.getTrackBeat()
    expect(clock).not.toBeNull()
    // Varispeed shortens the beat period in context time.
    expect(clock!.periodSeconds).toBeCloseTo(60 / grid!.bpm / 1.05, 4)
    const periodTrack = 60 / grid!.bpm
    const phase =
      ((((10 - grid!.firstBeatSeconds) / periodTrack) % 1) + 1) % 1
    expect(clock!.beatAtContext).toBeCloseTo(200 - phase * clock!.periodSeconds, 4)
  })

  it('SYNC matches tempo within the envelope and refuses outside it', async () => {
    const { engine, channel } = makeFakeEngine()
    vi.mocked(channel.loadTrack).mockResolvedValue(
      loadedTrackDto({
        duration: 24,
        bpm: 120,
        grid: { bpm: 120, firstBeatSeconds: 0 },
      }),
    )
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())
    await act(async () => {
      await result.current.loadTrack(TEST_SOURCE, 'Syncable')
    })
    const bpm = result.current.track!.bpm!

    let synced = ''
    act(() => {
      synced = result.current.syncTrack(bpm * 1.05)
    })
    expect(synced).toBe('synced')
    // The rate crosses to the shell, which also recomputes the synced echo's
    // clock from the load-time analysis (ADR-0030).
    expect(channel.setTrackRate).toHaveBeenCalledWith(expect.closeTo(1.05, 5))
    expect(result.current.track!.rate).toBeCloseTo(1.05, 5)

    act(() => {
      synced = result.current.syncTrack(bpm * 1.2)
    })
    expect(synced).toBe('out_of_range')
    act(() => {
      synced = result.current.syncTrack(null)
    })
    expect(synced).toBe('no_tempo')
  })

  // ── Hot cues and track loops (M21, ADR-0015) ─────────────────────

  async function griddedDeck(position: number) {
    const { engine, channel } = makeFakeEngine()
    vi.mocked(channel.loadTrack).mockResolvedValue(
      loadedTrackDto({
        duration: 24,
        bpm: 120,
        grid: { bpm: 120, firstBeatSeconds: 0 },
      }),
    )
    // The engine mock holds a loop like the real boundary would, so
    // the hook's mirror-from-the-engine path is what's under test.
    let engineLoop: { start: number; end: number } | null = null
    vi.mocked(channel.setTrackLoop).mockImplementation((start, end) => {
      engineLoop = { start, end }
    })
    vi.mocked(channel.clearTrackLoop).mockImplementation(() => {
      engineLoop = null
    })
    // The real boundary exits the loop on any seek (ADR-0015).
    vi.mocked(channel.seekTrack).mockImplementation(() => {
      engineLoop = null
    })
    const status = { position }
    vi.mocked(channel.getTrackStatus).mockImplementation(() => ({
      position: status.position,
      duration: 24,
      playing: true,
      ended: false,
      rate: 1,
      loop: engineLoop,
      contextTime: 200,
    }))
    const rendered = renderDeck(engine)
    act(() => socket(0).serverOpen())
    await act(async () => {
      await rendered.result.current.loadTrack(TEST_SOURCE, 'Cueable')
    })
    expect(rendered.result.current.track!.grid).not.toBeNull()
    return { ...rendered, channel, status }
  }

  it('an empty hot cue pad captures the playhead on the grid; a filled one jumps', async () => {
    const { result, channel, status } = await griddedDeck(10.1)
    const grid = result.current.track!.grid!
    const period = 60 / grid.bpm

    act(() => result.current.hotCuePad(2))
    const cue = result.current.track!.cues[2]!
    // Snapped onto the lattice, within half a beat of the press.
    const phase = ((cue - grid.firstBeatSeconds) / period) % 1
    expect(Math.min(phase, 1 - phase)).toBeLessThan(1e-6)
    expect(Math.abs(cue - 10.1)).toBeLessThanOrEqual(period / 2)

    status.position = 20
    act(() => result.current.hotCuePad(2))
    expect(channel.seekTrack).toHaveBeenCalledWith(cue)
    // The jump must not overwrite the slot.
    expect(result.current.track!.cues[2]).toBe(cue)

    act(() => result.current.clearHotCue(2))
    expect(result.current.track!.cues[2]).toBeNull()
  })

  it('the zoom source carries filled hot cues in the playhead’s hop domain (M21)', async () => {
    const { result } = await griddedDeck(10.1)
    // Nothing captured yet: the close-up has no cues to draw.
    expect(result.current.getZoomSource()!.cues).toEqual([])

    act(() => result.current.hotCuePad(2))
    const cue = result.current.track!.cues[2]!
    const source = result.current.getZoomSource()!
    // Only the one filled slot, and in the same hop units as the
    // playhead — so the strip lines the marker up against the centre.
    expect(source.cues).toHaveLength(1)
    expect(source.cues[0] / source.playheadHop).toBeCloseTo(cue / 10.1, 6)
  })

  it('cue capture runs free without a grid — no fabricated lattice', async () => {
    const { engine, channel } = makeFakeEngine()
    vi.mocked(channel.getTrackStatus).mockReturnValue({
      position: 10.1,
      duration: 120,
      playing: true,
      ended: false,
      rate: 1,
      loop: null,
      contextTime: 200,
    })
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())
    await act(async () => {
      await result.current.loadTrack(TEST_SOURCE, 'Gridless')
    })
    expect(result.current.track!.grid).toBeNull()
    act(() => result.current.hotCuePad(0))
    expect(result.current.track!.cues[0]).toBe(10.1)
  })

  it('IN and OUT close a whole-beat loop on the engine; EXIT releases it', async () => {
    const { result, channel, status } = await griddedDeck(8.1)
    const period = 60 / result.current.track!.grid!.bpm

    act(() => result.current.loopIn())
    const armed = result.current.track!.pendingLoopIn!
    expect(Math.abs(armed - 8.1)).toBeLessThanOrEqual(period / 2)

    status.position = 10.2
    act(() => result.current.loopOut())
    expect(channel.setTrackLoop).toHaveBeenCalled()
    const loop = result.current.track!.loop!
    expect(loop.start).toBe(armed)
    // A whole number of beats — 4 at 120 BPM across ~2.1s.
    const beats = (loop.end - loop.start) / period
    expect(Math.abs(beats - Math.round(beats))).toBeLessThan(1e-6)
    expect(Math.round(beats)).toBe(4)
    expect(result.current.track!.pendingLoopIn).toBeNull()

    act(() => result.current.loopExit())
    expect(channel.clearTrackLoop).toHaveBeenCalled()
    expect(result.current.track!.loop).toBeNull()
  })

  it('a beat loop sets N beats; halve and double scale it, anchored on the IN (M23)', async () => {
    const { result, channel } = await griddedDeck(8.1)
    const period = 60 / result.current.track!.grid!.bpm

    act(() => result.current.beatLoop(4))
    const set = result.current.track!.loop!
    expect((set.end - set.start) / period).toBeCloseTo(4)
    expect(channel.setTrackLoop).toHaveBeenLastCalledWith(set.start, set.end)

    act(() => result.current.halveLoop())
    const halved = result.current.track!.loop!
    expect(halved.start).toBe(set.start) // the IN holds
    expect((halved.end - halved.start) / period).toBeCloseTo(2)

    act(() => result.current.doubleLoop())
    const doubled = result.current.track!.loop!
    expect(doubled.start).toBe(set.start)
    expect((doubled.end - doubled.start) / period).toBeCloseTo(4)
  })

  it('halve and double are no-ops with no active loop (M23)', async () => {
    const { result, channel } = await griddedDeck(8.1)
    act(() => result.current.halveLoop())
    act(() => result.current.doubleLoop())
    expect(channel.setTrackLoop).not.toHaveBeenCalled()
    expect(result.current.track!.loop).toBeNull()
  })

  it('a beat loop is a no-op without a confident grid — inert, not guessed (M23)', async () => {
    const { engine, channel } = makeFakeEngine()
    vi.mocked(channel.getTrackStatus).mockReturnValue({
      position: 10.1,
      duration: 120,
      playing: true,
      ended: false,
      rate: 1,
      loop: null,
      contextTime: 200,
    })
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())
    await act(async () => {
      await result.current.loadTrack(TEST_SOURCE, 'Gridless')
    })
    expect(result.current.track!.grid).toBeNull()
    act(() => result.current.beatLoop(4))
    expect(channel.setTrackLoop).not.toHaveBeenCalled()
    expect(result.current.track!.loop).toBeNull()
  })

  it('the zoom source carries the active loop region in hop units (M21)', async () => {
    const { result, status } = await griddedDeck(8.1)
    // Not looping yet: the close-up has no region to wash.
    expect(result.current.getZoomSource()!.loop).toBeNull()

    act(() => result.current.loopIn())
    status.position = 10.2
    act(() => result.current.loopOut())
    const loop = result.current.track!.loop!
    const source = result.current.getZoomSource()!
    // The region in the playhead's hop units, so the wash and the
    // entry/exit caps land where the audio actually wraps.
    expect(source.loop).not.toBeNull()
    expect(source.loop!.startHop / source.playheadHop).toBeCloseTo(
      loop.start / 10.2,
      6,
    )
    expect(source.loop!.endHop / source.playheadHop).toBeCloseTo(
      loop.end / 10.2,
      6,
    )

    act(() => result.current.loopExit())
    expect(result.current.getZoomSource()!.loop).toBeNull()
  })

  it('OUT with no IN armed is a no-op, and a seek drops loop and pending IN', async () => {
    const { result, channel, status } = await griddedDeck(8.1)
    act(() => result.current.loopOut())
    expect(channel.setTrackLoop).not.toHaveBeenCalled()

    act(() => result.current.loopIn())
    status.position = 10.2
    act(() => result.current.loopOut())
    expect(result.current.track!.loop).not.toBeNull()

    // The engine clears its loop on seek (ADR-0015, mirrored by the
    // helper's mock); the hook must follow, not show a ghost region.
    act(() => result.current.loopIn())
    act(() => result.current.seekTrack(2))
    expect(result.current.track!.loop).toBeNull()
    expect(result.current.track!.pendingLoopIn).toBeNull()
  })

  it('transport CUE drops the loop everywhere — engine, mirror, pending IN', async () => {
    // CUE's back-to-top is a seek (ADR-0013 + ADR-0015): the loop
    // must not survive on screen while playback runs linear.
    const { result, status } = await griddedDeck(8.1)
    act(() => result.current.loopIn())
    status.position = 10.2
    act(() => result.current.loopOut())
    expect(result.current.track!.loop).not.toBeNull()

    act(() => result.current.loopIn())
    await act(() => result.current.prime())
    expect(result.current.track!.loop).toBeNull()
    expect(result.current.track!.pendingLoopIn).toBeNull()
  })

  it('the position poll mirrors an engine-side loop drop within a tick', async () => {
    const { result, channel, status } = await griddedDeck(8.1)
    act(() => result.current.loopIn())
    status.position = 10.2
    act(() => result.current.loopOut())
    expect(result.current.track!.loop).not.toBeNull()

    // The engine drops the loop behind the hook's back (any internal
    // seek path); the 250 ms poll must catch up without help.
    vi.mocked(channel.clearTrackLoop).getMockImplementation()?.()
    act(() => void vi.advanceTimersByTime(250))
    expect(result.current.track!.loop).toBeNull()
  })
})

describe('useDeck mixer projection (ADR-0020 phase C)', () => {
  // The shell hydrates engine + store BEFORE the webview exists, so every
  // snapshot is authoritative — the per-field synced gates are gone with the
  // localStorage boot replay they fenced.
  it('adopts any differing store value — first snapshot or later external move', () => {
    const { result } = renderDeck(makeFakeEngine().engine)
    expect(result.current.volume).toBe(0.8)

    // The first snapshot (hydration) adopts directly, no echo handshake…
    act(() => fireStore({ volume: 0.55 }))
    expect(result.current.volume).toBe(0.55)
    // …and so does a later external move (MIDI / an MCP agent).
    act(() => fireStore({ volume: 0.3 }))
    expect(result.current.volume).toBe(0.3)
  })

  it('a local gesture is not fought by its own store echo', () => {
    const { result } = renderDeck(makeFakeEngine().engine)
    act(() => {
      result.current.setFx('filter')
    })
    // The channel echoed the write through the harness store (like the real
    // set_fx command); the adoption saw its own value and left it alone.
    expect(result.current.fx).toEqual({ kind: 'filter', amount: 0.5 })

    // Unrelated churn (an analysis tick) carries the CURRENT mixer — never
    // a stale one — so the FX survives it.
    act(() => fireStore({ playing: true }))
    expect(result.current.fx.kind).toBe('filter')
  })

  it('a 14-bit-quantized hardware echo is not mistaken for an external move', () => {
    const { result } = renderDeck(makeFakeEngine().engine)
    act(() => {
      result.current.setFx('filter')
    })
    expect(result.current.fx).toEqual({ kind: 'filter', amount: 0.5 })

    // The FLX4 position-sync echoes our amount quantised to a 14-bit centre
    // detent (0.5 → 0.5000305): inside the epsilon, so it must not jitter
    // the knob back…
    act(() => fireStore({ fx: { kind: 'filter', amount: 0.5000305 } }))
    expect(result.current.fx.amount).toBe(0.5)

    // …while a genuinely different amount is an external move and adopts.
    act(() => fireStore({ fx: { kind: 'filter', amount: 0.9 } }))
    expect(result.current.fx.amount).toBe(0.9)
  })
})

describe('useDeck realtime mirror + transport projection (ADR-0020)', () => {
  it('mirrors the worker-reported model up via set_deck_model', () => {
    renderDeck(makeFakeEngine().engine)
    act(() => socket(0).serverOpen())
    act(() => socket(0).serverEvent({ event: 'ready', deck: 'a', model: 'mrt2_base' }))
    const calls = native.invoke.mock.calls.filter(([cmd]) => cmd === 'set_deck_model')
    expect(calls.at(-1)?.[1]).toEqual({ deck: 0, model: 'mrt2_base' })
  })

  it('lights the transport from the store snapshot, never from local intent', async () => {
    const { result } = renderDeck(makeFakeEngine().engine)
    act(() => socket(0).serverOpen())
    // Sever the harness echo: deck_play reaches Rust, no snapshot has landed yet.
    native.invoke.mockImplementation((cmd: string) =>
      cmd === 'app_info'
        ? Promise.resolve({ generationPort: null })
        : Promise.resolve(undefined),
    )

    await act(() => result.current.play())
    // Pure projection: play() emits the intent but no longer flips the reducer.
    expect(deckInvokes()).toEqual([['deck_play', { deck: 0 }]])
    expect(result.current.state.playing).toBe(false)

    // The store round-trip lands → the button lights.
    act(() => fireStore({ playing: true }))
    expect(result.current.state.playing).toBe(true)
  })

  it('adopts an agent play from the store and readies the deck channel', async () => {
    const { engine } = makeFakeEngine()
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())

    await act(async () => fireStore({ playing: true }))

    expect(result.current.state.playing).toBe(true)
    // The channel exists too, so the meters populate for an agent-started deck.
    expect(engine.createDeckChannel).toHaveBeenCalled()
  })

  it('adopts an agent stop from the store', async () => {
    const { result } = renderDeck(makeFakeEngine().engine)
    act(() => socket(0).serverOpen())
    await act(() => result.current.play())
    expect(result.current.state.playing).toBe(true)

    act(() => fireStore({ playing: false }))
    expect(result.current.state.playing).toBe(false)
  })

  it('unrelated store churn leaves the transport alone', async () => {
    const { result } = renderDeck(makeFakeEngine().engine)
    act(() => socket(0).serverOpen())
    await act(() => result.current.play())

    // A snapshot fired for another control (a mixer echo, the other deck's
    // transport mirror) carries the store's CURRENT playing — the deck stays lit.
    act(() => fireStore({ volume: 0.3 }))
    expect(result.current.state.playing).toBe(true)
  })

  it('a second play tap racing the round-trip is safe — the shell dedups it', async () => {
    const { engine } = makeFakeEngine()
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())
    // Sever the echo: the first tap's round-trip has not landed yet.
    native.invoke.mockImplementation((cmd: string) =>
      cmd === 'app_info'
        ? Promise.resolve({ generationPort: null })
        : Promise.resolve(undefined),
    )

    await act(() => result.current.play())
    await act(() => result.current.play())

    // Both taps send the intent (there is no webview in-flight guard any
    // more, phase D): the pre-send work is idempotent — channel.reset is a
    // native no-op — and Rust deck_play's atomic start_transport makes the
    // second intent a shell-side no-op (store.rs pins that).
    expect(deckInvokes()).toEqual([
      ['deck_play', { deck: 0 }],
      ['deck_play', { deck: 0 }],
    ])

    // The snapshot lands: the transport lights through the projection.
    act(() => fireStore({ playing: true }))
    expect(result.current.state.playing).toBe(true)
  })

  it('re-priming an already-playing deck never wedges the transport guard', async () => {
    const { engine, channel } = makeFakeEngine()
    const { result } = renderDeck(engine)
    act(() => socket(0).serverOpen())

    // The routine set: CUE (prime) → PLAY (drop on air) → CUE → PLAY → CUE.
    // Re-primes #2 and #3 round-trip as store no-ops (playing already true) —
    // an intent-armed guard with a transition-gated clear wedged on them.
    await act(() => result.current.prime())
    await act(() => result.current.play())
    await act(() => result.current.prime())
    await act(() => result.current.play())
    await act(() => result.current.prime())

    expect(result.current.primed).toBe(true)
    // Every prime parked the deck off air — none was silently swallowed.
    const parks = vi.mocked(channel.setOnAir).mock.calls.filter(([on]) => !on)
    expect(parks.length).toBe(3)
  })
})
