/** The native (Tauri) AudioEngine: the SAME `AudioEngine` / `DeckChannel`
 * interface as the Web Audio engine (`engine.ts`), but every control call is a
 * Tauri `invoke` to the Rust audio engine, and every synchronous getter
 * (`getLevel`, `getTrackStatus`, `getMasterLevel`, `getContextTime`, …) reads a
 * per-frame snapshot the poller caches (ADR-0017/0018, the Phase 2 part 3 swap).
 *
 * # Why a cache
 *
 * The interface has many *synchronous* getters the UI calls every animation
 * frame, but IPC is asynchronous. So a single poller invokes one consolidated
 * `engine_snapshot` command per `requestAnimationFrame` and stores the result;
 * the getters serve from that cache. `playLoop`'s synchronous boolean — the only
 * control return the UI consumes synchronously — is answered from the cached slot
 * state (the engine returns false iff the slot is empty, which the cache knows).
 *
 * # What moved, what stayed
 *
 * - Model PCM no longer flows through the UI: the sidecar feeds the engine
 *   directly (part 4), so `postPcm` is a no-op here. The PCM tap re-feeds the
 *   TS loudness/band-scroller visuals; beat analysis lives in the shell
 *   (ADR-0025), read back through the interface store.
 * - Tracks load by scoped reference (ADR-0030): the shell decodes + analyses;
 *   `loadTrack` returns numbers and summaries. `getTrackPeaks` serves the
 *   overview envelope prefetched from `track_peaks` at load (sync getter, one
 *   IPC per load); the synced dub echo's clock is driven entirely shell-side.
 * - Cue routing is native: the engine derives the headphone feed and routes it
 *   to the output device's channels 3/4, so the webview only sends the live
 *   controls (`setCue`/`setCueMix`); `nudgeTrackPhase` (the jog-while-playing
 *   platter bend) is wired to the engine's `nudge_track_phase` rate bend. */

import type { EqBand } from './eq'
import {
  SAMPLE_RATE,
  type AudioEngine,
  type DeckChannel,
  type DeckId,
  type LoadedTrack,
  type OutputDevice,
  type StatsHandler,
} from './types'
import type { FxKind } from './fx'
import { interleaveChannels } from './styleSample'

/** Minimal shape of the `withGlobalTauri` global we use (core `invoke`). */
type TauriGlobal = {
  core: {
    invoke: <T>(cmd: string, args?: unknown, options?: unknown) => Promise<T>
  }
}

function tauriGlobal(): TauriGlobal | null {
  const g = globalThis as { __TAURI__?: TauriGlobal }
  return g.__TAURI__ ?? null
}

/** Whether the native (Tauri) IPC bridge is present. False in a plain browser (dev
 * without the shell), where native-only actions like writing a song to disk can't
 * run — callers skip them rather than surfacing an avoidable error. */
export function isTauri(): boolean {
  return tauriGlobal() !== null
}

let apiBaseUrlPromise: Promise<string> | null = null

/** Base URL for the backend `/api/*` generation endpoints (sa3/Magenta pad+track
 * render). FastAPI no longer serves the UI, so the Rust shell runs a generation
 * server on a loopback port it reports via `app_info`; the webview fetches
 * `http://127.0.0.1:<port>/api/...`. Resolved once and cached; falls back to ''
 * (relative) if the port can't be resolved. */
export function getApiBaseUrl(): Promise<string> {
  if (!apiBaseUrlPromise) {
    apiBaseUrlPromise = invoke<{ generationPort: number | null }>('app_info')
      .then((info) => (info.generationPort ? `http://127.0.0.1:${info.generationPort}` : ''))
      .catch(() => '')
  }
  return apiBaseUrlPromise
}

/** The native MCP server endpoint + bearer token (ADR-0020 Phase 2), reported by
 * `app_info`. The server is always on; `port` is null only if the loopback bind
 * failed. The Settings drawer surfaces these so a client can be pointed at the URL. */
export type McpInfo = { port: number | null; token: string | null }

export function getMcpInfo(): Promise<McpInfo> {
  return invoke<{ mcpPort: number | null; mcpToken: string | null }>('app_info')
    .then((info) => ({ port: info.mcpPort ?? null, token: info.mcpToken ?? null }))
    .catch(() => ({ port: null, token: null }))
}

/** Mint a new MCP bearer token, persist it, and swap it in live (the Settings
 * "Rotate token" button) — invalidating a leaked token without an app restart.
 * Resolves to the new token. */
export function rotateMcpToken(): Promise<string> {
  return invoke<string>('rotate_mcp_token')
}

/** Set + persist the MCP server's loopback port and restart it on that port (the
 * Settings port field). Resolves to the new port; rejects if it can't be bound (e.g.
 * the port is already taken), leaving the running server untouched. */
export function setMcpPort(port: number): Promise<number> {
  return invoke<number>('set_mcp_port', { port })
}

/** Fire a command at the Rust engine (or a Tauri plugin, e.g. `plugin:dialog|open`).
 * Rejects (caught by callers that care) when the IPC bridge is absent — never
 * throws synchronously. Exported for the few non-engine native callers
 * (MediaExplorer's folder picker). */
export function invoke<T = void>(cmd: string, args?: unknown, options?: unknown): Promise<T> {
  const g = tauriGlobal()
  if (!g) return Promise.reject(new Error('Tauri IPC unavailable'))
  return g.core.invoke<T>(cmd, args, options)
}

/** Minimal shape of the Tauri event API we use (`event.listen`). */
type TauriEventApi = {
  listen: (
    event: string,
    handler: (e: { payload: unknown }) => void,
  ) => Promise<() => void>
}

/** The library a `library://changed` event names (Rust `watcher::LibraryChanged`). */
export type LibraryKind = 'songs' | 'samples'

/** Subscribe to the Rust folder watcher's `library://changed` event (ADR-0022): the
 * named library's folder changed out-of-band — a deck auto-saved a sample, a file
 * was dropped in or deleted by hand — so the matching Media Explorer tab should
 * re-list. Returns an unsubscribe fn (safe to call before the async `listen`
 * resolves). A no-op outside Tauri or without the event bridge (so tests that stub
 * only `core` are unaffected). */
export function subscribeLibraryChanged(
  onChange: (library: LibraryKind) => void,
): () => void {
  return listenTo<{ library?: LibraryKind }>('library://changed', (payload) => {
    if (payload.library === 'songs' || payload.library === 'samples') onChange(payload.library)
  })
}

// --- Model manager (issue #43) ---------------------------------------------

