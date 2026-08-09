/** Pure deck state: every WebSocket and worklet event funnels through this
 * reducer, so the UI is a function of one state object and the stream's
 * health (buffer level, underruns) is always visible, never inferred. */

export type Mrt2RuntimeDiagnostics = {
  runtime: string
  accelerator?: string
  hardware_qualified?: boolean
  experimental?: boolean
  model_revision?: string
  processor_revision?: string
  upstream_source_revision?: string
  torch_version?: string
  torch_cuda_runtime?: string
  nvidia_driver?: string | null
  cuda_device?: string
  cuda_capability?: number[]
  cuda_total_memory_bytes?: number
  capabilities?: {
    weighted_prompts?: boolean
    audio_style?: boolean
    notes?: boolean
    drums?: boolean
    negative_prompt?: boolean
    explicit_seed?: boolean
    reset_to_reseed?: boolean
  }
}

export type ServerEvent =
  | { event: 'ready'; deck: string; model: string; runtime?: Mrt2RuntimeDiagnostics }
  | { event: 'warming'; deck: string; model: string }
  | { event: 'startup_failed'; deck: string; model: string; error: string }
  | {
      event: 'chunk'
      index: number
      rtf: number | null
      generation_latency_ms?: number
      queue_depth?: number | null
    }
  | {
      event: 'style_applied'
      prompts: StylePrompt[]
      effective_from_chunk: number
    }
  | { event: 'model_loading'; model: string }
  | { event: 'worker_died'; model: string }
  | { event: 'error'; error: string }

export type WorkletStats = {
  underruns: number
  bufferedSeconds: number
  playing: boolean
}

export type RamInfo = {
  totalGb: number
  estimateGbByModel: Record<string, number>
}

/** `sample` marks a captured-audio target (M15): `text` is its display
 * label, the id resolves the embedding in the worker's cache. */
export type StylePrompt = { text: string; weight: number; sample?: string }

/** The style the worker confirmed it is generating with. */
export type ActiveStyle = {
  prompts: StylePrompt[]
}

export type DeckAction =
  | { type: 'socket_open' }
  | { type: 'server_event'; event: ServerEvent }
  | { type: 'worklet_stats'; stats: WorkletStats }
  // The Rust store's transport projected down (ADR-0020: the store owns
  // `playing`; deck_play/deck_stop and the sidecar status relay write it there).
  | { type: 'playing_changed'; playing: boolean }
  | { type: 'local_error'; error: string }
  // The available-models list + RAM info, on its own (the native shell fetches it
  // from the generation server — there is no `/ws/deck` hello to carry it). Sets
  // only the picker data, never the per-deck model / switch / style state.
  | { type: 'deck_info'; models: string[]; ramInfo: RamInfo }

export type DeckState = {
  connection: 'connecting' | 'open'
  model: string | null
  availableModels: string[]
  ramInfo: RamInfo | null
  /** A model switch (worker restart) is in flight. */
  switchingModel: boolean
  /** The worker process died; the deck offers a restart. */
  workerDied: boolean
  /** The transport is running (generating — a primed deck counts). A projection
   * of the Rust store's `playing`, which owns it (ADR-0020); no local writer. */
  playing: boolean
  /** The worklet is actually emitting sound (false while prebuffering). */
  audible: boolean
  activeStyle: ActiveStyle | null
  bufferedSeconds: number
  underruns: number
  generationSpeed: number | null
  generationLatencyMs: number | null
  workerQueueDepth: number | null
  runtimeDiagnostics: Mrt2RuntimeDiagnostics | null
  error: string | null
}

/** Whether the deck can take commands right now — the single gating
 * predicate for the transport button, the style pad, and hardware control
 * intents, so the three can never drift apart. */
export function isDeckOperable(state: DeckState): boolean {
  return state.connection === 'open' && !state.switchingModel && !state.workerDied
}

export const initialDeckState: DeckState = {
  connection: 'connecting',
  model: null,
  availableModels: [],
  ramInfo: null,
  switchingModel: false,
  workerDied: false,
  playing: false,
  audible: false,
  activeStyle: null,
  bufferedSeconds: 0,
  underruns: 0,
  generationSpeed: null,
  generationLatencyMs: null,
  workerQueueDepth: null,
  runtimeDiagnostics: null,
  error: null,
}

export function deckReducer(state: DeckState, action: DeckAction): DeckState {
  switch (action.type) {
    case 'socket_open':
      return { ...state, connection: 'open', error: null }
    case 'playing_changed':
      return { ...state, playing: action.playing }
    case 'local_error':
      return { ...state, error: action.error }
    case 'deck_info':
      return { ...state, availableModels: action.models, ramInfo: action.ramInfo }
    case 'worklet_stats': {
      // The engine_snapshot rAF poll dispatches this ~10 Hz for the whole session
      // once a deck channel exists (it has no playing-gate and never stops). Bail
      // to the SAME state object when nothing changed, so an idle/stopped deck does
      // not re-render App ~10 Hz (which, with the Settings drawer inline, would
      // re-commit and dismiss any open native <select> — see ui/Select.tsx).
      const { bufferedSeconds, underruns, playing } = action.stats
      if (
        state.bufferedSeconds === bufferedSeconds &&
        state.underruns === underruns &&
        state.audible === playing
      ) {
        return state
      }
      return { ...state, bufferedSeconds, underruns, audible: playing }
    }
    case 'server_event':
      return applyServerEvent(state, action.event)
  }
}

function applyServerEvent(state: DeckState, event: ServerEvent): DeckState {
  switch (event.event) {
    case 'ready':
      // A fresh worker finished loading — after startup, a model switch, or
      // a crash restart. It has no prompt and is not streaming.
      return {
        ...state,
        model: event.model,
        switchingModel: false,
        workerDied: false,
        runtimeDiagnostics: event.runtime ?? null,
        error: null,
      }
    case 'warming':
      return {
        ...state,
        model: event.model,
        switchingModel: true,
        workerDied: false,
        error: null,
      }
    case 'startup_failed':
      return {
        ...state,
        model: event.model,
        switchingModel: false,
        workerDied: true,
        generationSpeed: null,
        generationLatencyMs: null,
        workerQueueDepth: null,
        error: event.error,
      }
    case 'model_loading':
      // The old worker (and its stream and prompt) is gone. Adopting the
      // target model now lets the RAM warning lead the load instead of
      // trailing it. `playing` is not touched here: the Rust status relay
      // drops it in the store, and the projection carries it down.
      return {
        ...state,
        model: event.model,
        switchingModel: true,
        workerDied: false,
        activeStyle: null,
        generationSpeed: null,
      }
    case 'worker_died':
      return { ...state, workerDied: true, generationLatencyMs: null, workerQueueDepth: null }
    case 'chunk':
      return {
        ...state,
        generationSpeed: event.rtf,
        generationLatencyMs: event.generation_latency_ms ?? null,
        workerQueueDepth: event.queue_depth ?? null,
      }
    case 'style_applied':
      return {
        ...state,
        activeStyle: { prompts: event.prompts },
        error: null,
      }
    case 'error':
      return { ...state, error: event.error }
    default:
      return state
  }
}
