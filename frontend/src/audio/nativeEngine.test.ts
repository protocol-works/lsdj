import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { SAMPLE_RATE } from './types'
import {
  createNativeEngine,
  fetchGenerationApi,
  styleAddTarget,
  styleMoveTarget,
  styleSetCursor,
} from './nativeEngine'

// A controllable __TAURI__ global: records every invoke and serves a test
// snapshot for `engine_snapshot`. rAF is stubbed so the poller can be flushed
// deterministically.

type InvokeCall = { cmd: string; args: unknown }

let calls: InvokeCall[]
let snapshot: unknown
let rafQueue: FrameRequestCallback[]

function flushRaf() {
  const due = rafQueue
  rafQueue = []
  for (const cb of due) cb(performance.now())
}

/** Run the poller a few times so a cached snapshot is in place. */
async function settle() {
  for (let i = 0; i < 3; i++) {
    flushRaf()
    await Promise.resolve()
    await Promise.resolve()
  }
}

beforeEach(() => {
  calls = []
  snapshot = null
  rafQueue = []
  vi.stubGlobal('requestAnimationFrame', (cb: FrameRequestCallback) => {
    rafQueue.push(cb)
    return rafQueue.length
  })
  const invoke = vi.fn((cmd: string, args?: unknown) => {
    calls.push({ cmd, args })
    if (cmd === 'engine_snapshot') return Promise.resolve(snapshot)
    if (cmd === 'start_recording') return Promise.resolve('/Downloads/lsdj-take.wav') // path opened at start
    return Promise.resolve(undefined)
  })
  vi.stubGlobal('__TAURI__', { core: { invoke } })
})

afterEach(() => {
  vi.unstubAllGlobals()
})

const SNAP = {
  health: {
    outputRingFrames: 3600,
    deckRingFrames: [48000, 0],
    deckUnderruns: 0,
    outputUnderruns: 0,
    masterPeak: 0.42,
    masterGainReductionDb: -1.5,
    deckLevels: [0.3, 0.0],
    contextFrames: SAMPLE_RATE * 2, // 2 s of clock
  },
  tracks: [
    {
      playhead: SAMPLE_RATE * 10, // 10 s in
      playing: true,
      durationFrames: SAMPLE_RATE * 100, // 100 s long
      rate: 1.0,
      ended: false,
      loopRegion: { start: SAMPLE_RATE * 4, end: SAMPLE_RATE * 8 },
    },
    null,
  ],
  loops: [
    [
      { filled: true, playing: false },
      { filled: false, playing: false },
    ],
    [{ filled: false, playing: false }],
  ],
}