/** A model family the manager installs (Rust `models::Family`). */
export type ModelFamily = 'magenta' | 'sa3' | 'lora'

/** One installed Magenta model in `model_status` (serde camelCase). */
export type InstalledModel = {
  name: string
  sizeBytes: number
  /** Files present but the shared resources a load needs are not. */
  needsResources: boolean
}

/** SA3's four readiness states (Rust `models`/`sa3.readiness`). */
export type Sa3State = 'missing' | 'venv_missing' | 'not_warmed' | 'ready'

/** The source an SA3 checkout was installed from / is pinned to (Rust
 * `models::Sa3Source`): the `sa3-pin.json` repo + commit. */
export type Sa3Source = { repo: string; commit: string }

/** The DiT family an SA3 LoRA adapter rides (issue #66): the 1024-wide small
 * DiTs (the sfx/music pad kinds) or the 1536-wide medium track DiT. */
export type LoraBase = 'small' | 'medium'

/** One installed SA3 LoRA adapter (Rust `loras::LoraInfo`). `name` is the
 * `<base>/<slug>` id a generate request sends to the backend. */
export type LoraAdapter = {
  name: string
  base: LoraBase
  slug: string
  sizeBytes: number
  /** Import-manifest facts; null for a hand-placed adapter. */
  source: string | null
  adapterType: string | null
  rank: number | null
}

/** The model-manager status for both families (Rust `models::ModelStatus`). */
export type ModelStatus = {
  magenta: {
    modelsDir: string
    resourcesPresent: boolean
    installable: string[]
    installed: InstalledModel[]
  }
  sa3: {
    state: Sa3State
    sizeBytes: number
    checkout: string | null
    /** What the installed checkout was fetched from (`null` when unstamped). */
    installedSource: Sa3Source | null
    /** What's currently pinned. */
    pinnedSource: Sa3Source
    /** The installed checkout differs from the pin (or is unstamped) — offer an update. */
    updateAvailable: boolean
  }
  /** The installed SA3 LoRA adapters (issue #66), sorted by name. */
  loras: LoraAdapter[]
  /** The in-flight install, so the manager reflects it after a close/reopen even
   * though the live `model://progress` events were missed while unmounted. */
  installing: { family: ModelFamily; name: string } | null
}

/** A live install-progress event (Rust `models::ModelProgress`). */
export type ModelProgress = {
  family: ModelFamily
  name: string
  stage: string
  message: string | null
  file: string | null
}

function eventApi(): TauriEventApi | null {
  return (globalThis as { __TAURI__?: { event?: TauriEventApi } }).__TAURI__?.event ?? null
}

/** Subscribe to a Tauri event; a no-op (and safe unsubscribe) without the event
 * bridge, so tests that stub only `core` are unaffected. */
function listenTo<T>(event: string, onEvent: (payload: T) => void): () => void {
  const ev = eventApi()
  if (!ev) return () => {}
  let unlisten: (() => void) | null = null
  let cancelled = false
  void ev.listen(event, (e) => onEvent(e.payload as T)).then((un) => {
    if (cancelled) un()
    else unlisten = un
  })
  return () => {
    cancelled = true
    unlisten?.()
  }
}

/** Fetch the model-manager status (both families). */
export function modelStatus(): Promise<ModelStatus> {
  return invoke<ModelStatus>('model_status')
}

/** Start installing a model (Magenta needs `name`; SA3 ignores it). Resolves once
 * the install has STARTED; progress arrives via `subscribeModelProgress` and a
 * final `models://changed`. */
export function installModel(family: ModelFamily, name?: string): Promise<void> {
  return invoke('install_model', { family, name: name ?? null })
}

/** Update a family in place to the pinned source (SA3: re-fetch the pinned
 * checkout, rebuild, re-warm). Progress arrives like an install. */
export function updateModel(family: ModelFamily): Promise<void> {
  return invoke('update_model', { family })
}

/** Cancel the in-flight install (kills the child + cleans partials). */
export function cancelInstall(): Promise<void> {
  return invoke('cancel_install')
}

/** Reveal a family's folder in the OS file manager (for inspecting or removing
 * models natively — the watcher reflects a native delete live). Magenta opens its
 * models dir; SA3 opens its checkout. */
export function openModelFolder(family: ModelFamily): Promise<void> {
  return invoke('open_model_folder', { family })
}

/** Import an SA3 LoRA adapter (issue #66) from a HuggingFace repo id or a local
 * path (`.safetensors` file or PEFT adapter folder). `base` is only needed for
 * adapters whose base cannot be inferred (rank-only `-xs` shapes). Resolves once
 * the import has STARTED; progress arrives via `subscribeModelProgress` (family
 * `lora`) and a final `models://changed`. */
export function installLora(
  source: { hfRepo: string } | { path: string },
  base?: LoraBase,
): Promise<void> {
  return invoke('install_lora', { spec: { ...source, base: base ?? null } })
}

/** Delete an installed adapter (small, re-downloadable — unlike the model
 * families, adapters get an in-app delete). Emits `models://changed`. */
export function deleteLora(name: string): Promise<void> {
  return invoke('delete_lora', { name })
}

/** Subscribe to `models://changed` (the models-dir watcher / an install finishing):
 * re-fetch `model_status` and the deck picker's `/api/models`. */
export function subscribeModelsChanged(onChange: () => void): () => void {
  return listenTo<unknown>('models://changed', () => onChange())
}

/** Subscribe to live install progress (`model://progress`). */
export function subscribeModelProgress(onProgress: (p: ModelProgress) => void): () => void {
  return listenTo<ModelProgress>('model://progress', onProgress)
}

// --- The interface-state store (ADR-0020, issue #37 Phase 1) ---
//
// Rust owns the semantic/audio-param interface state; the webview projects it.
// These mirror the Rust `store::InterfaceState` serde shape (camelCase). The FX
// kind is the camelCase wire value (`dubEcho`), matching `FX_ARG`.

/** A Color FX kind as it appears in the store snapshot (the camelCase wire value). */
export type FxKindSnap = 'filter' | 'dubEcho' | 'space' | 'crush' | 'noise' | 'sweep'

/** One deck's state in the store: the mixer channel plus the realtime read-backs
 * the store mirrors (model / playing). */
