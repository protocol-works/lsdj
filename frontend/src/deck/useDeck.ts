import { useCallback, useEffect, useReducer, useRef, useState } from 'react'

import {
  BAND_HOP_FRAMES,
  bandSourceFromArrays,
  createBandScroller,
  type BandSource,
} from '../audio/bands'
import { EQ_FLAT, type EqBand } from '../audio/eq'
import { fxRestPosition, type FxKind } from '../audio/fx'
import {
  DEFAULT_LOOP_SECONDS,
  generatedLoopSeconds,
  LOOP_CROSSFADE_SECONDS,
  LOOP_SLOT_COUNT,
  quantiseLoopSeconds,
} from '../audio/loops'
import { createLoudnessTracker, trimDbFor } from '../audio/master'
import { STYLE_SAMPLE_SECONDS } from '../audio/styleSample'
import { useAudioEngine } from '../audio/engineContext'
import {
  getApiBaseUrl,
  setDeckCue,
  setDeckCuePoint,
  setDeckEq,
  setDeckFx,
  setDeckFxAmount,
  setDeckLoopLabels,
  setDeckMode,
  setDeckModel,
  setDeckPrimed,
  setDeckTrack,
  setDeckTransport,
  setDeckTrim,
  setDeckVolume,
  subscribeModelsChanged,
} from '../audio/nativeEngine'
import { fxKindFromSnap, useInterfaceStore } from '../audio/interfaceStore'
import {
  sendNativeDeckCommand,
  subscribeSidecarStatus,
  type DeckCommand,
} from './nativeDeck'
import { subscribeDeckPcm } from './nativeDeckPcm'
import {
  beatLoopRegion,
  clampRate,
  quantisedLoop,
  resizeLoop,
  snapToGrid,
  type TrackLoop,
} from '../audio/track'
import { loadDeckSettings, updateDeckSettings } from '../persistence'
import type { LoraChoice } from '../models/useLoras'
import {
  SAMPLE_RATE,
  type Beatgrid,
  type DeckChannel,
  type DeckId,
  type TrackSource,
} from '../audio/types'
import { TRACK_OVERVIEW_BUCKETS } from '../ui/TrackOverview'
import {
  deckReducer,
  initialDeckState,
  type DeckState,
  type ServerEvent,
} from './deckState'

/** Worklet stats older than this are stale: the live clocks and the
 * zoom view blank together, never a lie (ADR-0014). */
const STATS_FRESH_MS = 2_500

/** Gain-staging trim (M17): auto follows the source's loudness; a
 * manual knob move takes over until auto is re-engaged. */
export type TrimState = { mode: 'auto' | 'manual'; db: number }

/** One pad slot. The audio buffers live in the channel; `label` is null
 * for captures from the deck and the prompt for generated slots (M18).
 * One-shots overlay the playing source and end themselves. A filled loop
 * either REPLACES the live stream while active (a freeze) or LAYERS on
 * top of it (a loaded sample, `layer: true`, ADR-0022) — the play path
 * branches on `layer`. */
export type LoopSlot =
  | { state: 'empty' }
  | { state: 'pending'; label: string; oneShot: boolean }
  | { state: 'filled'; label: string | null; oneShot: boolean; layer: boolean }

export type LoopState = {
  slots: LoopSlot[]
  /** The slot currently replacing the live stream, if any (a freeze). */
  active: number | null
  /** Slots currently layering on top of the base (loaded samples, ADR-0022),
   * stacked — independent of `active`. */
  layering: number[]
  /** Capture length for the next press; persisted, unlike the loops. */
  seconds: number
}

export type GenerateEngine = 'sfx' | 'music' | 'magenta'

/** The backend caps prompts at a generous safety ceiling (sa3.MAX_PROMPT_LENGTH); the
 * input stops short of it so the BPM stamp (", NNN BPM") can never push a legal prompt
 * over the cap. High enough to hold a large structured/JSON prompt — not a UX limit. */
export const GENERATE_PROMPT_MAX_LENGTH = 32000 - ', 999 BPM'.length

/** A deck's source (M19, ADR-0013): the live Magenta stream, or one
 * decoded track. Loading decides the mode — there is no toggle. */
export type DeckMode = 'realtime' | 'playback'

/** The UI's view of the loaded track; the audio itself is session-only
 * in the channel, like every captured artefact. */
export type TrackState = {
  /** Monotonic per load — the collision-proof identity for derived
   * work like the overview envelope (titles can repeat). */
  loadId: number
  title: string
  duration: number
  position: number
  playing: boolean
  ended: boolean
  /** The offline tracker's verdict at load — null is honest (M14).
   * When a grid exists this is its refined BPM (one number, M20). */
  bpm: number | null
  /** The offline beatgrid (M20, ADR-0014), or null — no grid beats a
   * wrong grid. Gates ticks, the phase meter, and quantise; NOT sync. */
  grid: Beatgrid | null
  /** Varispeed rate (M20): 1 = as recorded; the readout shows
   * bpm × rate. */
  rate: number
  /** Hot cues (M21, ADR-0015): one slot per pad, in track seconds.
   * Session-only — they die with the load, like every captured
   * artefact (ADR-0011 precedent). */
  cues: (number | null)[]
  /** The active loop region, mirrored from the engine; null = linear. */
  loop: TrackLoop | null
  /** A loop IN press awaiting its OUT (already snapped). */
  pendingLoopIn: number | null
}

/** One hot-cue slot per pad in the bank. */
export const HOT_CUE_COUNT = 8

/** One deck's beat clock for the phase meter (M20): the context time
 * of some beat plus the period — two clocks compare by phase without
 * either side needing "now". */
export type BeatClock = { periodSeconds: number; beatAtContext: number }

/** One deck's feed for the zoom view (M22): hop-indexed band
 * energies, the playhead in hop units, the wall-time span of a hop
 * on this deck's display (track hops shrink under varispeed), the
 * beat lattice when the deck's clock is confident, the filled hot
 * cues in hop units (M21) — empty for a live deck, which has none —
 * and the active loop region in hop units (null when not looping). */
export type ZoomSource = {
  bands: BandSource
  playheadHop: number
  realSecondsPerHop: number
  beat: { periodHops: number; anchorHop: number } | null
  cues: number[]
  loop: { startHop: number; endHop: number } | null
}

/** SYNC's verdict (M20): refusals name their reason so the UI never
 * blames the wrong thing. */
export type SyncResult = 'synced' | 'no_tempo' | 'out_of_range'

const EMPTY_SLOT: LoopSlot = { state: 'empty' }

/** Lower bound on how often the playback playhead is mirrored UP into the store for the
 * MCP interface-state resource (ADR-0020). The deck's track-status poll already updates
 * `track` at ~4 Hz (a 250 ms interval), so this matches that and mainly guards
 * defensively against a future faster poll: an agent reads the resource on demand, and a
 * per-frame mirror would churn `store://changed` (and every projection re-render).
 * Rate/loop changes bypass the throttle — they are rare and worth reflecting at once. */
const TRANSPORT_MIRROR_MS = 250

function withSlot(current: LoopState, slot: number, value: LoopSlot): LoopSlot[] {
  return current.slots.map((existing, index) => (index === slot ? value : existing))
}

