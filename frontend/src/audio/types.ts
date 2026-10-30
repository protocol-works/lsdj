/** The shared audio contract — deck/mixer types + a few constants the whole app
 * types against. Extracted from the Web Audio engine so the native engine adapter
 * (`nativeEngine.ts`) and the UI/deck/control layers depend on the interface, not
 * the implementation. The Web Audio engine that originally defined these is gone
 * (the app runs only under Tauri now); the Rust engine implements `AudioEngine`
 * over IPC. */

import type { EqBand } from './eq'
import type { FxKind } from './fx'
import type { TrackLoop } from './track'

export type DeckId = 'a' | 'b'

/** Where a deck track loads from (ADR-0030): a scoped reference the shell
 * resolves, reads, decodes, and analyses — decoded PCM never crosses the IPC
 * boundary. The in-memory compose take is the one bytes case (a WAV container
 * that may not be on disk yet). */
export type TrackSource =
  | { kind: 'folder'; dir: string; name: string }
  | { kind: 'song'; name: string }
  | { kind: 'bytes'; wav: ArrayBuffer }

/** The offline beatgrid verdict (M20, ADR-0014) — computed shell-side at load
 * (ADR-0030): tempo plus where beat 0 falls, in seconds from the track start.
 * Null when the track refused a grid (no grid beats a wrong grid). */
export type Beatgrid = {
  bpm: number
  firstBeatSeconds: number
}

/** What a track load returns (ADR-0030): the shell's offline analysis —
 * duration, the honesty-gated tempo (grid-refined when a grid exists), the
 * grid, the band profile for the zoom strip, and the overview peaks at the
 * requested resolution. */
export type LoadedTrack = {
  duration: number
  bpm: number | null
  grid: Beatgrid | null
  bands: { low: Float32Array; mid: Float32Array; high: Float32Array }
  peaks: { min: Float32Array; max: Float32Array } | null
}

/** Metadata persisted with a generated sample / freeze capture (ADR-0022) — the
 * Rust `samples::NewSample`. `prompt`/`model` are null for a freeze (no prompt; the
 * model column reads "freeze"); `oneShot` is the loop-vs-one-shot verdict reload
 * uses to put the sample back into a slot the way it was made. */
export type SampleMeta = {
  title: string
  prompt: string | null
  model: string | null
  oneShot: boolean
}