export type DeckSnap = {
  volume: number
  eq: { low: number; mid: number; high: number }
  trimDb: number
  cue: boolean
  onAir: boolean
  fx: { kind: FxKindSnap | null; amount: number }
  /** The realtime deck's loaded model (a sidecar read-back the store mirrors). */
  model: string | null
  /** Whether the realtime deck is generating (a derived read-back the store mirrors). */
  playing: boolean
  /** Which source the deck plays (M19): the realtime model stream or a loaded
   * track. Written by the load flow's mirror; agents read it natively. */
  mode: 'realtime' | 'playback'
  /** Hot-cue points on the loaded track in track seconds, one per pad (empty with
   * no track). The store OWNS them (phase D): pads reset with the track identity,
   * mutate through setDeckCuePoint / the MCP cue tools, and project down. */
  cues: (number | null)[]
  /** The loaded track's identity on a playback deck (a read-back the store
   * mirrors), or null on a realtime deck / with no track. */
  track: { title: string; bpm: number | null; durationSeconds: number } | null
  /** The playback deck's live transport (playhead / rate / loop) — a throttled
   * read-back the webview mirrors up, null on a realtime deck / with no track. */
  transport: {
    playheadSeconds: number
    rate: number
    loopRegion: { startSeconds: number; endSeconds: number } | null
  } | null
  /** Freeze/sample loop-slot labels, one per pad (null for an empty/unlabelled
   * slot) — a read-back the store mirrors. */
  loopLabels: (string | null)[]
  /** The realtime deck's 2D style-pad targets (prompt + position). The store
   * OWNS the arrangement (ADR-0020 phase B) — the webview projects it and
   * mutates through the style_* intents. A sampled chip (M15) carries its
   * session embedding key. */
  styleTargets: { x: number; y: number; text: string; sample?: string }[]
  /** Which style targets are in the active blend (the net mask, one bool per
   * target; empty = no mask) — mirrored up for the native pad LEDs (ADR-0031). */
  styleSelected: boolean[]
  /** The 2D style-pad cursor (the blend point). */
  cursor: { x: number; y: number }
  /** Whether the deck is primed off-air (the transport-CUE LED state) — a
   * read-back the webview mirrors up. */
  primed: boolean
  /** The performance-surface config (issue #48): armed decks take pad/keyboard
   * notes and run the small ADR-0023 chunk. Written through the shell
   * note-steering service; the webview projects it. */
  performance: {
    armed: boolean
    key: number
    scale: 'major' | 'minor' | 'pentatonicMinor' | 'chromatic'
    mode: 'chord' | 'onset'
  }
  /** The realtime deck's note steering (ADR-0023) — held pitches + mode, or
   * null when unsteered. Authored by the shell note-steering service
   * (hardware pads/keyboard, MCP); cleared on transport transitions. */
  notes: { pitches: number[]; mode: 'chord' | 'onset' } | null
  /** Drum conditioning (ADR-0023): null = the model decides, false = suppress
   * drums ("sit beside"). The product is binary (suppress vs auto); `true`
   * (force) is a valid model flag no LSDJ surface emits. Unlike `notes` this
   * is deck config (issue #50): it survives transport transitions — the shell
   * re-asserts it to the worker on the play edge. */
  drums: boolean | null
  /** The drum-conditioning strength (issue #50): the `cfg_drums` guidance
   * scale the worker applies every chunk regardless of `drums` (like the
   * reference). Deck config like `drums`; the shell defaults it to the
   * measured sweet spot. */
  drumsStrength: number
  /** The deck's live generation operating point (issue #84): the tunable
   * sampling/guidance params (the reference `magenta-realtime` knobs). Deck
   * config that persists; the shell re-sends it to a fresh worker on `ready`. */
  generation: {
    temperature: number
    topK: number
    cfgMusiccoca: number
    cfgNotes: number
  }
  /** The deck's live beat analysis (ADR-0025), written by the shell's analysis
   * thread at most ~once per second. `bpm` is the honesty-gated readout (null =
   * blank, the feature); `liveBeat` the phase clock (anchor in pushed frames
   * since the stream reset, paired with its tempo); `originFrames` the engine
   * context-frame origin captured at that reset — the mapping from the anchor's
   * pushed-frame domain onto engine time. */
  analysis: {
    bpm: number | null
    confidence: number
    liveBeat: { anchorFrame: number; bpm: number } | null
    originFrames: number
  }
  /** The worker crashed / is reloading — shell-written from the status relay
   * (ADR-0020 phase A), so an agent sees deck health without the webview. */
  workerDied: boolean
  switchingModel: boolean
  /** The deck's hardware SHIFT held-state, written by the native translator
   * (the origin). */
  shiftHeld: boolean
}

/** The authoritative interface state the webview projects (mirrors Rust
 * `store::InterfaceState`). View state is deliberately absent — it stays in React
 * (the ADR-0020 narrowing). */
export type InterfaceState = {
  decks: DeckSnap[]
  crossfade: number
  cueMix: number
  /** The shell recorder's state — written by the recording commands, so a
   * reload (or an agent) reads the truth instead of a local flag. */
  recording: { active: boolean; path: string | null }
  /** Shell-persisted settings (ADR-0020 phase A): the pickers project these;
   * the device/folder commands persist them Rust-side. */
  mainDevice: string
  cueDevice: string
  recordingsFolder: string
  /** Whether the standalone MIDI-keyboard window (issue #49) is open — a shell
   * window-lifecycle read-back the drawer's toggle reflects. Optional so test
   * fixtures need not set it; the shell snapshot always carries it. */
  pianoWindowOpen?: boolean
}

/** Fetch the current interface-state snapshot (the projection's initial hydrate). */
export function storeSnapshot(): Promise<InterfaceState> {
  return invoke<InterfaceState>('store_snapshot')
}

/** Subscribe to `store://changed` — the store emits the fresh snapshot on every
 * mutation (from any controller: UI, MIDI, or a future MCP agent). Returns an
 * unsubscribe fn. */
export function subscribeStoreChanged(onChange: (state: InterfaceState) => void): () => void {
  return listenTo<InterfaceState>('store://changed', onChange)
}

/** An MCP agent's `load_track` (Rust emits `mcp://load-track`). The webview owns the
 * load-state orchestration (deck mode is React state until ADR-0020's store owns
 * it), so it runs the deck's load flow by reference — the shell decodes and
 * analyses (ADR-0030); no bytes round-trip. */
export function subscribeLoadTrack(
  onLoad: (payload: { deck: number; file: string; title: string }) => void,
): () => void {
  return listenTo('mcp://load-track', onLoad)
}

/** An MCP agent's `load_sample` (Rust emits `mcp://load-sample`); the webview installs
 * it into the deck's pad bank. */