export type DeckControls = {
  state: DeckState
  volume: number
  eq: Record<EqBand, number>
  /** Headphone cue (PFL) on this channel. Deliberately not persisted:
   * a reload never blasts the phones unexpectedly. */
  cue: boolean
  setCue: (on: boolean) => void
  /** Color FX insert (M12): the selected effect and its knob position. */
  fx: { kind: FxKind | null; amount: number }
  /** Selecting an effect parks the knob at its rest position, so a
   * switch never lands mid-effect. */
  setFx: (kind: FxKind | null) => void
  setFxAmount: (amount: number) => void
  /** Freeze pads (M13, ADR-0009): one press on an empty slot captures
   * the just-played tail and loops it on air; a filled slot swaps in;
   * the active slot returns to live. Loops are session-only. */
  loop: LoopState
  toggleLoopPad: (slot: number) => void
  clearLoopPad: (slot: number) => void
  setLoopSeconds: (seconds: number) => void
  /** Generated pads (M18, ADR-0012): fill the first empty slot from a
   * text prompt. The engine picks the sound world — Stable Audio's
   * sfx/music models, or the booth's own third Magenta engine (a
   * dedicated render worker; first use pays its model load inside the
   * pending state). One-shots overlay, loops replace like captures;
   * music-model loops snap to whole bars while the tempo gate is
   * locked and respect the quality floor. An SA3 engine can ride a LoRA
   * stack — adapters + per-adapter strengths (issue #66); Magenta has no
   * adapter path. */
  generateToPad: (
    prompt: string,
    engine: GenerateEngine,
    oneShot: boolean,
    loras?: LoraChoice[] | null,
  ) => void
  /** Load a saved sample (ADR-0022) into the first empty loop slot, as a loop or
   * one-shot per the sample. Resolves false when every slot is full, the deck isn't
   * a live Realtime deck, or the body doesn't decode (surfaced via `generateError`). */
  loadSampleToSlot: (
    wav: ArrayBuffer,
    oneShot: boolean,
    label: string,
  ) => Promise<boolean>
  /** Why the last generation produced nothing, until the next attempt. */
  generateError: string | null
  /** Detected tempo of the deck's stream (M14, ADR-0010), or null
   * while the honesty gate refuses — never a wrong number. */
  bpm: number | null
  /** Style sampling (M15): the just-played tail as wire-format PCM,
   * or null when the deck has not played enough to embed. */
  captureStyleSample: () => Promise<Float32Array<ArrayBuffer> | null>
  /** Per-channel trim (M17): auto-gain toward the loudness target, or
   * a held manual value. */
  trim: TrimState
  setTrimDb: (db: number) => void
  enableAutoTrim: () => void
  /** Playback mode (M19, ADR-0013): trade the live stream for one
   * decoded track. loadTrack and leavePlayback are the only doors —
   * loading decides the mode. */
  mode: DeckMode
  track: TrackState | null
  loadTrack: (source: TrackSource, title: string) => Promise<boolean>
  leavePlayback: () => void
  /** Jump the track playhead (overview click / FLX4); playback-mode
   * only, a no-op on the live stream. */
  seekTrack: (seconds: number) => void
  /** Relative seek (the jog wheel): reads the channel's live playhead
   * so rapid ticks accumulate instead of racing the 250 ms poll. */
  nudgeTrack: (seconds: number) => void
  /** Varispeed (M20, ADR-0014): clamped to the ±8% envelope; the
   * synced echo's clock and the BPM readout follow. */
  setTrackRate: (rate: number) => void
  /** Phase nudge (jog while playing): slip the playhead via a stepped
   * rate bend — the platter drag, never a click. */
  nudgeTrackPhase: (seconds: number) => void
  /** SYNC: match the track's tempo to `targetBpm`. Refuses honestly,
   * and says why: no tempo on either side, or the required rate falls
   * outside the varispeed envelope. Needs no grid (ADR-0014). */
  syncTrack: (targetBpm: number | null) => SyncResult
  /** Hot cues (M21, ADR-0015): an empty slot captures the playhead
   * (snapped to the grid when confident), a filled one jumps to it —
   * a jump is a plain seek, so it exits any loop. */
  hotCuePad: (index: number) => void
  clearHotCue: (index: number) => void
  /** Track loop (M21): IN arms a start, OUT closes the region (both
   * quantised while a grid is confident; a free loop owes a minimum
   * length), EXIT releases it cleanly. */
  loopIn: () => void
  loopOut: () => void
  loopExit: () => void
  /** Beat loops (M23, ADR-0016): set a `beats`-long loop at the playhead
   * (grid-required; a no-op without one), and halve/double an active
   * loop's length. Set-only — the FLX4's 4 BEAT/EXIT toggle is in
   * dispatch, exiting via loopExit. */
  beatLoop: (beats: number) => void
  halveLoop: () => void
  doubleLoop: () => void
  /** The track's beat clock (playing + grid required), for the meter. */
  getTrackBeat: () => BeatClock | null
  /** The live stream's beat clock at the speakers (gated BPM, a
   * continuous anchor, fresh worklet stats required). */
  getLiveBeat: () => BeatClock | null
  /** The zoom view's feed (M22): track bands around the playhead, or
   * the live scroller at the played position — null when this deck
   * has nothing honest to show. */
  getZoomSource: () => ZoomSource | null
  /** Static envelope of the loaded track for the overview strip. */
  getTrackPeaks: (
    buckets: number,
  ) => { min: Float32Array; max: Float32Array } | null
  /** Generating but off air (M10): buffer fills, only the cue tap hears
   * it. play() then drops it on air without flushing what was built up.
   * On a playback deck, CUE instead returns the track to the top. */
  primed: boolean
  prime: () => Promise<void>
  play: () => Promise<void>
  stop: () => void
  setModel: (model: string) => void
  restartWorker: () => void
  setVolume: (volume: number) => void
  setEqBand: (band: EqBand, value: number) => void
  getChannelLevel: () => number
}

/** Owns one deck's native sidecar transport (control over IPC `deck_*`
 * commands, status over `sidecar://status`) and its channel on the shared
 * audio engine — the ring buffer → deck gain → crossfade bus now lives in
 * the native Rust engine (`src-tauri/engine/src/graph.rs`). */