describe('createNativeEngine — control contract', () => {
  it('authenticates generation requests with the in-memory launch capability', async () => {
    const invoke = vi.fn((cmd: string) =>
      cmd === 'app_info'
        ? Promise.resolve({
            generationPort: 4321,
            generationCapability: 'b'.repeat(64),
          })
        : Promise.resolve(undefined),
    )
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    const fetchMock = vi.fn(async (_url: string, _init: RequestInit) => {
      void _url
      void _init
      return { ok: true }
    })
    vi.stubGlobal('fetch', fetchMock)

    await fetchGenerationApi('/api/models')

    expect(fetchMock).toHaveBeenCalledTimes(1)
    const [url, init] = fetchMock.mock.calls[0]
    expect(url).toBe('http://127.0.0.1:4321/api/models')
    expect((init.headers as Headers).get('x-lsdj-capability')).toBe('b'.repeat(64))
  })

  it('routes Magenta requests to the distinct native gateway', async () => {
    const invoke = vi.fn((cmd: string) =>
      cmd === 'app_info'
        ? Promise.resolve({
            generationPort: 4321,
            generationCapability: 's'.repeat(64),
            magentaPort: 9876,
            magentaCapability: 'm'.repeat(64),
          })
        : Promise.resolve(undefined),
    )
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    const fetchMock = vi.fn(async (_url: string, _init: RequestInit) => {
      void _url
      void _init
      return { ok: true }
    })
    vi.stubGlobal('fetch', fetchMock)

    await fetchGenerationApi('/api/render', { method: 'POST' })
    await fetchGenerationApi('/api/generate', { method: 'POST' })

    const [renderUrl, renderInit] = fetchMock.mock.calls[0]
    expect(renderUrl).toBe('http://127.0.0.1:9876/api/render')
    expect((renderInit.headers as Headers).get('x-lsdj-capability')).toBe('m'.repeat(64))
    const [generateUrl, generateInit] = fetchMock.mock.calls[1]
    expect(generateUrl).toBe('http://127.0.0.1:4321/api/generate')
    expect((generateInit.headers as Headers).get('x-lsdj-capability')).toBe('s'.repeat(64))
  })

  it('fails closed when a managed Magenta gateway could not bind', async () => {
    const invoke = vi.fn((cmd: string) =>
      cmd === 'app_info'
        ? Promise.resolve({
            generationPort: 4321,
            generationCapability: 's'.repeat(64),
            magentaPort: null,
            magentaCapability: null,
          })
        : Promise.resolve(undefined),
    )
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    const fetchMock = vi.fn(async (_url: string, _init: RequestInit) => {
      void _url
      void _init
      return { ok: true }
    })
    vi.stubGlobal('fetch', fetchMock)

    await expect(fetchGenerationApi('/api/render', { method: 'POST' })).rejects.toThrow(
      'authentication is unavailable',
    )
    expect(fetchMock).not.toHaveBeenCalled()
  })

  it('refreshes app_info when a first SA3 install starts the service', async () => {
    let appInfoCalls = 0
    const invoke = vi.fn((cmd: string) => {
      if (cmd !== 'app_info') return Promise.resolve(undefined)
      appInfoCalls += 1
      return Promise.resolve(
        appInfoCalls === 1
          ? { generationPort: null, generationCapability: null }
          : { generationPort: 2468, generationCapability: 'n'.repeat(64) },
      )
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    const fetchMock = vi.fn(async (_url: string, _init: RequestInit) => {
      void _url
      void _init
      return { ok: true }
    })
    vi.stubGlobal('fetch', fetchMock)

    await fetchGenerationApi('/api/generate', { method: 'POST' })

    expect(appInfoCalls).toBe(2)
    const [url, init] = fetchMock.mock.calls[0]
    expect(url).toBe('http://127.0.0.1:2468/api/generate')
    expect((init.headers as Headers).get('x-lsdj-capability')).toBe('n'.repeat(64))
  })

  it('createDeckChannel replays NO mixer config — the shell hydrates (phase C)', async () => {
    const engine = createNativeEngine()
    await engine.createDeckChannel(
      'b',
      { volume: 0.8, eq: { low: 0.5, mid: 0.5, high: 0.5 }, cue: false, fx: { kind: null, amount: 0 }, trimDb: 3 },
      () => {},
    )
    // A replay here could overwrite the shell-hydrated values with the
    // webview's pre-snapshot defaults (an agent-started deck racing boot).
    const cmds = calls.map((c) => c.cmd)
    for (const cmd of ['set_volume', 'set_eq', 'clear_fx', 'set_fx', 'set_trim', 'set_cue']) {
      expect(cmds).not.toContain(cmd)
    }
  })

  it('maps deck ids and FX kinds (snake→camel) and routes null to clear_fx', async () => {
    const engine = createNativeEngine()
    const ch = await engine.createDeckChannel(
      'a',
      { volume: 1, eq: { low: 0.5, mid: 0.5, high: 0.5 }, cue: false, fx: { kind: null, amount: 0 }, trimDb: 0 },
      () => {},
    )
    calls.length = 0
    ch.setFx('dub_echo')
    ch.setFxAmount(0.7)
    ch.setOnAir(false)
    ch.setEq('high', 0.9)
    flushRaf() // ship the coalesced set_fx_amount / set_eq writes
    expect(calls).toContainEqual({ cmd: 'set_fx', args: { deck: 0, kind: 'dubEcho' } })
    expect(calls).toContainEqual({ cmd: 'set_fx_amount', args: { deck: 0, amount: 0.7 } })
    expect(calls).toContainEqual({ cmd: 'set_on_air', args: { deck: 0, on: false } })
    expect(calls).toContainEqual({ cmd: 'set_eq', args: { deck: 0, band: 'high', value: 0.9 } })

    calls.length = 0
    ch.setFx(null)
    expect(calls).toContainEqual({ cmd: 'clear_fx', args: { deck: 0 } })
  })

  it('converts transport units (seconds↔frames) at the boundary', async () => {
    const engine = createNativeEngine()
    const ch = await engine.createDeckChannel(
      'a',
      { volume: 1, eq: { low: 0.5, mid: 0.5, high: 0.5 }, cue: false, fx: { kind: null, amount: 0 }, trimDb: 0 },
      () => {},
    )
    calls.length = 0
    ch.seekTrack(2)
    ch.setTrackLoop(1, 1.5)
    ch.nudgeTrackPhase(0.01) // the jog-while-playing platter bend
    expect(calls).toContainEqual({ cmd: 'seek_track', args: { deck: 0, frames: 2 * SAMPLE_RATE } })
    expect(calls).toContainEqual({
      cmd: 'set_track_loop',
      args: { deck: 0, start: 1 * SAMPLE_RATE, end: Math.round(1.5 * SAMPLE_RATE) },
    })
    expect(calls).toContainEqual({ cmd: 'nudge_track_phase', args: { deck: 0, frames: 0.01 * SAMPLE_RATE } })
  })

  it('setCrossfade goes to the engine', async () => {
    const engine = createNativeEngine()
    await engine.createDeckChannel(
      'a',
      { volume: 1, eq: { low: 0.5, mid: 0.5, high: 0.5 }, cue: false, fx: { kind: null, amount: 0 }, trimDb: 0 },
      () => {},
    )
    calls.length = 0
    engine.setCrossfade(0.25)
    flushRaf() // set_crossfade is coalesced per frame
    expect(calls).toContainEqual({ cmd: 'set_crossfade', args: { position: 0.25 } })
  })
})

describe('createNativeEngine — per-frame IPC coalescing', () => {
  // A continuous setter on one target, swept many times within a single frame,
  // must collapse to one invoke carrying the latest value; discrete commands stay
  // immediate and flush pending writes first so they can never be leapfrogged.
  async function deckA() {
    const engine = createNativeEngine()
    const ch = await engine.createDeckChannel(
      'a',
      { volume: 1, eq: { low: 0.5, mid: 0.5, high: 0.5 }, cue: false, fx: { kind: null, amount: 0 }, trimDb: 0 },
      () => {},
    )
    calls.length = 0
    return { engine, ch }
  }

  it('collapses a same-band setEq sweep to one invoke with the latest value', async () => {
    const { ch } = await deckA()
    ch.setEq('low', 0.1)
    ch.setEq('low', 0.2)
    ch.setEq('low', 0.9)
    // Nothing has shipped yet — the writes are pending the frame.
    expect(calls.filter((c) => c.cmd === 'set_eq')).toHaveLength(0)
    flushRaf()
    const eqCalls = calls.filter((c) => c.cmd === 'set_eq')
    expect(eqCalls).toHaveLength(1)
    expect(eqCalls[0].args).toEqual({ deck: 0, band: 'low', value: 0.9 })
  })

  it('coalesces different bands independently — one invoke each', async () => {
    const { ch } = await deckA()
    ch.setEq('low', 0.2)
    ch.setEq('mid', 0.3)
    ch.setEq('low', 0.4)
    ch.setEq('high', 0.6)
    flushRaf()
    const eqCalls = calls.filter((c) => c.cmd === 'set_eq')
    expect(eqCalls).toHaveLength(3)
    expect(eqCalls).toContainEqual({ cmd: 'set_eq', args: { deck: 0, band: 'low', value: 0.4 } })
    expect(eqCalls).toContainEqual({ cmd: 'set_eq', args: { deck: 0, band: 'mid', value: 0.3 } })
    expect(eqCalls).toContainEqual({ cmd: 'set_eq', args: { deck: 0, band: 'high', value: 0.6 } })
  })

  it('a discrete command flushes pending coalesced writes FIRST, then sends itself', async () => {
    const { ch } = await deckA()
    ch.setFxAmount(0.5) // coalesced, pending
    ch.setFx('dub_echo') // discrete — must flush set_fx_amount before it lands
    const cmds = calls.map((c) => c.cmd)
    expect(cmds).toEqual(['set_fx_amount', 'set_fx'])
    expect(calls[0].args).toEqual({ deck: 0, amount: 0.5 })
    expect(calls[1].args).toEqual({ deck: 0, kind: 'dubEcho' })
  })

  it('a seek flushes pending coalesced writes FIRST, then sends itself', async () => {
    const { ch } = await deckA()
    ch.setVolume(0.7) // coalesced, pending
    ch.seekTrack(2) // discrete
    const cmds = calls.map((c) => c.cmd)
    expect(cmds).toEqual(['set_volume', 'seek_track'])
    expect(calls[0].args).toEqual({ deck: 0, gain: 0.7 })
    expect(calls[1].args).toEqual({ deck: 0, frames: 2 * SAMPLE_RATE })
  })

  it('drops a coalesced re-send of the already-shipped value but ships a distinct one', async () => {
    const { ch } = await deckA()
    ch.setVolume(0.5)
    flushRaf() // ships 0.5
    calls.length = 0
    ch.setVolume(0.5) // identical to the live value → dropped
    flushRaf()
    expect(calls.filter((c) => c.cmd === 'set_volume')).toHaveLength(0)
    ch.setVolume(0.8) // distinct → ships
    flushRaf()
    const volCalls = calls.filter((c) => c.cmd === 'set_volume')
    expect(volCalls).toHaveLength(1)
    expect(volCalls[0].args).toEqual({ deck: 0, gain: 0.8 })
  })

  it('discrete commands are never coalesced — each fires immediately', async () => {
    const { ch } = await deckA()
    ch.setOnAir(false)
    ch.setOnAir(true)
    ch.setCue(true)
    // No frame flush — they must already be on the wire.
    expect(calls).toEqual([
      { cmd: 'set_on_air', args: { deck: 0, on: false } },
      { cmd: 'set_on_air', args: { deck: 0, on: true } },
      { cmd: 'set_cue', args: { deck: 0, on: true } },
    ])
  })
})

describe('createNativeEngine — snapshot-backed getters', () => {
  it('serves synchronous getters from the cached snapshot', async () => {
    snapshot = SNAP
    const engine = createNativeEngine()
    const ch = await engine.createDeckChannel(
      'a',
      { volume: 1, eq: { low: 0.5, mid: 0.5, high: 0.5 }, cue: false, fx: { kind: null, amount: 0 }, trimDb: 0 },
      () => {},
    )
    await settle()

    expect(engine.getMasterLevel()).toBeCloseTo(0.42)
    expect(engine.getMasterGainReduction()).toBeCloseTo(-1.5)
    expect(engine.getContextTime()).toBeCloseTo(2)
    expect(ch.getLevel()).toBeCloseTo(0.3)

    const status = ch.getTrackStatus()
    expect(status).not.toBeNull()
    expect(status!.position).toBeCloseTo(10)
    expect(status!.duration).toBeCloseTo(100)
    expect(status!.playing).toBe(true)
    expect(status!.loop).toEqual({ start: 4, end: 8 })
    expect(status!.contextTime).toBeCloseTo(2)
  })

  it('playLoop reports the cached filled state and only fires when filled', async () => {
    snapshot = SNAP
    const engine = createNativeEngine()
    const ch = await engine.createDeckChannel(
      'a',
      { volume: 1, eq: { low: 0.5, mid: 0.5, high: 0.5 }, cue: false, fx: { kind: null, amount: 0 }, trimDb: 0 },
      () => {},
    )
    await settle()
    calls.length = 0

    expect(ch.playLoop(0, false)).toBe(true) // slot 0 filled
    expect(calls).toContainEqual({
      cmd: 'play_loop',
      args: { deck: 0, slot: 0, layer: false },
    })

    calls.length = 0
    expect(ch.playLoop(1, true)).toBe(false) // slot 1 empty
    expect(calls.find((c) => c.cmd === 'play_loop')).toBeUndefined()
  })

  it('drives the per-deck stats handler from the snapshot', async () => {
    snapshot = SNAP
    const engine = createNativeEngine()
    const stats = vi.fn()
    await engine.createDeckChannel(
      'a',
      { volume: 1, eq: { low: 0.5, mid: 0.5, high: 0.5 }, cue: false, fx: { kind: null, amount: 0 }, trimDb: 0 },
      stats,
    )
    await settle()
    expect(stats).toHaveBeenCalled()
    const last = stats.mock.calls.at(-1)![0]
    expect(last.bufferedSeconds).toBeCloseTo(1) // 48000 frames / SR
    expect(last.contextTime).toBeCloseTo(2)
    expect(last.playing).toBe(true)
  })
})

describe('createNativeEngine — graceful native stubs', () => {
  it('recording drives the engine commands and returns the path opened at start', async () => {
    const engine = createNativeEngine()
    // The file opens at start (the take streams to disk), so the folder + stem go
    // out with start_recording, which returns the path; stop just closes it.
    const path = await engine.startRecording('/Sets', 'lsdj-take')
    await engine.stopRecording()
    const cmds = calls.map((c) => c.cmd)
    expect(calls).toContainEqual({
      cmd: 'start_recording',
      args: { folder: '/Sets', name: 'lsdj-take' },
    })
    expect(cmds).toContain('stop_recording')
    expect(path).toBe('/Downloads/lsdj-take.wav')
  })

  it('resume resolves without throwing and cue-mix goes to the engine', async () => {
    const engine = createNativeEngine()
    await expect(engine.resume()).resolves.toBeUndefined()
    calls.length = 0
    engine.setCueMix(0.3)
    flushRaf() // set_cue_mix is coalesced per frame
    expect(calls).toContainEqual({ cmd: 'set_cue_mix', args: { position: 0.3 } })
  })
})

describe('createNativeEngine — output device', () => {
  it('listOutputDevices invokes list_output_devices and returns the array', async () => {
    const devices = [
      { name: 'Built-in', channels: 2, cueCapable: false },
      { name: 'FLX4', channels: 4, cueCapable: true },
    ]
    // Serve the device list for the list command (other commands keep defaults).
    const invoke = vi.fn((cmd: string, args?: unknown) => {
      calls.push({ cmd, args })
      if (cmd === 'list_output_devices') return Promise.resolve(devices)
      return Promise.resolve(undefined)
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })

    const engine = createNativeEngine()
    const result = await engine.listOutputDevices()
    expect(result).toEqual(devices)
    expect(calls).toContainEqual({ cmd: 'list_output_devices', args: undefined })
  })

  it('setMainDevice invokes set_main_device with the name', async () => {
    const engine = createNativeEngine()
    await engine.setMainDevice('FLX4')
    expect(calls).toContainEqual({ cmd: 'set_main_device', args: { name: 'FLX4' } })
  })

  it('setCueDevice invokes set_cue_device with the name', async () => {
    const engine = createNativeEngine()
    await engine.setCueDevice('Built-in')
    expect(calls).toContainEqual({ cmd: 'set_cue_device', args: { name: 'Built-in' } })
  })

  it('a device switch propagates a rejection so the caller can catch it', async () => {
    const invoke = vi.fn((cmd: string, args?: unknown) => {
      calls.push({ cmd, args })
      if (cmd === 'set_main_device') return Promise.reject(new Error('device busy'))
      return Promise.resolve(undefined)
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })

    const engine = createNativeEngine()
    await expect(engine.setMainDevice('FLX4')).rejects.toThrow('device busy')
  })
})

// The style intents (ADR-0020 phase B): continuous gestures (cursor, target
// drags) coalesce to the latest value per key in a frame — a pad drag fires
// per pointermove and each store mutation broadcasts a full snapshot — while
// discrete intents flush the pending map first, so a stale queued move can
// never land after (and displace) an add or a fan-out.
describe('the style intents', () => {
  it('coalesces a cursor drag to the latest value in the frame', () => {
    styleSetCursor(0, 0.1, 0.1)
    styleSetCursor(0, 0.2, 0.9)
    // Nothing crosses until the animation frame.
    expect(calls.filter((c) => c.cmd === 'style_set_cursor')).toHaveLength(0)
    flushRaf()
    const shipped = calls.filter((c) => c.cmd === 'style_set_cursor')
    expect(shipped).toHaveLength(1)
    expect(shipped[0].args).toEqual({ deck: 0, x: 0.2, y: 0.9 })
  })

  it('keys target moves per target so a two-dot jog reel ships both', () => {
    styleMoveTarget(0, 'dub techno', 0.2, 0.2)
    styleMoveTarget(0, 'acid', 0.8, 0.8)
    styleMoveTarget(0, 'dub techno', 0.3, 0.3)
    flushRaf()
    const shipped = calls.filter((c) => c.cmd === 'style_move_target')
    expect(shipped).toHaveLength(2)
    expect(shipped[0].args).toEqual({ deck: 0, text: 'dub techno', x: 0.3, y: 0.3 })
    expect(shipped[1].args).toEqual({ deck: 0, text: 'acid', x: 0.8, y: 0.8 })
  })

  it('flushes pending moves before a discrete intent so order holds', () => {
    styleSetCursor(0, 0.4, 0.4)
    styleAddTarget(0, 'breakbeat')
    // The add fired immediately AND pushed the queued cursor move out first.
    expect(calls.map((c) => c.cmd)).toEqual(['style_set_cursor', 'style_add_target'])
    // The already-flushed move is not re-sent on the frame.
    flushRaf()
    expect(calls.filter((c) => c.cmd === 'style_set_cursor')).toHaveLength(1)
  })
})