export function subscribeLoadSample(
  onLoad: (payload: {
    deck: number
    file: string
    oneShot: boolean
    label: string
  }) => void,
): () => void {
  return listenTo('mcp://load-sample', onLoad)
}

/** An MCP agent's track-transport gesture (Rust emits `mcp://deck-command`): the
 * webview runs the deck's own method (seek / rate / sync / beatloop) so its state and
 * the UI follow. `value` is null for argument-less commands (sync). */
export function subscribeDeckCommand(
  onCommand: (payload: { deck: number; command: string; value: number | null }) => void,
): () => void {
  return listenTo('mcp://deck-command', onCommand)
}

/** Mirror a realtime deck's model read-back into the store. The webview derives it
 * from worker status ('ready'/'model_loading') and writes the current value up — no
 * engine effect. `playing` is NOT mirrored: the store owns the transport
 * (deck_play/deck_stop + the Rust status relay) and the webview only projects it.
 * Fire-and-forget (a dropped mirror write must never surface as a rejection). */
export function setDeckModel(deck: number, model: string | null): void {
  void invoke('set_deck_model', { deck, model }).catch(() => {})
}

/** Set or clear one hot-cue pad (ADR-0020 phase D): a store intent — the UI
 * computes the snapped position, the store owns the points, the pads project
 * the snapshot. Fire-and-forget. */
export function setDeckCuePoint(
  deck: number,
  index: number,
  seconds: number | null,
): void {
  void invoke('set_deck_cue_point', { deck, index, seconds }).catch(() => {})
}

/** Record which source a deck plays (M19, phase D): the load flow writes it so
 * an agent sees playback vs realtime natively. Fire-and-forget. */
export function setDeckMode(deck: number, mode: 'realtime' | 'playback'): void {
  void invoke('set_deck_mode', { deck, mode }).catch(() => {})
}

/** Mirror a playback deck's live transport (playhead / rate / loop) into the store
 * (null clears it on unload / a realtime deck). A read-back the webview writes up at
 * a throttled cadence — the playhead moves every audio frame; no engine effect.
 * Fire-and-forget. */
export function setDeckTransport(
  deck: number,
  transport: {
    playheadSeconds: number
    rate: number
    loopRegion: { startSeconds: number; endSeconds: number } | null
  } | null,
): void {
  void invoke('set_deck_transport', { deck, transport }).catch(() => {})
}

/** Mirror a playback deck's loaded-track identity into the store (null clears it).
 * A read-back the webview writes up; no engine effect. Fire-and-forget. */
export function setDeckTrack(
  deck: number,
  track: { title: string; bpm: number | null; durationSeconds: number } | null,
): void {
  void invoke('set_deck_track', { deck, track }).catch(() => {})
}

/** Mirror a deck's freeze/sample loop-slot labels into the store. A read-back the
 * webview writes up when its slots change. Fire-and-forget. */
export function setDeckLoopLabels(deck: number, labels: (string | null)[]): void {
  void invoke('set_deck_loop_labels', { deck, labels }).catch(() => {})
}

/** Mirror the primed-off-air read-back into the store (the transport-CUE LED
 * state, read by the native LED painter — ADR-0031). Fire-and-forget. */
export function setDeckPrimed(deck: number, primed: boolean): void {
  void invoke('set_deck_primed', { deck, primed }).catch(() => {})
}

/** Set (and shell-persist) the recordings folder — "" = Downloads. */
export function setRecordingsFolder(folder: string): void {
  void invoke('set_recordings_folder', { folder }).catch(() => {})
}

/** Set a deck's performance-surface config (issue #48): arm/disarm, key,
 * scale, note mode. Routed through the shell note-steering service — the
 * same single sender the hardware uses; arming also applies the ADR-0023
 * chunk knob. Fire-and-forget; the store projection reflects it. */
export function setDeckPerformance(
  deck: number,
  perf: DeckSnap['performance'],
): void {
  void invoke('set_deck_performance', { deck, perf }).catch(() => {})
}

/** Play one note from the on-screen keyboard (issue #49): a raw MIDI pitch and
 * an on/off edge, scoped to one deck. The shell note-steering service snaps it
 * to the deck's key/scale and holds it on the surface's own ledger. It does NOT
 * arm the deck — routing is independent of the MIDI-steering switch; the note
 * conditions generation either way (arm steering for the tighter chunk).
 * Fire-and-forget; the held state is the local surface's, the sound the store's. */
export function deckKeyboardNote(
  deck: number,
  pitch: number,
  down: boolean,
): void {
  void invoke('deck_keyboard_note', { deck, pitch, down }).catch(() => {})
}

/** Show / hide the standalone MIDI-keyboard window (issue #49), creating it on
 * the first call. The shell mirrors its visibility into the store, so the
 * drawer's toggle reflects it. Fire-and-forget. */
export function togglePianoWindow(): void {
  void invoke('toggle_piano_window').catch(() => {})
}

/** The drum-conditioning vocabulary the IPC boundary speaks (issue #50) —
 * mirrors the shell's `DrumModeArg` and the MCP tool. Binary (suppress vs
 * auto), matching the magenta-realtime reference's `drumless` toggle. */
export type DrumMode = 'suppress' | 'auto'

/** Set a deck's drum conditioning (issue #50): suppress ("sit beside") or
 * auto (the model decides). Routed through the shell
 * note-steering service — the same single sender the MCP tool uses; the
 * authored state sticks across play/stop (re-asserted on the play edge).
 * Fire-and-forget; the store projection (`drums`) reflects it. */
export function setDeckDrums(deck: number, mode: DrumMode): void {
  void invoke('set_deck_drums', { deck, mode }).catch(() => {})
}

/** Set a deck's drum-conditioning strength (issue #50): the `cfg_drums`
 * guidance scale behind the drum-sit control. Routed through the shell
 * note-steering service like the mode; the shell clamps to the model's
 * range and re-asserts it on the play edge. Fire-and-forget; the store
 * projection (`drumsStrength`) reflects it. */
export function setDeckDrumsStrength(deck: number, strength: number): void {
  void invoke('set_deck_drums_strength', { deck, strength }).catch(() => {})
}

/** The live generation params as they cross the IPC boundary (issue #84) —
 * mirrors the shell's `GenerationSnap` (serde camelCase). */
export type Generation = DeckSnap['generation']

/** One tunable generation param, named for a reset-to-default (the shell owns
 * the baseline). Matches the Rust `GenerationField` (serde camelCase). */
export type GenerationField = keyof Generation