export function useDeck(deckId: DeckId): DeckControls {
  const engine = useAudioEngine()
  const deckIndex = deckId === 'a' ? 0 : 1
  const [state, dispatch] = useReducer(deckReducer, initialDeckState)
  // Mixer fields are projections (ADR-0020 phase C): the SHELL hydrates the
  // engine and the store from its settings file before the webview exists,
  // so the first snapshot is authoritative — these initials only cover the
  // frames before it arrives (and match the shipped Rust defaults).
  const [volume, setVolumeState] = useState(0.8)
  const volumeRef = useRef(volume)
  const [eq, setEqState] = useState<Record<EqBand, number>>({
    low: EQ_FLAT,
    mid: EQ_FLAT,
    high: EQ_FLAT,
  })
  const eqRef = useRef(eq)
  const [cue, setCueState] = useState(false)
  const cueRef = useRef(cue)
  const [fx, setFxState] = useState<{ kind: FxKind | null; amount: number }>({
    kind: null,
    amount: 0,
  })
  const fxRef = useRef(fx)
  const [loop, setLoopState] = useState<LoopState>(() => ({
    slots: Array<LoopSlot>(LOOP_SLOT_COUNT).fill(EMPTY_SLOT),
    active: null,
    layering: [],
    seconds: loadDeckSettings(deckId).loopSeconds ?? DEFAULT_LOOP_SECONDS,
  }))
  const loopRef = useRef(loop)
  const setLoop = useCallback((next: LoopState) => {
    setLoopState(next)
    loopRef.current = next
  }, [])
  // Capture is a port round-trip; a STOP or another pad press landing
  // inside that window must win over the stale capture. Every loop
  // gesture bumps this, and the capture callback bails if it moved.
  const loopGestureRef = useRef(0)
  // Generations are slower round-trips with the same race: a clear (or
  // a newer generation) during the flight must win. Per-slot counters,
  // bumped by anything that takes the slot over.
  const slotGenerationRef = useRef<number[]>(Array<number>(LOOP_SLOT_COUNT).fill(0))
  const [generateError, setGenerateError] = useState<string | null>(null)
  const [bpm, setBpm] = useState<number | null>(null)
  const [mode, setModeState] = useState<DeckMode>('realtime')
  const modeRef = useRef(mode)
  const setMode = useCallback(
    (next: DeckMode) => {
      setModeState(next)
      modeRef.current = next
      // Record it in the store (phase D) so an agent sees playback vs
      // realtime natively; the load orchestration itself stays here.
      setDeckMode(deckIndex, next)
    },
    [deckIndex],
  )
  const [track, setTrack] = useState<TrackState | null>(null)
  const trackLoadRef = useRef(0)
  // Fresh mirrors for the beat clocks and sync (state would be stale
  // inside callbacks): the loaded track's analysis and rate, the
  // latest worklet stats, and the continuity-approved live anchor.
  const trackMetaRef = useRef<{ bpm: number | null; grid: Beatgrid | null } | null>(
    null,
  )
  const trackRateRef = useRef(1)
  // Hot cues and the pending loop IN (M21): refs beside the state so
  // bus-driven callbacks read fresh values without re-subscribing.
  const trackCuesRef = useRef<(number | null)[]>([])
  const pendingLoopInRef = useRef<number | null>(null)
  const statsRef = useRef<{
    playing: boolean
    playedFrames: number
    contextTime: number
    receivedAt: number
  } | null>(null)
  const liveBeatRef = useRef<{ anchorFrame: number; bpm: number } | null>(null)
  // The gated readout, mirrored for the synchronous consumers (freeze/loop
  // quantise read it at press time). Fed by the store projection below.
  const bpmRef = useRef<number | null>(null)
  // The played-frame origin for the live beat clock: the engine context-frame
  // count at the stream's reset, captured SHELL-SIDE atomically with the
  // tracker reset (ADR-0025) and published with the analysis. Subtracting it
  // maps the engine's global render count back into the tracker's
  // pushed-frames-since-reset domain (ADR-0014).
  const analysisOriginRef = useRef(0)
  // The loudness tracker behind auto-gain (M17) — reset on stream
  // discontinuities so a measurement never spans two unrelated streams (the
  // reset rule the capture history follows too, ADR-0009); the beat tracker
  // that shared this rule lives shell-side now (ADR-0025) and resets over
  // `resetAnalysis`. The trim VALUE holds across resets.
  const [loudness] = useState(() => createLoudnessTracker(SAMPLE_RATE))
  // Band envelopes for the zoom view (M22): the live wire feeds a
  // rolling scroller; a loaded track's arrive from the shell at load.
  const [bandScroller] = useState(() => createBandScroller(SAMPLE_RATE))
  const trackBandsRef = useRef<BandSource | null>(null)
  const resetStreamMeasurements = useCallback(() => {
    // Shell-side: tracker + gates + origin reset atomically in stream order;
    // the blank publishes back over the store. Blank the local mirrors now so
    // the readout can't flash a dead stream's number while that round-trips.
    channelRef.current?.resetAnalysis()
    loudness.reset()
    setBpm(null)
    bpmRef.current = null
    liveBeatRef.current = null
    bandScroller.reset()
  }, [loudness, bandScroller])
  // The trim VALUE lives in the store (shell-hydrated, phase C); only the
  // auto/manual MODE is webview state — the auto-gain loudness tracker is
  // TS, so auto-trim stays an intent stream from here (inventory
  // constraint 4) and the mode persists webview-side.
  const [trim, setTrimState] = useState<TrimState>(() => ({
    mode: loadDeckSettings(deckId).trimMode ?? 'auto',
    db: 0,
  }))
  const trimRef = useRef(trim)
  const applyTrim = useCallback(
    (next: TrimState) => {
      setTrimState(next)
      trimRef.current = next
      updateDeckSettings(deckId, { trimMode: next.mode })
      // A deck-indexed intent, not a channel call (phase C): the write must
      // reach the engine and the store even before the channel exists.
      setDeckTrim(deckIndex, next.db)
    },
    [deckId, deckIndex],
  )

  // Project the per-deck mixer from the store (ADR-0020 phase C). The shell
  // hydrates engine + store before the webview exists, so every snapshot is
  // authoritative — the old per-field synced gates (which fenced off the
  // pre-hydration Rust defaults until the webview's own boot replay echoed)
  // are gone with the boot replay itself. Our own setters update the refs
  // before the store echoes, so an echo compares equal and never loops; the
  // epsilon absorbs the 14-bit MIDI position-sync quantum (a centre detent
  // echoes as 0.5000305, not 0.5).
  const storeState = useInterfaceStore()
  // Trim is the one field whose ADOPTION carries a semantic: an external
  // (MCP/MIDI) trim write is deliberate, so it flips the deck to manual —
  // but the FIRST snapshot is boot hydration, not a gesture, so it seeds
  // the value without stealing auto mode.
  const trimSeededRef = useRef(false)
  useEffect(() => {
    const mix = storeState?.decks[deckIndex]
    if (!mix) return
    const near = (a: number, b: number) => Math.abs(a - b) < 1e-3

    if (!near(mix.volume, volumeRef.current)) {
      volumeRef.current = mix.volume
      setVolumeState(mix.volume)
    }

    if (
      !near(mix.eq.low, eqRef.current.low) ||
      !near(mix.eq.mid, eqRef.current.mid) ||
      !near(mix.eq.high, eqRef.current.high)
    ) {
      const next = { low: mix.eq.low, mid: mix.eq.mid, high: mix.eq.high }
      eqRef.current = next
      setEqState(next)
    }

    if (mix.cue !== cueRef.current) {
      cueRef.current = mix.cue
      setCueState(mix.cue)
    }

    const fxKind = fxKindFromSnap(mix.fx.kind)
    if (fxKind !== fxRef.current.kind || !near(mix.fx.amount, fxRef.current.amount)) {
      const next = { kind: fxKind, amount: mix.fx.amount }
      fxRef.current = next
      setFxState(next)
    }

    if (!trimSeededRef.current) {
      trimSeededRef.current = true
      if (!near(mix.trimDb, trimRef.current.db)) {
        const next = { mode: trimRef.current.mode, db: mix.trimDb }
        trimRef.current = next
        setTrimState(next)
      }
    } else if (!near(mix.trimDb, trimRef.current.db)) {
      const next = { mode: 'manual' as const, db: mix.trimDb }
      trimRef.current = next
      setTrimState(next)
    }
  }, [storeState, deckIndex])

  // Mirror the realtime deck's model read-back UP into the store, so an MCP agent
  // observes it (ADR-0020). The webview derives it from worker status ('ready' /
  // 'model_loading'); a write-only mirror. `playing` is deliberately NOT mirrored:
  // the store owns the transport — deck_play/deck_stop write it for every
  // controller, and the Rust status relay drops it when a worker dies or reloads —
  // and the projection below is the reducer's only coupling to it.
  useEffect(() => {
    setDeckModel(deckIndex, state.model)
  }, [deckIndex, state.model])

  // Project the store-owned hot cues into the rendered track (phase D). The
  // store resets the bank with the track identity (set_deck_track) and applies
  // the set/clear intents — ours and an MCP agent's alike — so the pads light
  // only through this echo; there is no local cue write left to fence, and the
  // cuesSyncedRef gate died with the mirror it guarded.
  useEffect(() => {
    const storeCues = storeState?.decks[deckIndex]?.cues
    if (!storeCues) return
    const current = trackCuesRef.current
    const sameCues =
      storeCues.length === current.length &&
      storeCues.every((point, i) => point === current[i])
    if (sameCues) return
    const next = [...storeCues]
    trackCuesRef.current = next
    setTrack((track) => track && { ...track, cues: next })
  }, [storeState, deckIndex])

  // Mirror the loaded-track identity UP into the store (ADR-0020): title, BPM, and
  // duration on a playback deck, null with no track. A write-only read-back mirror,
  // keyed on the identity fields (not the whole track object) so cue/transport
  // churn doesn't re-push it.
  useEffect(() => {
    setDeckTrack(
      deckIndex,
      track
        ? { title: track.title, bpm: track.bpm, durationSeconds: track.duration }
        : null,
    )
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [deckIndex, track?.title, track?.bpm, track?.duration])

  // Mirror the playback transport (playhead / rate / loop) UP into the store for the
  // MCP interface-state resource (ADR-0020). `track` is a fresh object on each ~4 Hz
  // (250 ms) status poll, so this effect runs at that rate; the throttle ref bounds the
  // playhead push to TRANSPORT_MIRROR_MS (defensive against a faster poll), while a rate
  // or loop change goes up at once. Null on a realtime deck / with no track.
  const transportMirrorRef = useRef<{
    at: number
    rate: number | null
    loopKey: string
  }>({ at: 0, rate: null, loopKey: '' })
  useEffect(() => {
    if (!track) {
      // Clear once on the transition to no-track; nothing to push while it stays null.
      if (transportMirrorRef.current.rate !== null) {
        transportMirrorRef.current = { at: 0, rate: null, loopKey: '' }
        setDeckTransport(deckIndex, null)
      }
      return
    }
    const loopKey = track.loop ? `${track.loop.start}:${track.loop.end}` : ''
    const last = transportMirrorRef.current
    const changed = track.rate !== last.rate || loopKey !== last.loopKey
    const now = performance.now()
    if (!changed && now - last.at < TRANSPORT_MIRROR_MS) return
    transportMirrorRef.current = { at: now, rate: track.rate, loopKey }
    setDeckTransport(deckIndex, {
      playheadSeconds: track.position,
      rate: track.rate,
      loopRegion: track.loop
        ? { startSeconds: track.loop.start, endSeconds: track.loop.end }
        : null,
    })
  }, [deckIndex, track])

  // Mirror the freeze/sample loop-slot labels UP into the store (ADR-0020): a
  // read-back the webview writes when its slots change (null for an empty slot).
  useEffect(() => {
    setDeckLoopLabels(
      deckIndex,
      loop.slots.map((slot) => (slot.state === 'empty' ? null : slot.label)),
    )
  }, [deckIndex, loop.slots])

  const [primed, setPrimedState] = useState(false)
  const primedRef = useRef(primed)

  const setPrimed = useCallback((next: boolean) => {
    setPrimedState(next)
    primedRef.current = next
  }, [])

  // Mirror the primed-off-air state UP into the store: the native LED painter
  // reads it for the transport-CUE LED (ADR-0031 — LEDs read the store).
  useEffect(() => {
    setDeckPrimed(deckIndex, primed)
  }, [deckIndex, primed])

  const channelRef = useRef<DeckChannel | null>(null)
  // Memoised in-flight channel build so rapid play() clicks share one
  // channel instead of stacking worklets on the bus.
  const channelPromiseRef = useRef<Promise<DeckChannel> | null>(null)

  const ensureChannel = useCallback(() => {
    if (!channelPromiseRef.current) {
      channelPromiseRef.current = engine
        .createDeckChannel(
          deckId,
          {
            volume: volumeRef.current,
            eq: eqRef.current,
            cue: cueRef.current,
            fx: fxRef.current,
            trimDb: trimRef.current.db,
          },
          (stats) => {
            statsRef.current = { ...stats, receivedAt: performance.now() }
            dispatch({ type: 'worklet_stats', stats })
          },
        )
        .then((channel) => {
          channelRef.current = channel
          return channel
        })
        .catch((error: unknown) => {
          channelPromiseRef.current = null // allow a retry after failure
          throw error
        })
    }
    return channelPromiseRef.current
  }, [engine, deckId])

  // Project the store's transport into the reducer (ADR-0020: the store OWNS
  // `playing`). Every path lands there — our own play()/stop() via the deck_play /
  // deck_stop commands, an MCP agent's tools, the Rust status relay when a worker
  // dies or reloads — so the button lights when the store's snapshot round-trips,
  // and never before. One direction only (nothing mirrors `playing` back up), so
  // no echo can loop, and the store's emit order is the total order. On a play we
  // also ensure the deck channel exists (idempotent — applies the current params
  // and starts the stats poll, no ring reset), so the buffer/BPM/underrun meters
  // populate for an agent-started deck too. `playingRef` mirrors the projected
  // transport for the stable-dep callbacks. (The old `playPendingRef` in-flight
  // guard is gone, phase D: deck_play's atomic start_transport in the Rust
  // store is the ordering now — a second tap is a shell-side no-op, and the
  // webview's own pre-send work is idempotent.)
  const playingRef = useRef(false)
  useEffect(() => {
    playingRef.current = state.playing
  }, [state.playing])
  useEffect(() => {
    const storePlaying = storeState?.decks[deckIndex]?.playing
    if (storePlaying === undefined) return
    if (storePlaying === state.playing) return
    if (storePlaying) void ensureChannel().catch(() => {})
    dispatch({ type: 'playing_changed', playing: storePlaying })
  }, [storeState, deckIndex, state.playing, ensureChannel])

  useEffect(() => {
    // The sidecar feeds the engine directly and reports status as `sidecar://status`
    // events, teeing the model PCM back over a Tauri Channel so the TS
    // beat/loudness/band analysis (ADR-0017: stays in TypeScript) gets the same input.
    // The engine + sidecar IPC is available immediately — there is no socket
    // handshake to wait for, so the deck is "open" (operable) at once. The
    // sidecar's `ready`/`model_loading` status events then fill in the model +
    // switch state.
    dispatch({ type: 'socket_open' })
    const unsubscribeStatus = subscribeSidecarStatus(deckId, (status) => {
      if (status.event === 'model_loading' || status.event === 'worker_died') {
        channelRef.current?.reset()
        resetStreamMeasurements()
      }
      dispatch({ type: 'server_event', event: status as ServerEvent })
    })
    const unsubscribePcm = subscribeDeckPcm(deckId, (samples) => {
      // A parked deck (playback mode) drops stragglers so a late chunk cannot
      // pollute the track's clock. Beat analysis reads the same wire
      // shell-side (ADR-0025); this tap feeds the live visuals that stayed
      // in TypeScript (loudness, the band scroller).
      if (modeRef.current === 'playback') return
      loudness.push(samples)
      bandScroller.push(samples)
    })
    // The model list + RAM info come from the generation server so the model
    // picker populates and the RAM warning works. Re-fetched when the model
    // manager installs or removes a Magenta model (`models://changed`, issue #43).
    let cancelled = false
    const fetchModels = () => {
      void getApiBaseUrl()
        .then((base) => fetch(`${base}/api/models`))
        .then((response) => (response.ok ? response.json() : null))
        .then((info) => {
          if (cancelled || !info) return
          dispatch({
            type: 'deck_info',
            models: info.models,
            ramInfo: {
              totalGb: info.total_ram_gb,
              estimateGbByModel: info.model_ram_estimate_gb,
            },
          })
        })
        .catch(() => {})
    }
    fetchModels()
    const unsubscribeModels = subscribeModelsChanged(fetchModels)
    return () => {
      cancelled = true
      unsubscribeStatus()
      unsubscribePcm()
      unsubscribeModels()
      channelRef.current?.dispose()
      channelRef.current = null
      channelPromiseRef.current = null
    }
  }, [deckId, loudness, bandScroller, resetStreamMeasurements])

  // Project the shell's live beat analysis (ADR-0025): the honesty-gated
  // readout and the anchor-agreed phase clock arrive over the store at the
  // estimate cadence (~1/s while streaming), already gated — the webview just
  // mirrors them. A playback deck ignores the live values (its clock is the
  // track's grid, ADR-0013), but the stream origin is domain state either way.
  useEffect(() => {
    const analysis = storeState?.decks[deckIndex]?.analysis
    if (!analysis) return
    analysisOriginRef.current = analysis.originFrames
    if (modeRef.current === 'playback') return
    setBpm(analysis.bpm)
    bpmRef.current = analysis.bpm
    liveBeatRef.current = analysis.liveBeat
  }, [storeState, deckIndex])

  // Auto-gain (M17) rides a once-a-second tick: a slow glide toward the
  // loudness target, held when the meter has nothing trustworthy. (The beat
  // estimate that shared this tick lives shell-side now, ADR-0025.)
  useEffect(() => {
    const timer = setInterval(() => {
      if (modeRef.current === 'playback') return
      if (trimRef.current.mode === 'auto') {
        const db = trimDbFor(loudness.rms())
        if (db !== null && Math.abs(db - trimRef.current.db) > 0.1) {
          applyTrim({ mode: 'auto', db })
        }
      }
    }, 1_000)
    return () => clearInterval(timer)
  }, [loudness, applyTrim])

  const send = useCallback(
    (command: DeckCommand) => {
      // Forward control to the sidecar over IPC (deck_* commands).
      sendNativeDeckCommand(deckId, command)
    },
    [deckId],
  )

  const play = useCallback(async () => {
    // A playback deck's PLAY drives the track, not the worker.
    if (modeRef.current === 'playback') {
      try {
        await engine.resume()
      } catch (error) {
        dispatch({
          type: 'local_error',
          error: error instanceof Error ? error.message : String(error),
        })
        return
      }
      channelRef.current?.playTrack()
      return
    }
    // Dropping a primed deck on air: the worker already streams and the
    // buffer holds the prepped audio — unmute, don't flush or replay.
    if (primedRef.current) {
      channelRef.current?.setOnAir(true)
      setPrimed(false)
      return
    }
    try {
      const channel = await ensureChannel()
      await engine.resume()
      // Drop whatever an earlier session left in the ring buffer, so the
      // first thing heard is the new stream, not stale chunks. The beat
      // tracker starts over with the stream. All of this is idempotent —
      // a second tap racing the round-trip re-runs it harmlessly, and the
      // deck_play it re-sends is a shell-side no-op (start_transport).
      channel.reset()
      resetStreamMeasurements()
      channel.setOnAir(true)
    } catch (error) {
      dispatch({
        type: 'local_error',
        error: error instanceof Error ? error.message : String(error),
      })
      return
    }
    // The deck_play command drives the worker AND writes the store's transport;
    // the button lights when the snapshot round-trips (the transport projection).
    send({ type: 'play' })
  }, [ensureChannel, engine, send, setPrimed, resetStreamMeasurements])

  const seekTrack = useCallback((seconds: number) => {
    if (modeRef.current !== 'playback') return
    channelRef.current?.seekTrack(seconds)
    // A seek exits the loop at the engine (ADR-0015); a pending IN
    // dies with it — a region spanning a jump would be an accident.
    pendingLoopInRef.current = null
    const status = channelRef.current?.getTrackStatus()
    if (!status) return
    setTrack(
      (current) =>
        current && {
          ...current,
          position: status.position,
          playing: status.playing,
          ended: status.ended,
          loop: status.loop,
          pendingLoopIn: null,
        },
    )
  }, [])

  /** Start generating off air: like play(), but muted on the master so
   * the prep is only audible over the cue tap (M10 transport CUE). */
  const prime = useCallback(async () => {
    // Transport CUE on a track deck: return to the top, parked — the
    // deck-prep semantics, adapted (ADR-0013). Through the hook's
    // seek so the loop exit and the pending IN reach the UI mirror
    // (ADR-0015 — every seek path, no ghost regions).
    if (modeRef.current === 'playback') {
      channelRef.current?.pauseTrack()
      seekTrack(0)
      return
    }
    if (primedRef.current) return
    try {
      const channel = await ensureChannel()
      await engine.resume()
      channel.reset()
      resetStreamMeasurements()
      channel.setOnAir(false)
    } catch (error) {
      dispatch({
        type: 'local_error',
        error: error instanceof Error ? error.message : String(error),
      })
      return
    }
    setPrimed(true)
    // A primed deck IS playing (generating, off air): deck_play writes the store's
    // transport, so the button lights over the same projection as a plain play.
    send({ type: 'play' })
  }, [ensureChannel, engine, send, setPrimed, resetStreamMeasurements, seekTrack])

  // Stop every layered sample on the engine (ADR-0022); the caller resets the UI
  // `layering` set in its own setLoop. Reads only refs, so it stays stable.
  const silenceLayers = useCallback(() => {
    for (const slot of loopRef.current.layering) {
      channelRef.current?.stopLayer(slot)
    }
  }, [])

  const stop = useCallback(() => {
    // A playback deck's STOP pauses the track; running pads stop with
    // it, exactly as on the live deck.
    if (modeRef.current === 'playback') {
      channelRef.current?.pauseTrack()
      loopGestureRef.current += 1
      channelRef.current?.stopLoop()
      channelRef.current?.stopOneShot()
      silenceLayers()
      if (loopRef.current.active !== null || loopRef.current.layering.length > 0) {
        setLoop({ ...loopRef.current, active: null, layering: [] })
      }
      return
    }
    send({ type: 'stop' })
    // Flush instead of letting the buffered seconds play out, so stop is
    // immediate like a DJ expects. The empty channel goes back on air so
    // the next plain play() isn't silent.
    channelRef.current?.reset()
    channelRef.current?.setOnAir(true)
    // STOP silences the deck — a running freeze loop goes with it (the
    // slot keeps its capture), any layered samples stop too, a ringing
    // one-shot is cut, and an in-flight capture may not land.
    loopGestureRef.current += 1
    channelRef.current?.stopLoop()
    channelRef.current?.stopOneShot()
    silenceLayers()
    if (loopRef.current.active !== null || loopRef.current.layering.length > 0) {
      setLoop({ ...loopRef.current, active: null, layering: [] })
    }
    resetStreamMeasurements()
    setPrimed(false)
  }, [send, setPrimed, setLoop, resetStreamMeasurements, silenceLayers])

  const loadTrack = useCallback(
    async (source: TrackSource, title: string) => {
      let channel: DeckChannel
      try {
        channel = await ensureChannel()
        await engine.resume()
      } catch (error) {
        dispatch({
          type: 'local_error',
          error: error instanceof Error ? error.message : String(error),
        })
        return false
      }
      // A rolling deck keeps rolling across a load: read it before the
      // load replaces the channel's track and STOP parks the source.
      // "Rolling" means ON AIR — a primed deck is audible only in the
      // phones, so its track loads parked rather than blasting the
      // master (the deck-prep semantics, ADR-0013).
      const wasPlaying =
        modeRef.current === 'playback'
          ? (channel.getTrackStatus()?.playing ?? false)
          : state.playing && !primedRef.current
      // The shell reads, decodes, and runs the offline passes (ADR-0030):
      // the same honesty bar as the stream, the refined grid BPM and the
      // coarse verdict already collapsed to one number (M20). Only numbers
      // and summaries come back.
      const loaded = await channel.loadTrack(source, TRACK_OVERVIEW_BUCKETS)
      if (!loaded) return false
      // Park whatever was running — the live stream's worker idles
      // warm, a previous track pauses — exactly like STOP (ADR-0013).
      stop()
      const { bpm: trackTempo, grid } = loaded
      // One debug line per load: "why no ticks?" answers itself in the
      // console (the shell logs the beatgrid's refusal numbers to stderr).
      console.debug('[beatgrid] verdict', deckId, grid, 'bpm', trackTempo)
      trackBandsRef.current = bandSourceFromArrays(
        loaded.bands.low,
        loaded.bands.mid,
        loaded.bands.high,
      )
      trackMetaRef.current = { bpm: trackTempo, grid }
      trackRateRef.current = 1
      // The local seed matches the fresh bank the set_deck_track mirror
      // opens store-side (the store owns the points, phase D).
      trackCuesRef.current = Array<number | null>(HOT_CUE_COUNT).fill(null)
      pendingLoopInRef.current = null
      setMode('playback')
      if (wasPlaying) channel.playTrack()
      setTrack({
        loadId: ++trackLoadRef.current,
        title,
        duration: loaded.duration,
        position: 0,
        playing: wasPlaying,
        ended: false,
        bpm: trackTempo,
        grid,
        rate: 1,
        cues: trackCuesRef.current,
        loop: null,
        pendingLoopIn: null,
      })
      return true
    },
    [ensureChannel, engine, stop, setMode, state.playing, deckId],
  )

  const leavePlayback = useCallback(() => {
    if (modeRef.current !== 'playback') return
    // A rolling track hands straight back to the stream; a parked one
    // leaves the deck stopped, like a track load in reverse.
    const wasPlaying = channelRef.current?.getTrackStatus()?.playing ?? false
    channelRef.current?.unloadTrack()
    trackMetaRef.current = null
    trackBandsRef.current = null
    trackRateRef.current = 1
    trackCuesRef.current = []
    pendingLoopInRef.current = null
    setMode('realtime')
    setTrack(null)
    // The stream's measurements start over either way.
    resetStreamMeasurements()
    if (wasPlaying) void play()
  }, [setMode, resetStreamMeasurements, play])

  const getTrackPeaks = useCallback(
    (buckets: number) => channelRef.current?.getTrackPeaks(buckets) ?? null,
    [],
  )

  const nudgeTrack = useCallback(
    (seconds: number) => {
      const status = channelRef.current?.getTrackStatus()
      if (!status) return
      seekTrack(status.position + seconds)
    },
    [seekTrack],
  )

  const setTrackRate = useCallback((rate: number) => {
    if (modeRef.current !== 'playback') return
    const clamped = clampRate(rate)
    trackRateRef.current = clamped
    // The synced echo's clock follows varispeed shell-side: the rate command
    // recomputes 60 / (bpm × rate) from the load-time analysis (ADR-0030).
    channelRef.current?.setTrackRate(clamped)
    setTrack((current) => current && { ...current, rate: clamped })
  }, [])

  const nudgeTrackPhase = useCallback((seconds: number) => {
    if (modeRef.current !== 'playback') return
    channelRef.current?.nudgeTrackPhase(seconds)
  }, [])

  const hotCuePad = useCallback(
    (index: number) => {
      if (modeRef.current !== 'playback') return
      if (index < 0 || index >= HOT_CUE_COUNT) return
      const existing = trackCuesRef.current[index] ?? null
      if (existing !== null) {
        // Filled pad: jump. The seek path carries the loop-exit rule.
        seekTrack(existing)
        return
      }
      const status = channelRef.current?.getTrackStatus()
      if (!status) return
      // Empty pad: capture the playhead, on the lattice when the grid
      // is confident, free when not (the consumer rule) — then hand the
      // point to the store; the pad lights when the snapshot echoes.
      const grid = trackMetaRef.current?.grid ?? null
      const cue = Math.min(snapToGrid(status.position, grid), status.duration)
      setDeckCuePoint(deckIndex, index, cue)
    },
    [seekTrack, deckIndex],
  )

  const clearHotCue = useCallback(
    (index: number) => {
      if (modeRef.current !== 'playback') return
      if (trackCuesRef.current[index] == null) return
      setDeckCuePoint(deckIndex, index, null)
    },
    [deckIndex],
  )

  const loopIn = useCallback(() => {
    if (modeRef.current !== 'playback') return
    const status = channelRef.current?.getTrackStatus()
    if (!status) return
    const grid = trackMetaRef.current?.grid ?? null
    const start = snapToGrid(status.position, grid)
    pendingLoopInRef.current = start
    setTrack((current) => current && { ...current, pendingLoopIn: start })
  }, [])

  const loopOut = useCallback(() => {
    if (modeRef.current !== 'playback') return
    const start = pendingLoopInRef.current
    // OUT with no IN armed is a no-op, not a guess.
    if (start === null) return
    const status = channelRef.current?.getTrackStatus()
    if (!status) return
    const grid = trackMetaRef.current?.grid ?? null
    const region = quantisedLoop(start, status.position, grid, status.duration)
    if (!region) return
    channelRef.current?.setTrackLoop(region.start, region.end)
    pendingLoopInRef.current = null
    // Mirror what the engine actually holds — its boundary may refuse.
    const loop = channelRef.current?.getTrackStatus()?.loop ?? null
    setTrack((current) => current && { ...current, loop, pendingLoopIn: null })
  }, [])

  const loopExit = useCallback(() => {
    if (modeRef.current !== 'playback') return
    channelRef.current?.clearTrackLoop()
    pendingLoopInRef.current = null
    setTrack(
      (current) => current && { ...current, loop: null, pendingLoopIn: null },
    )
  }, [])

  const beatLoop = useCallback((beats: number) => {
    if (modeRef.current !== 'playback') return
    const status = channelRef.current?.getTrackStatus()
    if (!status) return
    const grid = trackMetaRef.current?.grid ?? null
    const region = beatLoopRegion(status.position, beats, grid, status.duration)
    // Grid-required: a beat loop on a gridless track is a no-op, not a guess.
    if (!region) return
    channelRef.current?.setTrackLoop(region.start, region.end)
    // A fresh region drops any half-armed IN, like loopOut and seekTrack.
    pendingLoopInRef.current = null
    const loop = channelRef.current?.getTrackStatus()?.loop ?? null
    setTrack((current) => current && { ...current, loop, pendingLoopIn: null })
  }, [])

  // Halve/double scale the active loop's length (M23) — pure arithmetic,
  // no grid needed; the engine's planLoopSet re-anchors a playhead the
  // resize leaves outside the region, the same path loopOut uses.
  const resizeTrackLoop = useCallback((factor: number) => {
    if (modeRef.current !== 'playback') return
    const status = channelRef.current?.getTrackStatus()
    if (!status?.loop) return
    const region = resizeLoop(status.loop, factor, status.duration)
    if (!region) return
    channelRef.current?.setTrackLoop(region.start, region.end)
    const loop = channelRef.current?.getTrackStatus()?.loop ?? null
    setTrack((current) => current && { ...current, loop })
  }, [])
  const halveLoop = useCallback(() => resizeTrackLoop(0.5), [resizeTrackLoop])
  const doubleLoop = useCallback(() => resizeTrackLoop(2), [resizeTrackLoop])

  const syncTrack = useCallback(
    (targetBpm: number | null): SyncResult => {
      const bpm = trackMetaRef.current?.bpm ?? null
      if (bpm === null || targetBpm === null) return 'no_tempo'
      const required = targetBpm / bpm
      // Out of the varispeed envelope: refuse rather than land close
      // and pretend (ADR-0014).
      if (clampRate(required) !== required) return 'out_of_range'
      setTrackRate(required)
      return 'synced'
    },
    [setTrackRate],
  )

  const getTrackBeat = useCallback((): BeatClock | null => {
    const grid = trackMetaRef.current?.grid ?? null
    const status = channelRef.current?.getTrackStatus()
    if (!grid || !status?.playing) return null
    const periodTrack = 60 / grid.bpm
    const phase =
      ((((status.position - grid.firstBeatSeconds) / periodTrack) % 1) + 1) % 1
    const periodContext = periodTrack / status.rate
    return {
      periodSeconds: periodContext,
      beatAtContext: status.contextTime - phase * periodContext,
    }
  }, [])

  const getZoomSource = useCallback((): ZoomSource | null => {
    const hopSeconds = BAND_HOP_FRAMES / SAMPLE_RATE
    if (modeRef.current === 'playback') {
      const bands = trackBandsRef.current
      const status = channelRef.current?.getTrackStatus()
      if (!bands || !status) return null
      const grid = trackMetaRef.current?.grid ?? null
      return {
        bands,
        playheadHop: status.position / hopSeconds,
        // Varispeed squeezes more track-hops into a wall second.
        realSecondsPerHop: hopSeconds / status.rate,
        beat: grid
          ? {
              periodHops: 60 / grid.bpm / hopSeconds,
              anchorHop: grid.firstBeatSeconds / hopSeconds,
            }
          : null,
        // Filled hot cues, track seconds → hops, in the same domain as
        // the playhead so the strip lines them up exactly (M21).
        cues: trackCuesRef.current
          .filter((cue): cue is number => cue !== null)
          .map((cue) => cue / hopSeconds),
        // The active loop region, same seconds → hops conversion so the
        // wash and its entry/exit edges land where the audio wraps (M21).
        loop: status.loop
          ? {
              startHop: status.loop.start / hopSeconds,
              endHop: status.loop.end / hopSeconds,
            }
          : null,
      }
    }
    const stats = statsRef.current
    if (!stats?.playing) return null
    if (performance.now() - stats.receivedAt > STATS_FRESH_MS) return null
    const contextNow = engine.getContextTime()
    if (contextNow === null) return null
    // The played index in the pushed-frame domain — the scroller's
    // and the beat anchor's clock (M20). Subtract the per-stream origin the
    // shell captured at reset (ADR-0025) so this shares the tracker's
    // reset-to-0 frame domain.
    const playedFrames =
      stats.playedFrames -
      analysisOriginRef.current +
      (contextNow - stats.contextTime) * SAMPLE_RATE
    const clock = liveBeatRef.current
    return {
      bands: bandScroller.source(),
      playheadHop: playedFrames / BAND_HOP_FRAMES,
      realSecondsPerHop: hopSeconds,
      beat: clock
        ? {
            periodHops: 60 / clock.bpm / hopSeconds,
            anchorHop: clock.anchorFrame / BAND_HOP_FRAMES,
          }
        : null,
      // Hot cues and track loops are playback artefacts (ADR-0015); a
      // live deck has neither.
      cues: [],
      loop: null,
    }
  }, [engine, bandScroller])

  const getLiveBeat = useCallback((): BeatClock | null => {
    const clock = liveBeatRef.current
    const stats = statsRef.current
    if (!clock || !stats?.playing) return null
    // Stale stats mean a stale clock: blank, never a lie (ADR-0014).
    if (performance.now() - stats.receivedAt > STATS_FRESH_MS) return null
    return {
      periodSeconds: 60 / clock.bpm,
      // Subtract the per-stream origin (captured shell-side at reset,
      // ADR-0025) so anchorFrame (pushed, resets) and playedFrames (native:
      // global render count) share a frame domain; their difference is the
      // buffer lead.
      beatAtContext:
        stats.contextTime +
        (clock.anchorFrame - (stats.playedFrames - analysisOriginRef.current)) /
          SAMPLE_RATE,
    }
  }, [])

  // The playhead readout follows the channel while a track is loaded —
  // the graph is the source of truth (the LevelMeter pattern).
  useEffect(() => {
    if (mode !== 'playback') return
    const timer = setInterval(() => {
      const status = channelRef.current?.getTrackStatus()
      if (!status) return
      setTrack(
        (current) =>
          current && {
            ...current,
            position: status.position,
            playing: status.playing,
            ended: status.ended,
            // The loop mirrors generically: any engine-side exit
            // (every seek path, ADR-0015) reaches the UI within a
            // poll tick — no ghost regions.
            loop: status.loop,
          },
      )
    }, 250)
    return () => clearInterval(timer)
  }, [mode])

  const setModel = useCallback(
    (model: string) => {
      send({ type: 'set_model', model })
    },
    [send],
  )

  const restartWorker = useCallback(() => {
    // Carry the current model so the native path can respawn the same model (the
    // web controller ignores it and restarts with the model it already tracks).
    send({ type: 'restart', model: state.model ?? undefined })
  }, [send, state.model])

  const setVolume = useCallback(
    (next: number) => {
      setVolumeState(next)
      volumeRef.current = next
      setDeckVolume(deckIndex, next)
    },
    [deckIndex],
  )

  const getChannelLevel = useCallback(
    () => channelRef.current?.getLevel() ?? 0,
    [],
  )

  const captureStyleSample = useCallback(
    () =>
      // The worklet history holds the dead stream in playback mode;
      // sampling a track will slice the buffer instead — deferred past
      // M19 (ADR-0013).
      modeRef.current === 'playback'
        ? Promise.resolve(null)
        : (channelRef.current?.captureSample(STYLE_SAMPLE_SECONDS) ??
          Promise.resolve(null)),
    [],
  )

  const setTrimDb = useCallback(
    (db: number) => applyTrim({ mode: 'manual', db }),
    [applyTrim],
  )

  const enableAutoTrim = useCallback(() => {
    // Snap to the tracker's current opinion when it has one; the next
    // tick keeps following either way.
    const db = trimDbFor(loudness.rms())
    applyTrim({ mode: 'auto', db: db ?? trimRef.current.db })
  }, [applyTrim, loudness])

  const setCue = useCallback(
    (on: boolean) => {
      setCueState(on)
      cueRef.current = on
      setDeckCue(deckIndex, on)
    },
    [deckIndex],
  )

  const setFx = useCallback(
    (kind: FxKind | null) => {
      // One discrete intent: the Rust set_fx/clear_fx parks the amount at
      // the kind's rest position, engine and store in the same write.
      const next = { kind, amount: kind ? fxRestPosition(kind) : 0 }
      setFxState(next)
      fxRef.current = next
      setDeckFx(deckIndex, kind)
    },
    [deckIndex],
  )

  const setFxAmount = useCallback(
    (amount: number) => {
      const next = { ...fxRef.current, amount }
      setFxState(next)
      fxRef.current = next
      setDeckFxAmount(deckIndex, amount)
    },
    [deckIndex],
  )

  const toggleLoopPad = useCallback(
    (slot: number) => {
      const channel = channelRef.current
      if (!channel || slot < 0 || slot >= LOOP_SLOT_COUNT) return
      const gesture = ++loopGestureRef.current
      const current = loopRef.current
      const slotState = current.slots[slot]
      // A pending generation owns the slot; the press waits it out.
      if (slotState.state === 'pending') return
      // A layer (loaded sample, ADR-0022) toggles independently and stacks: pressing
      // it starts/stops its own summed loop, never touching the freeze or the others.
      if (slotState.state === 'filled' && slotState.layer) {
        if (current.layering.includes(slot)) {
          channel.stopLayer(slot)
          setLoop({ ...current, layering: current.layering.filter((s) => s !== slot) })
        } else if (channel.playLoop(slot, true)) {
          setLoop({ ...current, layering: [...current.layering, slot] })
        }
        return
      }
      if (current.active === slot) {
        channel.stopLoop()
        setLoop({ ...current, active: null })
        return
      }
      if (slotState.state === 'filled') {
        if (!channel.playLoop(slot, false)) return
        // One-shots overlay and end themselves — never "active", which
        // means "replacing the live stream".
        if (!slotState.oneShot) setLoop({ ...current, active: slot })
        return
      }
      // In playback mode the worklet's history holds the dead stream —
      // a capture would loop garbage. Buffer slicing is deferred past
      // M19; until then an empty pad refuses the press (ADR-0013).
      if (modeRef.current === 'playback') return
      // One gesture: capture the just-played tail AND freeze onto it.
      // The press is refused (no state change) when too little has
      // played to loop (ADR-0009). A gated tempo snaps the length to
      // whole beats (M14) — the shell's readout, mirrored at press time.
      const gatedBpm = bpmRef.current
      const seconds =
        gatedBpm === null
          ? current.seconds
          : quantiseLoopSeconds(current.seconds, gatedBpm)
      void channel.captureLoop(slot, seconds).then((captured) => {
        if (!captured || channelRef.current !== channel) return
        if (loopGestureRef.current !== gesture) {
          // Overtaken by STOP or a newer press: drop the buffer too, so
          // the engine's slot state matches the UI's "empty".
          channel.clearLoop(slot)
          return
        }
        if (!channel.playLoop(slot, false)) return
        const latest = loopRef.current
        setLoop({
          ...latest,
          slots: withSlot(latest, slot, {
            state: 'filled',
            label: null,
            oneShot: false,
            // A freeze REPLACES (ADR-0009); only loaded samples layer.
            layer: false,
          }),
          active: slot,
        })
        // Auto-save the freeze to the generated-samples library (ADR-0022): the
        // shell reads the EXACT stored slot buffer (a freeze WAV never lives in the
        // webview) and persists it. Fire-and-forget — a save failure must never
        // disturb the loop the performer is hearing. A freeze is always a loop.
        void channel
          .saveLoopSlot(slot, {
            title: `Freeze ${deckId.toUpperCase()}`,
            prompt: null,
            model: 'freeze',
            oneShot: false,
          })
          .catch(() => {})
      })
    },
    [setLoop, deckId],
  )

  const clearLoopPad = useCallback(
    (slot: number) => {
      if (slot < 0 || slot >= LOOP_SLOT_COUNT) return
      loopGestureRef.current += 1
      slotGenerationRef.current[slot] += 1
      const channel = channelRef.current
      const current = loopRef.current
      if (current.active === slot) channel?.stopLoop()
      // clearLoop drops the engine buffer (and any active/layer it fed); mirror that
      // in the UI state for the active replace and any layer this slot was feeding.
      channel?.clearLoop(slot)
      if (current.slots[slot].state === 'empty') return
      setLoop({
        ...current,
        slots: withSlot(current, slot, EMPTY_SLOT),
        active: current.active === slot ? null : current.active,
        layering: current.layering.filter((s) => s !== slot),
      })
    },
    [setLoop],
  )

  const generateToPad = useCallback(
    (
      prompt: string,
      engine: GenerateEngine,
      oneShot: boolean,
      loras?: LoraChoice[] | null,
    ) => {
      const trimmed = prompt.trim()
      if (!trimmed) return
      const current = loopRef.current
      const slot = current.slots.findIndex((entry) => entry.state === 'empty')
      if (slot === -1) return
      // A locked tempo (M14) shapes a MUSIC-model loop on both axes:
      // whole bars and the figure in the prompt, plus the measured
      // quality floor (more bars beat broken audio). Other engines
      // take the picker's length as asked — the floor is an sm-music
      // fact, and Magenta ignores tempo text by design (ADR-0004).
      const gatedBpm = !oneShot && engine === 'music' ? bpmRef.current : null
      const seconds =
        !oneShot && engine === 'music'
          ? generatedLoopSeconds(current.seconds, gatedBpm)
          : current.seconds
      const requestPrompt =
        gatedBpm === null ? trimmed : `${trimmed}, ${Math.round(gatedBpm)} BPM`
      // Loops carry the seam surplus the engine folds away (the capture
      // convention), so the musical length survives the splice.
      const requestSeconds = oneShot ? seconds : seconds + LOOP_CROSSFADE_SECONDS
      const generation = ++slotGenerationRef.current[slot]
      setGenerateError(null)
      setLoop({
        ...current,
        slots: withSlot(current, slot, {
          state: 'pending',
          label: trimmed,
          oneShot,
        }),
      })
      const stale = () => slotGenerationRef.current[slot] !== generation
      void (async () => {
        try {
          // The channel is created on demand: pads can fill before the
          // deck has ever played (prepping weapons before the set).
          const apiBase = await getApiBaseUrl()
          const [channel, response] = await Promise.all([
            ensureChannel(),
            fetch(
              `${apiBase}${engine === 'magenta' ? '/api/render' : '/api/generate'}`,
              {
                method: 'POST',
                headers: { 'content-type': 'application/json' },
                body: JSON.stringify(
                  engine === 'magenta'
                    ? { prompt: requestPrompt, seconds: requestSeconds }
                    : {
                        prompt: requestPrompt,
                        seconds: requestSeconds,
                        kind: engine,
                        ...(loras && loras.length > 0 ? { loras } : {}),
                      },
                ),
              },
            ),
          ])
          if (!response.ok) {
            // The backend's detail names the problem (502/503 carry the
            // CLI tail or the setup hint).
            const detail = await response
              .json()
              .then((body: { detail?: string }) => body.detail)
              .catch(() => null)
            throw new Error(detail || `generation failed (${response.status})`)
          }
          const wav = await response.arrayBuffer()
          if (stale()) return
          // The engine reports whether it took the pad; a refusal is honestly
          // named, not blamed on decoding. (A loaded sample layers on any deck
          // now, so this only trips on a malformed slot — ADR-0022.)
          if (!(await channel.loadGeneratedLoop(slot, wav, oneShot))) {
            throw new Error('the deck could not load the generated pad')
          }
          // A clear landing mid-decode wins: the slot stays empty in the
          // UI and the channel's orphaned buffer waits for the next
          // capture to overwrite it.
          if (stale()) return
          const latest = loopRef.current
          setLoop({
            ...latest,
            slots: withSlot(latest, slot, {
              state: 'filled',
              label: trimmed,
              oneShot,
              // A generated pad LOOP layers like a loaded sample (ADR-0022); only a
              // freeze capture replaces. A one-shot overlays (not a layer).
              layer: !oneShot,
            }),
          })
          // Auto-save the generated pad to the samples library (ADR-0022): persist
          // the RAW backend WAV (it carries the seam surplus, so a single reload
          // fold reproduces the loop exactly — `decodeTo48k` cloned it, so `wav`
          // survives the load above). Fire-and-forget; the pad plays regardless.
          void channel
            .saveGeneratedSample(wav, {
              title: trimmed,
              prompt: trimmed,
              model: engine,
              oneShot,
            })
            .catch(() => {})
        } catch (error) {
          if (stale()) return
          const latest = loopRef.current
          setLoop({ ...latest, slots: withSlot(latest, slot, EMPTY_SLOT) })
          setGenerateError(error instanceof Error ? error.message : String(error))
        }
      })()
    },
    [setLoop, ensureChannel],
  )

  /** Load a saved sample's WAV into the first empty loop slot (ADR-0022): the
   * Samples-tab counterpart of a deck capture/generate. Reuses `generateToPad`'s
   * install tail (find a free slot → decode + fold into the slot → mark filled),
   * minus the network half. `oneShot` comes from the sample's registry row.
   * Resolves false when every slot is full or the body doesn't decode — a loaded
   * sample now layers on any deck regardless of mode (ADR-0022), surfaced via
   * `generateError`. */
  const loadSampleToSlot = useCallback(
    async (wav: ArrayBuffer, oneShot: boolean, label: string): Promise<boolean> => {
      const current = loopRef.current
      const slot = current.slots.findIndex((entry) => entry.state === 'empty')
      if (slot === -1) {
        setGenerateError('every loop slot is full — clear one to load a sample')
        return false
      }
      const generation = ++slotGenerationRef.current[slot]
      const stale = () => slotGenerationRef.current[slot] !== generation
      setGenerateError(null)
      setLoop({
        ...current,
        slots: withSlot(current, slot, { state: 'pending', label, oneShot }),
      })
      try {
        const channel = await ensureChannel()
        const accepted = await channel.loadGeneratedLoop(slot, wav, oneShot)
        if (stale()) return false
        if (!accepted) {
          const latest = loopRef.current
          setLoop({ ...latest, slots: withSlot(latest, slot, EMPTY_SLOT) })
          setGenerateError('the deck could not load the sample')
          return false
        }
        const latest = loopRef.current
        setLoop({
          ...latest,
          // A loaded sample LOOP layers over the deck (ADR-0022); a loaded one-shot
          // overlays once like any one-shot (not a layer).
          slots: withSlot(latest, slot, { state: 'filled', label, oneShot, layer: !oneShot }),
        })
        return true
      } catch (error) {
        if (!stale()) {
          const latest = loopRef.current
          setLoop({ ...latest, slots: withSlot(latest, slot, EMPTY_SLOT) })
          setGenerateError(error instanceof Error ? error.message : String(error))
        }
        return false
      }
    },
    [setLoop, ensureChannel],
  )

  const setLoopSeconds = useCallback(
    (seconds: number) => {
      setLoop({ ...loopRef.current, seconds })
      updateDeckSettings(deckId, { loopSeconds: seconds })
    },
    [deckId, setLoop],
  )

  const setEqBand = useCallback(
    (band: EqBand, value: number) => {
      const next = { ...eqRef.current, [band]: value }
      eqRef.current = next
      setEqState(next)
      setDeckEq(deckIndex, band, value)
    },
    [deckIndex],
  )

  return {
    state,
    volume,
    eq,
    cue,
    setCue,
    fx,
    setFx,
    setFxAmount,
    loop,
    toggleLoopPad,
    clearLoopPad,
    setLoopSeconds,
    generateToPad,
    loadSampleToSlot,
    generateError,
    bpm,
    captureStyleSample,
    mode,
    track,
    loadTrack,
    leavePlayback,
    seekTrack,
    nudgeTrack,
    setTrackRate,
    nudgeTrackPhase,
    syncTrack,
    hotCuePad,
    clearHotCue,
    loopIn,
    loopOut,
    loopExit,
    beatLoop,
    halveLoop,
    doubleLoop,
    getTrackBeat,
    getLiveBeat,
    getZoomSource,
    getTrackPeaks,
    trim,
    setTrimDb,
    enableAutoTrim,
    primed,
    prime,
    play,
    stop,
    setModel,
    restartWorker,
    setVolume,
    setEqBand,
    getChannelLevel,
  }
}