export type DeckChannel = {
  postPcm: (samples: Float32Array) => void
  reset: () => void
  setVolume: (volume: number) => void
  setEq: (band: EqBand, value: number) => void
  /** Feed this channel (post-EQ, pre-fader) into the headphone cue bus. */
  setCue: (on: boolean) => void
  /** Swap the Color FX insert's effect; null removes it (M12). */
  setFx: (kind: FxKind | null) => void
  /** Knob position for the active effect; inside the effect's dead zone
   * the insert is bit-transparently bypassed (ADR-0008). */
  setFxAmount: (amount: number) => void
  /** Stream discontinuity (play / prime / stop / mode switch): reset the
   * shell's live beat analysis for this deck (ADR-0025 — estimates never
   * span streams). Sidecar-origin discontinuities reset shell-side. */
  resetAnalysis: () => void
  /** Off-air mutes the channel's master feed only — generation, meters,
   * and the cue tap stay live. The primed-deck state (M10). */
  setOnAir: (on: boolean) => void
  /** Gain-staging trim at the chain head (M17): live stream and
   * freeze loops alike, pre-EQ so kills stay the performer's move. */
  setTrim: (db: number) => void
  /** Freeze pads (M13, ADR-0009): capture the just-played tail into a
   * slot. Resolves false when too little has been played to loop. */
  captureLoop: (slot: number, seconds: number) => Promise<boolean>
  /** Generated pads (M18, ADR-0012): decode a WAV into a slot. Loops
   * arrive with a surplus seam tail and get the capture treatment
   * (fold + wrap); one-shots are stored as-is. False on a bad slot or
   * an undecodable body. */
  loadGeneratedLoop: (
    slot: number,
    wav: ArrayBuffer,
    oneShot: boolean,
  ) => Promise<boolean>
  /** Play a filled slot's loop (false when the slot is empty). `layer`
   * picks how it plays: false REPLACES the live stream (a freeze — the
   * stream keeps running, muted, so stopLoop returns to fresh material
   * instantly); true LAYERS it on top, summed and stacking with other
   * layers (a loaded sample, ADR-0022). One-shot slots always overlay
   * (sum once) regardless of `layer`. */
  playLoop: (slot: number, layer: boolean) => boolean
  /** Return the active (replacing) freeze to the base; layers untouched. */
  stopLoop: () => void
  /** Stop one layered loop (ADR-0022): the slot stops summing; its
   * buffer stays for a re-press. */
  stopLayer: (slot: number) => void
  /** Cut a ringing one-shot (deck STOP silences everything). */
  stopOneShot: () => void
  clearLoop: (slot: number) => void
  /** Style sampling (M15): the just-played tail as wire-format PCM,
   * or null when too little has played to embed meaningfully. */
  captureSample: (seconds: number) => Promise<Float32Array<ArrayBuffer> | null>
  /** Persist a freeze capture in `slot` to the generated-samples library
   * (ADR-0022). The shell reads the EXACT stored slot buffer (the freeze WAV
   * never lives here), encodes it, and records it. Auto-save: fire-and-forget. */
  saveLoopSlot: (slot: number, meta: SampleMeta) => Promise<void>
  /** Persist a generated pad's WAV (already in hand) to the generated-samples
   * library. Same binary frame as the songs save. Auto-save: fire-and-forget. */
  saveGeneratedSample: (wav: ArrayBuffer, meta: SampleMeta) => Promise<void>
  /** Playback mode (M19, ADR-0013/0030): load a track by scoped reference —
   * the shell reads, decodes, resamples, and runs the offline analyses; the
   * webview gets back numbers and summaries, never PCM. `peaksBuckets` is
   * the overview resolution to prefetch so `getTrackPeaks` stays
   * synchronous. A refusal (unsupported codec, oversize) REJECTS with the
   * shell's reason — an explicit load error. Replaces any previous track. */
  loadTrack: (source: TrackSource, peaksBuckets: number) => Promise<LoadedTrack | null>
  /** The loom's seamless seam (architecture §3): decode + analyse a library
   * track like `loadTrack` and queue it to take over this deck's track when
   * the playhead reaches `switchAtSeconds`, entering it at `startSeconds`.
   * Resolves with the queued track's numbers; its bands and peaks arrive
   * with `promoteQueuedTrack`. A `bytes` source or a refusal REJECTS.
   * Replaces any earlier queue; a load discards it. */
  queueTrack: (
    source: TrackSource,
    startSeconds: number,
    switchAtSeconds: number,
  ) => Promise<Pick<LoadedTrack, 'duration' | 'bpm' | 'grid'>>
  clearQueuedTrack: () => void
  /** Install the queued track's analysis once the engine swapped (`swaps`
   * rose): resolves with its band profile (null when unavailable) after the
   * overview peaks are refreshed for the new buffer. */
  promoteQueuedTrack: () => Promise<LoadedTrack['bands'] | null>
  /** Start (or resume) the track; from the ended state it restarts at
   * the top. False with no track loaded. */
  playTrack: () => boolean
  /** Park the playhead where it is. */
  pauseTrack: () => void
  /** Jump the playhead; whether it was playing is preserved. Exits
   * an active loop (ADR-0015). */
  seekTrack: (seconds: number) => void
  /** Loop region on the source's native loop (M21, ADR-0015):
   * survives pause and rate changes; any seek exits it. Quantise is
   * the caller's job; the boundary refuses regions that can't loop. */
  setTrackLoop: (start: number, end: number) => void
  clearTrackLoop: () => void
  /** Playhead snapshot, or null with no track loaded. The end is an
   * explicit state: silence with the position parked (ADR-0013).
   * `rate` is the base varispeed rate (M20), bends excluded; the
   * position folds through an active loop (M21). */
  getTrackStatus: () => {
    position: number
    duration: number
    playing: boolean
    ended: boolean
    rate: number
    loop: TrackLoop | null
    /** Context time of this snapshot — lets callers convert the
     * track-domain playhead into the shared audio clock (M20). */
    contextTime: number
    /** A queued track is waiting for its switch point. */
    queued: boolean
    /** Queued hand-overs since the load: a rise means the queued track
     * now plays. */
    swaps: number
  } | null
  /** Varispeed (M20, ADR-0014): the playback rate the tempo control
   * set; the transport re-anchors so the playhead stays exact. */
  setTrackRate: (rate: number) => void
  /** Phase nudge: slip the playhead by `seconds` via a stepped rate
   * bend (the platter-drag metaphor) — never a click. Playing only. */
  nudgeTrackPhase: (seconds: number) => void
  /** Static min/max envelope of the loaded track for the overview. */
  getTrackPeaks: (
    buckets: number,
  ) => { min: Float32Array; max: Float32Array } | null
  /** Drop the track — leaving playback mode frees the buffer. */
  unloadTrack: () => void
  /** Post-fader RMS level, 0..~1 (for channel meters). */
  getLevel: () => number
  dispose: () => void
}