/** Edit a deck's live generation params (issue #84) with a PARTIAL patch — only
 * the changed field. The shell merges it onto its authoritative value under one
 * lock (never rebuilding from this webview's snapshot), so a rapid second edit
 * can't revert the first. Coalesced PER FIELD (a slider drag fires per
 * pointermove; distinct fields must not collapse into one another). The shell
 * clamps to the exposed ranges, mirrors the store, and re-sends on a fresh
 * worker's `ready`. Fire-and-forget; the store projection reflects it. */
export function setDeckGeneration(deck: number, patch: Partial<Generation>): void {
  const fields = Object.keys(patch).sort().join(',')
  coalesceIntent(`set_deck_generation:${deck}:${fields}`, () => {
    void invoke('set_deck_generation', { deck, patch }).catch(() => {})
  })
}

/** Reset one of a deck's generation params to the reference baseline (issue
 * #84). The shell owns the default, so this only names the field — the frontend
 * never holds a copy that could drift from the engine's. Discrete: flushes any
 * pending coalesced edit first so a queued drag can't land after (and undo) the
 * reset. Fire-and-forget. */
export function resetDeckGeneration(deck: number, field: GenerationField): void {
  flushIntents()
  void invoke('reset_deck_generation', { deck, field }).catch(() => {})
}

// Coalesce high-rate intents to ~one invoke per animation frame, like the
// engine's control coalescer. A pad drag or an EQ twist fires per pointermove
// (and the 14-bit jog can drive changes at 200-600/s); each store mutation
// broadcasts a full snapshot, so the continuous intents must not flood it.
// Only the latest value per key in a frame is shipped; discrete intents flush
// the pending map first so a queued stale move can never land after (and
// undo) them.
const intentPending = new Map<string, () => void>()
let intentFlushScheduled = false
function flushIntents(): void {
  const due = [...intentPending.values()]
  intentPending.clear()
  for (const run of due) run()
}
function coalesceIntent(key: string, run: () => void): void {
  intentPending.set(key, run)
  if (intentFlushScheduled) return
  intentFlushScheduled = true
  requestAnimationFrame(() => {
    intentFlushScheduled = false
    flushIntents()
  })
}

// --- Deck mixer intents (ADR-0020 phase C): deck-indexed commands that write
// --- the engine AND the store, independent of the deck channel's lifecycle —
// --- a pre-play FX pick must land in the store, or the next snapshot (whose
// --- values the gate-free projection adopts) would revert it.

/** Continuous (fader ride) — coalesced per deck. */
export function setDeckVolume(deck: number, gain: number): void {
  coalesceIntent(`set_volume:${deck}`, () => {
    void invoke('set_volume', { deck, gain }).catch(() => {})
  })
}

/** Continuous (knob twist) — coalesced per band. */
export function setDeckEq(deck: number, band: EqBand, value: number): void {
  coalesceIntent(`set_eq:${deck}:${band}`, () => {
    void invoke('set_eq', { deck, band, value }).catch(() => {})
  })
}

/** Continuous (auto-gain ticks, trim knob) — coalesced per deck. */
export function setDeckTrim(deck: number, db: number): void {
  coalesceIntent(`set_trim:${deck}`, () => {
    void invoke('set_trim', { deck, db }).catch(() => {})
  })
}

export function setDeckCue(deck: number, on: boolean): void {
  flushIntents()
  void invoke('set_cue', { deck, on }).catch(() => {})
}

export function setDeckFx(deck: number, kind: FxKind | null): void {
  flushIntents()
  if (kind === null) {
    void invoke('clear_fx', { deck }).catch(() => {})
  } else {
    void invoke('set_fx', { deck, kind: FX_ARG[kind] }).catch(() => {})
  }
}

/** Continuous (knob twist) — coalesced per deck. */
export function setDeckFxAmount(deck: number, amount: number): void {
  coalesceIntent(`set_fx_amount:${deck}`, () => {
    void invoke('set_fx_amount', { deck, amount }).catch(() => {})
  })
}

// --- Style-pad intents (ADR-0020 phase B): the store owns the arrangement; the
// --- webview emits gestures and projects the result. All fire-and-forget — the
// --- projection reflects whatever the store accepted (a dup add, an over-cap
// --- add, or a colliding rename is a quiet Rust-side no-op).

export function styleAddTarget(deck: number, text: string): void {
  flushIntents()
  void invoke('style_add_target', { deck, text }).catch(() => {})
}

export function styleAddSampleTarget(deck: number, label: string, sample: string): void {
  flushIntents()
  void invoke('style_add_sample_target', { deck, label, sample }).catch(() => {})
}

/** Continuous (drag) — coalesced per target. */
export function styleMoveTarget(deck: number, text: string, x: number, y: number): void {
  coalesceIntent(`style_move_target:${deck}:${text}`, () => {
    void invoke('style_move_target', { deck, text, x, y }).catch(() => {})
  })
}

export function styleRemoveTarget(deck: number, text: string): void {
  flushIntents()
  void invoke('style_remove_target', { deck, text }).catch(() => {})
}

export function styleRenameTarget(deck: number, from: string, to: string): void {
  flushIntents()
  void invoke('style_rename_target', { deck, from, to }).catch(() => {})
}

export function styleToggleSelection(deck: number, text: string): void {
  flushIntents()
  void invoke('style_toggle_selection', { deck, text }).catch(() => {})
}

export function styleFanOut(deck: number): void {
  flushIntents()
  void invoke('style_fan_out', { deck }).catch(() => {})
}

/** Continuous (drag / sweep / jog steer) — coalesced per deck. */
export function styleSetCursor(deck: number, x: number, y: number): void {
  coalesceIntent(`style_set_cursor:${deck}`, () => {
    void invoke('style_set_cursor', { deck, x, y }).catch(() => {})
  })
}

export function styleApplyPreset(
  deck: number,
  targets: { x: number; y: number; text: string }[],
  cursor: { x: number; y: number },
): void {
  flushIntents()
  void invoke('style_apply_preset', { deck, targets, cursor }).catch(() => {})
}

const DECK_INDEX: Record<DeckId, number> = { a: 0, b: 1 }

/** Map the TS `FxKind` (snake) to the Rust `FxKindArg` (camel, serde). */
export const FX_ARG: Record<FxKind, string> = {
  filter: 'filter',
  dub_echo: 'dubEcho',
  space: 'space',
  crush: 'crush',
  noise: 'noise',
  sweep: 'sweep',
}

// --- The wire DTOs (serde camelCase from `src-tauri/src/commands.rs`) ---

type TrackStatusDto = {
  playhead: number
  playing: boolean
  durationFrames: number
  rate: number
  ended: boolean
  loopRegion: { start: number; end: number } | null
}

type LoopSlotDto = { filled: boolean; playing: boolean }

type HealthDto = {
  outputRingFrames: number
  deckRingFrames: number[]
  deckUnderruns: number
  outputUnderruns: number
  masterPeak: number
  masterGainReductionDb: number
  deckLevels: number[]
  contextFrames: number
}

type EngineSnapshotDto = {
  health: HealthDto
  tracks: (TrackStatusDto | null)[]
  loops: LoopSlotDto[][]
}

/** Build the binary payload Tauri ships to the Rust engine: little-endian `u32`
 * prefix words (deck, …) then the interleaved-stereo f32 PCM as raw bytes. */
function framePayload(prefix: number[], pcm: Float32Array): Uint8Array {
  const header = new Uint8Array(prefix.length * 4)
  const view = new DataView(header.buffer)
  prefix.forEach((value, i) => view.setUint32(i * 4, value >>> 0, true))
  const body = new Uint8Array(pcm.buffer, pcm.byteOffset, pcm.byteLength)
  const out = new Uint8Array(header.length + body.length)
  out.set(header, 0)
  out.set(body, header.length)
  return out
}

/** Like `framePayload`, but the body is raw container bytes (a WAV file), not
 * PCM — the `load_track_bytes` frame (ADR-0030's in-memory take path). */
function framePayloadBytes(prefix: number[], bytes: ArrayBuffer): Uint8Array {
  const out = new Uint8Array(prefix.length * 4 + bytes.byteLength)
  const view = new DataView(out.buffer)
  prefix.forEach((value, i) => view.setUint32(i * 4, value >>> 0, true))
  out.set(new Uint8Array(bytes), prefix.length * 4)
  return out
}

/** Frame a save payload for the songs/samples libraries: `[u32 LE meta-JSON
 * byte-length][meta JSON utf-8][WAV bytes]`. A JSON args map would be MBs of text for
 * a multi-MB WAV, so the bytes ride a single binary-IPC arg. Shared by every
 * `save_generated_*` caller (the song/sample compose paths and the deck channel). */
export function encodeMetaFrame(meta: object, wav: ArrayBuffer): Uint8Array {
  const metaBytes = new TextEncoder().encode(JSON.stringify(meta))
  const payload = new Uint8Array(4 + metaBytes.length + wav.byteLength)
  new DataView(payload.buffer).setUint32(0, metaBytes.length, true)
  payload.set(metaBytes, 4)
  payload.set(new Uint8Array(wav), 4 + metaBytes.length)
  return payload
}

/** Decode + resample a WAV to 48 kHz: an `OfflineAudioContext` at the engine rate
 * resamples in `decodeAudioData`. WebKit (the native webview) supports this. */
async function decodeTo48k(wav: ArrayBuffer): Promise<AudioBuffer> {
  // `decodeAudioData` detaches its input — clone so the caller's buffer survives.
  const ctx = new OfflineAudioContext(2, 1, SAMPLE_RATE)
  return ctx.decodeAudioData(wav.slice(0))
}

/** Parse the `track_bands` binary frame — `[u32 LE hop count][low f32
 * LE…][mid…][high…]` — into the three band lanes the zoom strip reads. */
function parseBandsFrame(
  bytes: ArrayBuffer,
): { low: Float32Array; mid: Float32Array; high: Float32Array } | null {
  if (bytes.byteLength < 4) return null
  const hops = new DataView(bytes).getUint32(0, true)
  if (bytes.byteLength < 4 + hops * 3 * 4) return null
  const lane = (index: number) =>
    new Float32Array(bytes.slice(4 + index * hops * 4, 4 + (index + 1) * hops * 4))
  return { low: lane(0), mid: lane(1), high: lane(2) }
}

/** The overview peaks prefetched at load (`track_peaks`, one IPC per load) so
 * `getTrackPeaks` stays synchronous; cleared on unload. Keyed by the bucket
 * count it was fetched at — a different ask is an honest null. */
type CachedPeaks = { buckets: number; min: Float32Array; max: Float32Array }