/** An output device the native engine can open. `cueCapable` is `channels >= 4`
 * — only a ≥4-channel device (e.g. the FLX4) can carry the headphone cue on
 * channels 3/4 of the SAME device as the master ("same as main" / combined
 * mode). Any device can carry the cue as a separate cue device (split mode). */
export type OutputDevice = {
  name: string
  channels: number
  cueCapable: boolean
}

export type StatsHandler = (stats: {
  underruns: number
  bufferedSeconds: number
  playing: boolean
  /** Cumulative frames consumed since the last reset, and the
   * worklet clock at snapshot time — the played-index anchor the
   * beat clock extrapolates from (M20, ADR-0014). */
  playedFrames: number
  contextTime: number
}) => void

export type AudioEngine = {
  /** The shared context clock, or null before the bus exists — the
   * zoom view extrapolates played positions in this domain (M22). */
  getContextTime: () => number | null
  createDeckChannel: (
    deckId: DeckId,
    initial: {
      volume: number
      eq: Record<EqBand, number>
      cue: boolean
      fx: { kind: FxKind | null; amount: number }
      trimDb: number
    },
    onStats: StatsHandler,
  ) => Promise<DeckChannel>
  resume: () => Promise<void>
  setCrossfade: (position: number) => void
  /** Cue/master blend for the headphone feed: 0 = cue only, 1 = master. */
  setCueMix: (position: number) => void
  /** Preview a decoded WAV into the headphone/cue feed before loading it onto a
   * deck (ADR-0027): heard in the phones only, never the master, looping until
   * stopped. Replaces any current preview. */
  auditionPlay: (wav: ArrayBuffer) => Promise<void>
  /** Stop the headphone preview (ADR-0027). */
  auditionStop: () => void
  /** The output devices the engine can open, with their channel counts —
   * a ≥4-channel device can carry the combined cue on channels 3/4. */
  listOutputDevices: () => Promise<OutputDevice[]>
  /** Switch the engine's MAIN output device by name (empty = system default;
   * master → its channels 1/2). Rejects (audio left undisturbed) when the
   * device can't be opened. */
  setMainDevice: (name: string) => Promise<void>
  /** Switch the headphone CUE output device by name. Empty = "same as main"
   * (the FLX4 phones jack on channels 3/4); a different name routes the cue to
   * a second device. Rejects (audio left undisturbed) when it can't be opened. */
  setCueDevice: (name: string) => Promise<void>
  /** Master-bus RMS level, 0..~1 (what the speakers get). */
  getMasterLevel: () => number
  /** The limiter's current gain reduction in dB (≤ 0); 0 when idle. */
  getMasterGainReduction: () => number
  /** Start capturing the master bus (exactly the speaker feed), streaming it to a
   *  WAV in `folder` (empty = the OS Downloads folder, the default). The file is
   *  opened now, so the path is minted at start and resolved here; `name` is the
   *  filename stem — sanitised to one path component server-side. Streaming to disk
   *  means the take is bounded by free space, not RAM (no length limit). */
  startRecording: (folder: string, name: string) => Promise<string>
  /** Stop capturing: flush the take and close the file opened at start. */
  stopRecording: () => Promise<void>
}

/** The wire and graph rate end to end (backend workers generate 48 k). */
export const SAMPLE_RATE = 48_000

/** Centre position; the single source for App state and the bus default. */
export const INITIAL_CROSSFADE = 0.5

/** Even cue/master blend in the phones; same role as INITIAL_CROSSFADE. */
export const INITIAL_CUE_MIX = 0.5