export function createNativeEngine(): AudioEngine {
  // The latest snapshot the poller cached; the synchronous getters serve from it.
  let snapshot: EngineSnapshotDto | null = null
  // Per-deck stats handlers registered in createDeckChannel, fed from the poller.
  const statsHandlers: (StatsHandler | null)[] = [null, null]
  const peaks: (CachedPeaks | null)[] = [null, null]
  let polling = false
  let lastStatsAt = 0
  const STATS_INTERVAL_MS = 100 // ~10 Hz, matching the worklet's stat cadence

  // --- Per-frame IPC coalescing for high-rate continuous setters ---
  //
  // A fast EQ/fader/filter sweep (pointermove, or the doubled 14-bit FLX4 CCs at
  // ~200-600/s) fires one control command per event. These setters are all
  // idempotent absolute "last-write-wins" sets, so only the latest value per
  // target in a frame matters. We collapse them to ~one invoke per key per
  // animation frame: `coalesce(key, cmd, args)` overwrites the pending entry and
  // schedules a single rAF flush; `flushPending` ships them. Discrete/stateful
  // commands stay immediate and FIRST flush the pending map, so a coalesced
  // value can never leapfrog an ordering-dependent command (e.g. a pending
  // set_fx_amount must land before a set_fx kind-change that resets amount).
  // Purely an IPC reduction — the Rust engine is glitch-free regardless — so it
  // changes no observable behaviour beyond dropping redundant writes.
  type PendingWrite = { cmd: string; args: unknown; fingerprint: string }
  const pending = new Map<string, PendingWrite>()
  // The value last actually shipped per key (written in flushPending, never at
  // queue time) — lets us drop a coalesced write that re-sends an already-applied
  // value (an idempotent absolute set). Safe because every coalesced key is
  // written ONLY through this path, so lastFlushed never drifts from the engine —
  // with one exception that stays consistent: `set_fx` resets the engine's
  // fx_amount to the kind's rest, and `useDeck.setFx` immediately re-applies
  // `setFxAmount(rest)` (the same rest), so after a kind change the engine, the
  // re-applied value, and lastFlushed all agree. Keep that re-apply if you touch
  // the fx kind-change flow.
  const lastFlushed = new Map<string, string>()
  let flushScheduled = false

  function flushPending() {
    flushScheduled = false
    if (pending.size === 0) return
    const due = [...pending.entries()]
    pending.clear()
    for (const [key, { cmd, args, fingerprint }] of due) {
      lastFlushed.set(key, fingerprint)
      void invoke(cmd, args).catch(() => {})
    }
  }

  /** Queue a continuous absolute setter, overwriting any pending value for the
   * same key, and schedule a single flush for the next frame. A value that
   * equals the one last shipped for this key is dropped (idempotent set). */
  function coalesce(key: string, cmd: string, args: unknown) {
    const fingerprint = JSON.stringify(args)
    if (lastFlushed.get(key) === fingerprint) {
      // Already the live value; drop any stale pending entry for this key.
      pending.delete(key)
      return
    }
    pending.set(key, { cmd, args, fingerprint })
    if (!flushScheduled) {
      flushScheduled = true
      requestAnimationFrame(flushPending)
    }
  }

  /** Immediate (non-coalesced) fire-and-forget control for the many `void`
   * setters: first flush every pending coalesced write so this discrete command
   * can never be overtaken, then `invoke` with the rejection swallowed (a dropped
   * control command must never surface as an unhandled rejection). */
  function send(cmd: string, args?: unknown): void {
    flushPending()
    void invoke(cmd, args).catch(() => {})
  }

  function deckTrackStatus(deck: number) {
    const dto = snapshot?.tracks[deck]
    if (!dto) return null
    return {
      position: dto.playhead / SAMPLE_RATE,
      duration: dto.durationFrames / SAMPLE_RATE,
      playing: dto.playing,
      ended: dto.ended,
      rate: dto.rate,
      loop: dto.loopRegion
        ? { start: dto.loopRegion.start / SAMPLE_RATE, end: dto.loopRegion.end / SAMPLE_RATE }
        : null,
      contextTime: (snapshot?.health.contextFrames ?? 0) / SAMPLE_RATE,
    }
  }

  function pumpStats(now: number) {
    if (!snapshot || now - lastStatsAt < STATS_INTERVAL_MS) return
    lastStatsAt = now
    const { health } = snapshot
    const contextTime = health.contextFrames / SAMPLE_RATE
    for (let deck = 0; deck < statsHandlers.length; deck++) {
      const handler = statsHandlers[deck]
      if (!handler) continue
      const track = snapshot.tracks[deck]
      handler({
        underruns: health.deckUnderruns,
        bufferedSeconds: (health.deckRingFrames[deck] ?? 0) / SAMPLE_RATE,
        playing: track ? track.playing : (health.deckRingFrames[deck] ?? 0) > 0,
        playedFrames: health.contextFrames,
        contextTime,
      })
    }
  }

  /** One poll: fetch the consolidated snapshot, cache it, drive the stats
   * handlers, and schedule the next frame. An in-flight guard keeps a slow IPC
   * round-trip from stacking polls. */
  function poll() {
    invoke<EngineSnapshotDto>('engine_snapshot')
      .then((next) => {
        snapshot = next
        pumpStats(performance.now())
      })
      .catch(() => {})
      .finally(() => {
        if (polling) requestAnimationFrame(poll)
      })
  }

  function startPolling() {
    if (polling) return
    polling = true
    requestAnimationFrame(poll)
  }

  /** Resolve once a deck's slot reports `filled` in a fresh snapshot (capture /
   * generated-load landed), or `false` after a short timeout — so the boolean
   * the UI awaits is truthful and the cache is consistent before the follow-up
   * `playLoop` reads it. */
  function awaitSlotFilled(deck: number, slot: number, timeoutMs = 300): Promise<boolean> {
    const deadline = performance.now() + timeoutMs
    return new Promise((resolve) => {
      const check = () => {
        if (snapshot?.loops[deck]?.[slot]?.filled) return resolve(true)
        if (performance.now() >= deadline) return resolve(false)
        requestAnimationFrame(check)
      }
      check()
    })
  }

  function makeDeckChannel(deckId: DeckId): DeckChannel {
    const deck = DECK_INDEX[deckId]
    return {
      // Model PCM is fed engine-side by the sidecar (part 4), not through the UI.
      postPcm: () => {},
      // The realtime ring is cleared by the worker/sidecar lifecycle (part 4);
      // no engine-side reset command in the native path.
      reset: () => {},
      setVolume: (volume) =>
        coalesce(`set_volume:${deck}`, 'set_volume', { deck, gain: volume }),
      setEq: (band, value) =>
        coalesce(`set_eq:${deck}:${band}`, 'set_eq', { deck, band, value }),
      setCue: (on) => send('set_cue', { deck, on }),
      setFx: (kind) =>
        kind === null ? send('clear_fx', { deck }) : send('set_fx', { deck, kind: FX_ARG[kind] }),
      setFxAmount: (amount) =>
        coalesce(`set_fx_amount:${deck}`, 'set_fx_amount', { deck, amount }),
      // Fire-and-forget: the shell captures the frame origin and resets its
      // tracker+gates atomically in stream order (ADR-0025).
      resetAnalysis: () => send('analysis_reset', { deck }),
      setOnAir: (on) => send('set_on_air', { deck, on }),
      setTrim: (db) => coalesce(`set_trim:${deck}`, 'set_trim', { deck, db }),
      captureLoop: async (slot, seconds) => {
        await invoke('capture_loop', { deck, slot, seconds })
        return awaitSlotFilled(deck, slot)
      },
      loadGeneratedLoop: async (slot, wav, oneShot) => {
        const buf = await decodeTo48k(wav)
        const left = buf.getChannelData(0)
        const right = buf.numberOfChannels > 1 ? buf.getChannelData(1) : left
        const pcm = interleaveChannels(left, right)
        // The engine reports whether it accepted the pad (false off Realtime, or a
        // loop too short to install). Trust that verdict rather than polling the
        // slot — a poll can't tell a slow install from a silent refusal, which is
        // what surfaced as a phantom "could not be decoded" on an idle deck.
        return invoke<boolean>(
          'load_generated_loop',
          framePayload([deck, slot, oneShot ? 1 : 0], pcm),
        )
      },
      // Synchronous boolean from the cached slot state (false iff empty — exactly
      // the engine's own false condition). `layer` chooses replace vs sum-on-top
      // (ADR-0022); the engine ignores it for a one-shot buffer.
      playLoop: (slot, layer) => {
        const filled = snapshot?.loops[deck]?.[slot]?.filled ?? false
        if (filled) send('play_loop', { deck, slot, layer })
        return filled
      },
      stopLoop: () => send('stop_loop', { deck }),
      stopLayer: (slot) => send('stop_layer', { deck, slot }),
      stopOneShot: () => send('stop_one_shot', { deck }),
      clearLoop: (slot) => send('clear_loop', { deck, slot }),
      captureSample: async (seconds) => {
        const samples = await invoke<number[] | null>('capture_sample', { deck, seconds })
        return samples ? Float32Array.from(samples) : null
      },
      saveLoopSlot: async (slot, meta) => {
        // A freeze's audio lives only in the engine slot, so the shell reads the
        // exact stored buffer for this deck/slot and persists it — no PCM crosses
        // the boundary here, just the slot coordinates and metadata.
        await invoke('save_loop_slot', { deck, slot, meta })
      },
      saveGeneratedSample: async (wav, meta) => {
        await invoke('save_generated_sample', encodeMetaFrame(meta, wav))
      },
      loadTrack: async (source, peaksBuckets) => {
        // The shell resolves + decodes + analyses (ADR-0030); a refusal (bad
        // file, unsupported codec, oversize) rejects with the shell's reason,
        // which rides the rejection up to the load UI — an explicit load
        // error, never a silent "didn't decode".
        const loaded: { duration: number; bpm: number | null; grid: LoadedTrack['grid'] } =
          source.kind === 'bytes'
            ? await invoke('load_track_bytes', framePayloadBytes([deck], source.wav))
            : await invoke('load_track_file', {
                deck,
                source:
                  source.kind === 'folder'
                    ? { kind: 'folder', dir: source.dir, name: source.name }
                    : { kind: 'song', name: source.name },
              })
        // The summaries the strip and overview read, fetched once and cached
        // (the raw PCM never crosses). A bands/peaks failure degrades those
        // views, not the load.
        const bands = await invoke<ArrayBuffer>('track_bands', { deck })
          .then(parseBandsFrame)
          .catch(() => null)
        const fetched = await invoke<{ min: number[]; max: number[] } | null>('track_peaks', {
          deck,
          buckets: peaksBuckets,
        }).catch(() => null)
        peaks[deck] = fetched
          ? {
              buckets: peaksBuckets,
              min: Float32Array.from(fetched.min),
              max: Float32Array.from(fetched.max),
            }
          : null
        return {
          ...loaded,
          bands: bands ?? {
            low: new Float32Array(0),
            mid: new Float32Array(0),
            high: new Float32Array(0),
          },
          peaks: peaks[deck],
        }
      },
      // The engine ignores the boolean here (useDeck does too); report the cached
      // loaded state for interface compliance.
      playTrack: () => {
        const loaded = snapshot?.tracks[deck] != null
        send('play_track', { deck })
        return loaded
      },
      pauseTrack: () => send('pause_track', { deck }),
      seekTrack: (seconds) => send('seek_track', { deck, frames: seconds * SAMPLE_RATE }),
      setTrackLoop: (start, end) =>
        send('set_track_loop', {
          deck,
          start: Math.round(start * SAMPLE_RATE),
          end: Math.round(end * SAMPLE_RATE),
        }),
      clearTrackLoop: () => send('clear_track_loop', { deck }),
      getTrackStatus: () => deckTrackStatus(deck),
      setTrackRate: (rate) => send('set_track_rate', { deck, rate }),
      // Platter-drag phase nudge (the never-a-click stepped bend): the engine
      // slips the playhead via a rate bend, so the seconds delta crosses as
      // frames like every other transport command.
      nudgeTrackPhase: (seconds) => send('nudge_track_phase', { deck, frames: seconds * SAMPLE_RATE }),
      getTrackPeaks: (buckets) => {
        const cached = peaks[deck]
        if (!cached || cached.buckets !== buckets) return null
        return { min: cached.min, max: cached.max }
      },
      unloadTrack: () => {
        peaks[deck] = null
        send('unload_track', { deck })
      },
      getLevel: () => snapshot?.health.deckLevels[deck] ?? 0,
      dispose: () => {
        // Ship any queued writes so the last swept value isn't lost on teardown.
        flushPending()
        peaks[deck] = null
        statsHandlers[deck] = null
      },
    }
  }

  return {
    getContextTime: () => (snapshot ? snapshot.health.contextFrames / SAMPLE_RATE : null),
    createDeckChannel: async (deckId, _initial, onStats) => {
      const deck = DECK_INDEX[deckId]
      statsHandlers[deck] = onStats
      // No initial-config replay (ADR-0020 phase C): the SHELL hydrates the
      // mixer into engine + store at boot, and live gestures are
      // deck-indexed intents independent of this channel. Replaying here
      // could overwrite hydrated values with the webview's pre-snapshot
      // defaults (an agent-started deck racing the first store snapshot).
      startPolling()
      return makeDeckChannel(deckId)
    },
    // Audio is always running in the native engine — no Web Audio resume gesture.
    resume: () => Promise.resolve(),
    setCrossfade: (position) => coalesce('set_crossfade', 'set_crossfade', { position }),
    setCueMix: (position) => coalesce('set_cue_mix', 'set_cue_mix', { position }),
    // A library preview routed to the phones only (ADR-0027): decode like a track
    // load, then ship the raw PCM (no per-deck header — the preview is engine-wide).
    auditionPlay: async (wav) => {
      const buf = await decodeTo48k(wav)
      const left = buf.getChannelData(0).slice()
      const right = (
        buf.numberOfChannels > 1 ? buf.getChannelData(1) : buf.getChannelData(0)
      ).slice()
      await invoke('audition_play', framePayload([], interleaveChannels(left, right)))
    },
    auditionStop: () => send('audition_stop'),
    // Discrete, rare device commands — a direct invoke, never the per-frame
    // coalescing. The promises are returned so the caller can catch a rejection
    // (the device couldn't be opened; audio stays undisturbed).
    listOutputDevices: () => invoke<OutputDevice[]>('list_output_devices'),
    setMainDevice: (name) => invoke('set_main_device', { name }),
    setCueDevice: (name) => invoke('set_cue_device', { name }),
    getMasterLevel: () => snapshot?.health.masterPeak ?? 0,
    getMasterGainReduction: () => snapshot?.health.masterGainReductionDb ?? 0,
    // The engine taps the master bus on its render thread and streams it straight to
    // disk from Rust (the WAV never crosses IPC). The file opens at start, so the
    // path comes back from start_recording; stop just flushes and closes it.
    startRecording: (folder, name) =>
      invoke<string>('start_recording', { folder, name }),
    stopRecording: () => invoke('stop_recording'),
  }
}
