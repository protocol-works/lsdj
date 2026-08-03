import { useCallback, useEffect, useRef, useState } from 'react'
import type {
  KeyboardEvent as ReactKeyboardEvent,
  PointerEvent as ReactPointerEvent,
} from 'react'
import { useTranslation } from 'react-i18next'

import type { DeckId, TrackSource } from '../audio/types'
import { LOOP_CROSSFADE_SECONDS } from '../audio/loops'
import {
  encodeMetaFrame,
  getApiBaseUrl,
  invoke,
  isTauri,
  subscribeLibraryChanged,
  togglePianoWindow,
} from '../audio/nativeEngine'
import { useInterfaceStore } from '../audio/interfaceStore'
import { useControlBus } from '../control/busContext'
import { CrateBrowser } from '../crates/CrateBrowser'
import { postMagentaRender, postSa3Generate } from '../generation/client'
import {
  adaptersForKind,
  stackForKind,
  useLoras,
  useLoraStack,
} from '../models/useLoras'
import {
  APG_DEFAULT,
  CFG_DEFAULT,
  DEFAULT_SA3_STEERING,
  buildSongGeneration,
  mintSa3Seed,
  parseFixedSeed,
  parseGenerationRecipe,
  type GenerationMode,
  type Sa3SteeringDraft,
  type SongGenerationEngine,
  type SongGenerationRecipeV1,
} from '../generation/songGeneration'
import { Sa3AdvancedControls } from '../generation/Sa3AdvancedControls'
import { loadAppSettings, updateAppSettings } from '../persistence'
import { LoraControl } from '../ui/LoraControl'
import type { StylePreset } from '../presets'
import { Button } from '../ui/Button'
import { Panel } from '../ui/Panel'
import { PianoIcon } from '../ui/PianoIcon'
import { Select } from '../ui/Select'
import { SegmentedControl } from '../ui/SegmentedControl'
import { TextField } from '../ui/TextField'
import { randomSongTitle } from './songTitle'
import './media.css'

// The tray toggle's shortcut, shown in its tooltip. ⌘ on macOS (the primary
// target), Ctrl elsewhere — matches the Cmd/Ctrl+M handler in App.
const TOGGLE_SHORTCUT = /Mac/i.test(navigator.userAgent) ? '⌘M' : 'Ctrl+M'

type MediaTab = 'crates' | 'generate' | 'samples' | 'folder'
type TrackEngine = 'sfx' | 'music' | 'track' | 'magenta'
// The short loop engines (ADR-0022): SFX/Music now compose into the samples library,
// not the songs library — the Generate tab keeps only the full-track engines.
type SampleEngine = 'sfx' | 'music'

// One row of the on-disk song registry (Rust `songs::SongEntry`, camelCase): a file
// in the songs folder with the provenance the filesystem can't carry.
type SongEntry = {
  file: string
  title: string
  prompt: string | null
  model: string | null
  recipe?: unknown
}

// One row of the on-disk sample registry (Rust `samples::SampleEntry`, camelCase):
// like a SongEntry plus `oneShot`, the loop-vs-one-shot verdict reload needs.
type SampleEntry = {
  file: string
  title: string
  prompt: string | null
  model: string | null
  oneShot: boolean
}

type GeneratedTrack =
  | {
      id: number
      state: 'pending'
      title: string
      prompt: string
      model: TrackEngine
      recipe: SongGenerationRecipeV1
    }
  | {
      id: number
      state: 'ready'
      // The full display label (prompt + session id for a composed take, or the
      // filename stem for a file found in the folder).
      title: string
      // The full prompt that composed the take, shown by the 🔍 button (prompts are
      // now uncapped, so the row only shows a compact form). null for a file found in
      // the folder that LSDJ didn't generate.
      prompt: string | null
      // The engine that composed the take, or null for a file found in the songs
      // folder that LSDJ didn't generate ("model as option").
      model: TrackEngine | null
      // The filename on disk (the registry identity). null only outside Tauri, where
      // nothing was persisted and the take lives solely in `wav`.
      file: string | null
      // Bytes held only for a take composed THIS session; a restored take reads them
      // from disk on demand (a full render is 100 MB+ — don't hold them all).
      wav?: ArrayBuffer
      recipe?: unknown
    }

// A take in the Samples tab — the loop counterpart of GeneratedTrack. Carries
// `oneShot` (how it reloads into a slot) and a `model` that may also be 'freeze' (a
// deck capture) or null (imported).
type GeneratedSample =
  | {
      id: number
      state: 'pending'
      title: string
      prompt: string
      model: SampleEngine
      oneShot: boolean
    }
  | {
      id: number
      state: 'ready'
      title: string
      // The prompt that generated the clip; null for a freeze capture or an imported
      // file.
      prompt: string | null
      // The source: an engine (sfx/music/magenta), 'freeze' for a deck capture, or
      // null for an imported file. A plain display string, like SongEntry.model.
      model: string | null
      // The filename on disk (registry identity); null only outside Tauri.
      file: string | null
      // Loop vs one-shot — picks how it reloads into a slot.
      oneShot: boolean
      // Bytes held only for a clip composed THIS session in the Samples tab; a
      // restored sample (incl. a deck-saved freeze/pad) reads them from disk on load.
      wav?: ArrayBuffer
    }

// A browsable file: just the name. `read_audio_file` re-derives the path from the
// chosen folder + name (the webview never supplies a path to read).
type FolderFile = { name: string }

// Per-engine length menus, mirroring the backend caps: the small DiTs
// stop at sa3.MAX_SECONDS (32 s), the medium track DiT at
// sa3.TRACK_MAX_SECONDS (6:20), Magenta renders at
// controller.RENDER_MAX_SECONDS (3:00).
const ENGINE_LENGTHS: Record<TrackEngine, number[]> = {
  sfx: [5, 10, 20, 30],
  music: [5, 10, 20, 30],
  track: [60, 120, 240, 380],
  magenta: [30, 60, 120, 180],
}
const ENGINES = Object.keys(ENGINE_LENGTHS) as TrackEngine[]
// The Generate tab composes full tracks (songs); the Samples tab composes short
// loops (samples). SFX/Music moved to the Samples tab (ADR-0022). `ENGINES` stays
// the full set so `asTrackEngine` still recognises an older song saved as sfx/music.
const TRACK_ENGINES: SongGenerationEngine[] = ['track', 'magenta']
const SAMPLE_ENGINES: SampleEngine[] = ['sfx', 'music']

function formatLength(seconds: number): string {
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`
}

/** A registry `model` string back into a known engine, or null ("none") for a file
 * the app didn't generate — so an unknown/absent value renders as "Imported". */
function asTrackEngine(model: string | null): TrackEngine | null {
  return model && (ENGINES as string[]).includes(model) ? (model as TrackEngine) : null
}

/** Pretty-print a prompt when it's JSON so the inspector is readable (a pasted spec is
 * often minified or awkwardly wrapped); otherwise show it verbatim. */
function prettyPrompt(prompt: string): string {
  try {
    return JSON.stringify(JSON.parse(prompt), null, 2)
  } catch {
    return prompt
  }
}

/** The take's label for the row, the deck, and aria: the title plus a session-unique
 * #id for a composed take (so same-title siblings stay tellable apart), or just the
 * name for an imported file (no prompt). */
function trackLabel(track: GeneratedTrack): string {
  return track.prompt != null ? `${track.title} #${track.id}` : track.title
}

/** A sample's row/deck/aria label — the take rule, applied to a sample: title plus a
 * session-unique #id for a composed/generated clip (a prompt), or just the name for a
 * freeze / imported file. */
function sampleLabel(sample: GeneratedSample): string {
  return sample.prompt != null ? `${sample.title} #${sample.id}` : sample.title
}

/** A row's on-disk filename, or null for a pending row / an in-session take not yet
 * saved. The key a live re-list reuses an existing row by, so its id (and any
 * in-memory `wav`) survive a refresh instead of churning. */
function fileOf(row: { state: string; file?: string | null }): string | null {
  return row.state === 'ready' ? (row.file ?? null) : null
}

function hasVersionedRecipe(value: unknown): boolean {
  return (
    value != null &&
    typeof value === 'object' &&
    typeof (value as { version?: unknown }).version === 'number'
  )
}

/** Re-list one library (songs or samples) from its on-disk registry, reconciled
 * against the folder by the Rust shell (hand-added files appear; deleted files drop
 * out). A row already held for a file keeps its id + in-memory wav (reuse by
 * filename), so a live re-list never churns; a row whose file vanished is dropped; an
 * in-session take not yet on disk is kept. `ref` is read after the fetch resolves
 * (freshest), and the id mint (`toRow`) runs OUTSIDE the state updater — StrictMode
 * replays updaters, so they must be pure. A no-op outside Tauri. */
function reListLibrary<
  R extends { id: number; state: string; file?: string | null },
  E extends { file: string },
>(
  command: string,
  ref: { current: R[] },
  setRows: (next: (current: R[]) => R[]) => void,
  toRow: (entry: E) => R,
): void {
  if (!isTauri()) return
  void (async () => {
    let entries: E[]
    try {
      entries = (await invoke<E[]>(command)) ?? []
    } catch {
      return // a failed scan just means no refresh; composing still works
    }
    const byFile = new Map(
      ref.current
        .map((row) => [fileOf(row), row] as const)
        .filter((pair): pair is readonly [string, R] => pair[0] != null),
    )
    const restored = entries.map((entry) => byFile.get(entry.file) ?? toRow(entry))
    // Newest-first: in-session takes not yet on disk lead, above the restored
    // library reversed so the most recently composed file sits at the top (the
    // registry stores composition order, oldest first), sparing a scroll to the
    // take you just made.
    setRows((current) => [...current.filter((row) => fileOf(row) == null), ...restored.reverse()])
  })()
}

/** What the webview sends with a freshly composed take (Rust `songs::NewSong`). */
type NewSong = {
  title: string
  prompt: string
  model: SongGenerationEngine
  recipe: SongGenerationRecipeV1
}

/** Persist a ready take to ~/Documents/LSDJ/generated_songs through the Rust shell
 * and return its registry entry ({@link encodeMetaFrame} builds the binary payload —
 * a JSON args map would be megabytes of text for a multi-MB WAV). The old
 * `<a download>` is gone: it silently no-ops in WKWebView. */
function saveGeneratedSong(meta: NewSong, wav: ArrayBuffer): Promise<SongEntry> {
  return invoke<SongEntry>('save_generated_song', encodeMetaFrame(meta, wav))
}

/** What the webview sends with a freshly composed sample (Rust `samples::NewSample`):
 * a NewSong plus `oneShot`. `prompt`/`model` are nullable so the same shape carries a
 * deck freeze (server-side via `save_loop_slot`), though the Samples tab always sends
 * a prompt + engine. */
type NewSample = {
  title: string
  prompt: string | null
  model: string | null
  oneShot: boolean
}

/** Persist a Samples-tab composition to ~/Documents/LSDJ/generated_samples through
 * the Rust shell. (Deck freezes and pads auto-save through the deck channel; this is
 * only the explorer's own SFX/Music compositions.) */
function saveGeneratedSample(meta: NewSample, wav: ArrayBuffer): Promise<SampleEntry> {
  return invoke<SampleEntry>('save_generated_sample', encodeMetaFrame(meta, wav))
}

type MediaExplorerProps = {
  presets: StylePreset[]
  onLoadPreset: (deck: DeckId, preset: StylePreset) => void
  onDeletePreset: (name: string) => void
  onImportPresets: (presets: StylePreset[]) => void
  /** Load a decoded-to-be track onto a deck — flips it to playback
   * mode (ADR-0013). A shell refusal rejects with its reason (ADR-0030);
   * false only when the deck channel couldn't come up. */
  onLoadTrack: (deck: DeckId, source: TrackSource, title: string) => Promise<boolean>
  /** Load a saved sample into a deck loop slot (ADR-0022) — the first free
   * slot, as a loop or one-shot per the sample, layering over whatever the deck
   * plays (live or a track). Resolves false when every slot is full or the body
   * doesn't decode. */
  onLoadSample: (
    deck: DeckId,
    wav: ArrayBuffer,
    oneShot: boolean,
    label: string,
  ) => Promise<boolean>
  /** Preview a decoded WAV in the headphones before committing it to a deck
   * (ADR-0027) — routed to the cue feed only, never the master. */
  onPreview: (wav: ArrayBuffer) => Promise<void>
  /** Stop the headphone preview (ADR-0027). */
  onStopPreview: () => void
  /** Drawer state for the collapsible tray: whether it's expanded, and the
   * content height in px when open. State is owned by App (single source of
   * truth) so the keyboard shortcut and the in-panel toggle stay in sync. */
  open: boolean
  onToggle: () => void
  height: number
  /** Resize from dragging the top grip. `commit` is false during the drag (live
   * update only) and true on release, so App persists once at the end. */
  onResize: (height: number, commit: boolean) => void
}

/** The Media Explorer (M19, ADR-0013): one pane below the booth that
 * owns loading. Crates (M16, folded in), generated tracks, and local
 * folder tracks all load onto a deck; the item type decides the deck's
 * mode. The FLX4 rotary browses the visible tab; LOAD loads its
 * highlighted item. */
export function MediaExplorer({
  presets,
  onLoadPreset,
  onDeletePreset,
  onImportPresets,
  onLoadTrack,
  onLoadSample,
  onPreview,
  onStopPreview,
  open,
  onToggle,
  height,
  onResize,
}: MediaExplorerProps) {
  const { t } = useTranslation()
  // Whether the standalone MIDI-keyboard window (issue #49) is open — a shell
  // read-back that lights the tray's piano toggle.
  const pianoWindowOpen = useInterfaceStore()?.pianoWindowOpen ?? false
  const [tab, setTab] = useState<MediaTab>('crates')
  // True only while the top grip is being dragged, to drop the height
  // transition so the tray tracks the cursor instead of easing behind it.
  const [dragging, setDragging] = useState(false)
  const [tracks, setTracks] = useState<GeneratedTrack[]>([])
  // The take name (and on-disk filename), decoupled from the prompt. Blank → a random
  // song title at compose time, so a long/JSON prompt never becomes the name.
  const [title, setTitle] = useState('')
  const [prompt, setPrompt] = useState('')
  const [engine, setEngine] = useState<SongGenerationEngine>('track')
  const [seconds, setSeconds] = useState(120)
  const [generationMode, setGenerationMode] = useState<GenerationMode>(
    () => loadAppSettings().generationMode ?? 'basic',
  )
  const [steering, setSteering] = useState<Sa3SteeringDraft>(() => ({
    ...DEFAULT_SA3_STEERING,
    seed: { ...DEFAULT_SA3_STEERING.seed },
  }))
  const [recipeNotice, setRecipeNotice] = useState<string | null>(null)
  const [generateError, setGenerateError] = useState<string | null>(null)
  // The installed SA3 LoRA adapters (issue #66) and each form's stack: the
  // adapters riding the next generation, each at a trim strength. The racks
  // only offer base-matched adapters; a slot that stops resolving (adapter
  // deleted) silently drops from the request.
  const loras = useLoras()
  const trackStack = useLoraStack()
  const sampleStack = useLoraStack()
  // Auto-save runs after a take is composed; its failure is separate from a
  // generation failure (the take is already playable from memory).
  const [saveError, setSaveError] = useState<string | null>(null)
  // The Samples tab (ADR-0022): its own compose form + list, the loop counterpart of
  // the Generate tab's. Short SFX/Music compose here now; deck freezes/pads also land
  // in this library and surface on a re-list.
  const [samples, setSamples] = useState<GeneratedSample[]>([])
  const [sampleTitle, setSampleTitle] = useState('')
  const [samplePrompt, setSamplePrompt] = useState('')
  const [sampleEngine, setSampleEngine] = useState<SampleEngine>('sfx')
  const [sampleSeconds, setSampleSeconds] = useState(10)
  const [sampleOneShot, setSampleOneShot] = useState(false)
  const [sampleError, setSampleError] = useState<string | null>(null)
  const [sampleSaveError, setSampleSaveError] = useState<string | null>(null)
  const [folderName, setFolderName] = useState<string | null>(null)
  // The native picker's absolute folder path; `read_audio_file` scopes reads to it.
  const [folderPath, setFolderPath] = useState<string | null>(null)
  const [files, setFiles] = useState<FolderFile[]>([])
  const [folderError, setFolderError] = useState<string | null>(null)
  // The rotary highlight for the generate/folder tabs; the crates tab
  // keeps its own inside CrateBrowser (mounted only while visible, so
  // exactly one list answers the hardware at a time).
  const [highlight, setHighlight] = useState(0)
  // The take whose prompt is expanded to full text, or null. One at a time; the
  // rest stay truncated to one line.
  const [expandedId, setExpandedId] = useState<number | null>(null)
  // The item currently auditioning in the headphones (ADR-0027), or null. One at
  // a time — starting another preview, loading, or unmounting stops it.
  const [previewingId, setPreviewingId] = useState<number | null>(null)
  // Never leave a preview ringing in the phones after the pane unmounts.
  useEffect(() => onStopPreview, [onStopPreview])
  // A ref, not state: two composes batched into one render (Enter +
  // click) must not mint the same id.
  const nextIdRef = useRef(1)
  // The latest lists mirrored in refs (synced after commit). A live re-list (tab
  // open, or the folder watcher firing) reads these from its effect/callback to reuse
  // a row's id + in-memory wav by filename, so a refresh never churns ids or re-reads
  // bytes — and the id mint stays OUTSIDE the state updater (StrictMode replays
  // updaters, so they must be pure). At most one render stale, which is fine here.
  const tracksRef = useRef<GeneratedTrack[]>([])
  const samplesRef = useRef<GeneratedSample[]>([])
  useEffect(() => {
    tracksRef.current = tracks
    samplesRef.current = samples
  }, [tracks, samples])

  const ready = tracks.filter(
    (track): track is GeneratedTrack & { state: 'ready' } =>
      track.state === 'ready',
  )
  const highlightedReadyId =
    ready.length === 0 ? null : ready[Math.min(highlight, ready.length - 1)].id

  const readySamples = samples.filter(
    (sample): sample is GeneratedSample & { state: 'ready' } =>
      sample.state === 'ready',
  )
  const highlightedSampleId =
    readySamples.length === 0
      ? null
      : readySamples[Math.min(highlight, readySamples.length - 1)].id

  function stopPreview() {
    onStopPreview()
    setPreviewingId(null)
  }

  // Audition a take in the phones (ADR-0027): toggle it off if it's the one
  // playing, otherwise fetch its WAV (the same in-memory-or-disk path as a load)
  // and start previewing. Routed to the cue feed only, never the master.
  async function cueTrack(track: GeneratedTrack & { state: 'ready' }) {
    if (previewingId === track.id) {
      stopPreview()
      return
    }
    setGenerateError(null)
    try {
      const label = trackLabel(track)
      let wav = track.wav
      if (!wav) {
        if (!track.file) throw new Error(t('media.undecodable', { title: label }))
        wav = await invoke<ArrayBuffer>('read_generated_song', { name: track.file })
      }
      await onPreview(wav.slice(0))
      setPreviewingId(track.id)
    } catch (error) {
      setGenerateError(error instanceof Error ? error.message : String(error))
    }
  }

  async function cueSample(sample: GeneratedSample & { state: 'ready' }) {
    if (previewingId === sample.id) {
      stopPreview()
      return
    }
    setSampleError(null)
    try {
      const label = sampleLabel(sample)
      let wav = sample.wav
      if (!wav) {
        if (!sample.file) throw new Error(t('media.undecodable', { title: label }))
        wav = await invoke<ArrayBuffer>('read_generated_sample', { name: sample.file })
      }
      await onPreview(wav.slice(0))
      setPreviewingId(sample.id)
    } catch (error) {
      setSampleError(error instanceof Error ? error.message : String(error))
    }
  }

  async function loadGeneratedTrack(
    deck: DeckId,
    track: GeneratedTrack & { state: 'ready' },
  ) {
    // Committing to a deck ends the preview — the deck takes over.
    if (previewingId != null) stopPreview()
    setGenerateError(null)
    try {
      // A persisted take loads by library reference — the shell reads and
      // decodes it (ADR-0030); only a take composed this session and not yet
      // on disk ships its WAV bytes (once, as a container — never PCM).
      const label = trackLabel(track)
      let source: TrackSource
      if (track.file) {
        source = { kind: 'song', name: track.file }
      } else if (track.wav) {
        source = { kind: 'bytes', wav: track.wav.slice(0) }
      } else {
        throw new Error(t('media.undecodable', { title: label }))
      }
      const loaded = await onLoadTrack(deck, source, label)
      if (!loaded) setGenerateError(t('media.undecodable', { title: label }))
    } catch (error) {
      // The click is fire-and-forget (`void loadGeneratedTrack`), so a rejected
      // read/decode/load would otherwise vanish and look like nothing happened.
      setGenerateError(error instanceof Error ? error.message : String(error))
    }
  }

  const dropTrack = (id: number) =>
    setTracks((current) => current.filter((entry) => entry.id !== id))

  async function removeTrack(track: GeneratedTrack & { state: 'ready' }) {
    // An in-memory-only take (no file, or no native shell) just leaves the list. A
    // persisted one is removed only AFTER the file is moved to the Trash and the
    // registry pruned — so a failed delete keeps the row, matching what's on disk
    // rather than vanishing and then reappearing on the next launch's scan.
    if (!isTauri() || !track.file) {
      dropTrack(track.id)
      return
    }
    try {
      await invoke('delete_generated_song', { name: track.file })
      dropTrack(track.id)
    } catch (error) {
      setSaveError(
        t('media.generate.deleteFailed', {
          title: track.title,
          message: error instanceof Error ? error.message : String(error),
        }),
      )
    }
  }

  async function loadFolderFile(deck: DeckId, file: FolderFile) {
    if (folderPath === null) return
    setFolderError(null)
    try {
      // By reference: the shell re-derives the path (scoped to the chosen
      // folder), reads, and decodes (ADR-0030). No bytes cross to load.
      const loaded = await onLoadTrack(
        deck,
        { kind: 'folder', dir: folderPath, name: file.name },
        file.name,
      )
      if (!loaded) setFolderError(t('media.undecodable', { title: file.name }))
    } catch (error) {
      setFolderError(error instanceof Error ? error.message : String(error))
    }
  }

  /** The display label for a sample's source/engine column: an engine name, "Freeze"
   * for a deck capture, or "Imported" for a hand-added file / unknown model. */
  function sampleModelLabel(model: string | null): string {
    if (model == null) return t('media.generate.imported')
    if (model === 'freeze') return t('media.samples.freeze')
    if ((ENGINES as string[]).includes(model)) return t(`media.generate.engines.${model}`)
    return t('media.generate.imported')
  }

  async function loadSample(
    deck: DeckId,
    sample: GeneratedSample & { state: 'ready' },
  ) {
    // Committing to a deck ends the preview — the deck takes over.
    if (previewingId != null) stopPreview()
    setSampleError(null)
    try {
      // In memory for a clip composed this session; otherwise read the bytes back
      // from disk (scoped to the samples folder by the Rust shell).
      const label = sampleLabel(sample)
      let wav = sample.wav
      if (!wav) {
        if (!sample.file) throw new Error(t('media.undecodable', { title: label }))
        wav = await invoke<ArrayBuffer>('read_generated_sample', { name: sample.file })
      }
      // decodeAudioData (inside the slot load) detaches its input — hand over a copy
      // so the sample can load again, or onto the other deck.
      const loaded = await onLoadSample(deck, wav.slice(0), sample.oneShot, label)
      if (!loaded) setSampleError(t('media.samples.loadFailed', { title: label }))
    } catch (error) {
      // The click is fire-and-forget (`void loadSample`), so a rejected read/decode/
      // load would otherwise vanish and look like nothing happened.
      setSampleError(error instanceof Error ? error.message : String(error))
    }
  }

  const dropSample = (id: number) =>
    setSamples((current) => current.filter((entry) => entry.id !== id))

  async function removeSample(sample: GeneratedSample & { state: 'ready' }) {
    // Mirror removeTrack: an in-memory-only clip just leaves the list; a persisted
    // one is removed only after the file is trashed and the registry pruned.
    if (!isTauri() || !sample.file) {
      dropSample(sample.id)
      return
    }
    try {
      await invoke('delete_generated_sample', { name: sample.file })
      dropSample(sample.id)
    } catch (error) {
      setSampleSaveError(
        t('media.samples.deleteFailed', {
          title: sample.title,
          message: error instanceof Error ? error.message : String(error),
        }),
      )
    }
  }

  // The two libraries' re-list, each a thin {@link reListLibrary} call differing only
  // in the command, the ref, the setter, and the registry-entry → row mapping (a
  // sample carries `oneShot`; a song's model runs through `asTrackEngine`). Used at
  // startup and by the folder watcher.
  const refreshSongs = useCallback(
    () =>
      reListLibrary<GeneratedTrack, SongEntry>(
        'list_generated_songs',
        tracksRef,
        setTracks,
        (entry) => ({
          id: nextIdRef.current++,
          state: 'ready',
          title: entry.title,
          prompt: entry.prompt,
          model: asTrackEngine(entry.model),
          file: entry.file,
          recipe: entry.recipe,
        }),
      ),
    [],
  )
  const refreshSamples = useCallback(
    () =>
      reListLibrary<GeneratedSample, SampleEntry>(
        'list_generated_samples',
        samplesRef,
        setSamples,
        (entry) => ({
          id: nextIdRef.current++,
          state: 'ready',
          title: entry.title,
          prompt: entry.prompt,
          model: entry.model,
          file: entry.file,
          oneShot: entry.oneShot,
        }),
      ),
    [],
  )

  // Restore both lists at startup; the folder watcher keeps them live thereafter.
  useEffect(() => {
    refreshSongs()
    refreshSamples()
  }, [refreshSongs, refreshSamples])

  // Live-reload from the Rust folder watcher (ADR-0022): a deck auto-saving a sample,
  // or a file dropped in / deleted by hand, fires `library://changed`; re-list the
  // named library. A no-op outside Tauri (the event bridge is absent).
  useEffect(
    () =>
      subscribeLibraryChanged((library) =>
        library === 'songs' ? refreshSongs() : refreshSamples(),
      ),
    [refreshSongs, refreshSamples],
  )

  const bus = useControlBus()
  useEffect(() =>
    bus.subscribe((intent) => {
      if (intent.kind === 'browse_tab') {
        // Rotary press: cycle the visible tab from the hardware.
        setTab((current) => {
          const order: MediaTab[] = ['crates', 'generate', 'samples', 'folder']
          return order[(order.indexOf(current) + 1) % order.length]
        })
        setHighlight(0)
        return
      }
      if (tab === 'crates') return // CrateBrowser owns its own list
      const count =
        tab === 'generate'
          ? ready.length
          : tab === 'samples'
            ? readySamples.length
            : files.length
      if (intent.kind === 'browse_scroll') {
        if (count === 0) return
        setHighlight((current) =>
          Math.max(0, Math.min(count - 1, Math.min(current, count - 1) + intent.steps)),
        )
      } else if (intent.kind === 'browse_load') {
        const index = Math.min(highlight, count - 1)
        if (index < 0) return
        if (tab === 'generate') {
          void loadGeneratedTrack(intent.deck, ready[index])
        } else if (tab === 'samples') {
          void loadSample(intent.deck, readySamples[index])
        } else {
          void loadFolderFile(intent.deck, files[index])
        }
      }
    }),
  )

  /** Compose a short loop in the Samples tab → ~/Documents/LSDJ/generated_samples
   * (ADR-0022). Mirrors {@link generateTrack} but always SFX/Music and persists to the
   * sample library. A loop carries the seam surplus the engine folds on reload (the
   * deck-pad convention), so the saved WAV reloads at the right musical length. */
  function composeSample() {
    const trimmedPrompt = samplePrompt.trim()
    if (!trimmedPrompt) return
    const id = nextIdRef.current++
    const requestEngine = sampleEngine
    const requestLoras = stackForKind(sampleStack.stack, loras, requestEngine)
    const oneShot = sampleOneShot
    const clipTitle = sampleTitle.trim() || randomSongTitle()
    // A loop asks for the surplus tail the engine folds away on reload; a one-shot is
    // taken as asked.
    const requestSeconds = oneShot ? sampleSeconds : sampleSeconds + LOOP_CROSSFADE_SECONDS
    setSampleError(null)
    setSampleSaveError(null)
    setSamples((current) => [
      {
        id,
        state: 'pending',
        title: clipTitle,
        prompt: trimmedPrompt,
        model: requestEngine,
        oneShot,
      },
      ...current,
    ])
    void (async () => {
      try {
        const apiBase = await getApiBaseUrl()
        const response = await fetch(`${apiBase}/api/generate`, {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({
            prompt: trimmedPrompt,
            seconds: requestSeconds,
            kind: requestEngine,
            ...(requestLoras.length > 0 ? { loras: requestLoras } : {}),
          }),
        })
        if (!response.ok) {
          const detail = await response
            .json()
            .then((body: { detail?: string }) => body.detail)
            .catch(() => null)
          throw new Error(detail || `generation failed (${response.status})`)
        }
        const wav = await response.arrayBuffer()
        setSamples((current) =>
          current.map((sample) =>
            sample.id === id
              ? {
                  id,
                  state: 'ready',
                  title: clipTitle,
                  prompt: trimmedPrompt,
                  model: requestEngine,
                  file: null,
                  oneShot,
                  wav,
                }
              : sample,
          ),
        )
        if (isTauri()) {
          try {
            const entry = await saveGeneratedSample(
              { title: clipTitle, prompt: trimmedPrompt, model: requestEngine, oneShot },
              wav,
            )
            setSamples((current) =>
              current.map((sample) =>
                sample.id === id && sample.state === 'ready'
                  ? { ...sample, file: entry.file }
                  : sample,
              ),
            )
          } catch (error) {
            setSampleSaveError(
              t('media.samples.saveFailed', {
                title: clipTitle,
                message: error instanceof Error ? error.message : String(error),
              }),
            )
          }
        }
      } catch (error) {
        setSamples((current) => current.filter((sample) => sample.id !== id))
        setSampleError(error instanceof Error ? error.message : String(error))
      }
    })()
  }

  async function openSamplesFolder() {
    setSampleSaveError(null)
    try {
      await invoke('open_samples_folder')
    } catch (error) {
      setSampleSaveError(
        t('media.samples.openFolderFailed', {
          message: error instanceof Error ? error.message : String(error),
        }),
      )
    }
  }

  function generateTrack() {
    const trimmedPrompt = prompt.trim()
    if (!trimmedPrompt) return
    const requestEngine = engine
    const requestLoras =
      requestEngine === 'magenta'
        ? []
        : stackForKind(trackStack.stack, loras, requestEngine)
    let generation
    try {
      generation = buildSongGeneration(
        generationMode,
        requestEngine,
        trimmedPrompt,
        seconds,
        requestLoras,
        steering,
        requestEngine === 'track' &&
          (generationMode === 'basic' || steering.seed.mode === 'random')
          ? mintSa3Seed()
          : undefined,
      )
    } catch {
      setGenerateError(t('media.generate.advanced.seedInvalid'))
      return
    }
    const id = nextIdRef.current++
    // The name (and on-disk filename) come from the Title field, NOT the prompt — a
    // blank title gets a random song title so a long/JSON prompt never becomes the
    // name. The row appends a session-unique #id to tell same-title siblings apart.
    const songTitle = title.trim() || randomSongTitle()
    setGenerateError(null)
    setSaveError(null)
    setRecipeNotice(null)
    setTracks((current) => [
      {
        id,
        state: 'pending',
        title: songTitle,
        prompt: trimmedPrompt,
        model: requestEngine,
        recipe: generation.recipe,
      },
      ...current,
    ])
    void (async () => {
      try {
        const wav =
          'kind' in generation.request
            ? await postSa3Generate(generation.request)
            : await postMagentaRender(generation.request)
        setTracks((current) =>
          current.map((track) =>
            track.id === id
              ? {
                  id,
                  state: 'ready',
                  title: songTitle,
                  prompt: trimmedPrompt,
                  model: requestEngine,
                  file: null,
                  wav,
                  recipe: generation.recipe,
                }
              : track,
          ),
        )
        // Auto-persist to the songs folder so a take is never lost to a webview that
        // can't download. Skipped in a plain browser (no shell to write through); a
        // native write failure is surfaced but leaves the in-memory take playable.
        // On success, stamp the take with its on-disk filename so it survives a
        // restart and can be reloaded/deleted.
        if (isTauri()) {
          try {
            const entry = await saveGeneratedSong(
              {
                title: songTitle,
                prompt: trimmedPrompt,
                model: requestEngine,
                recipe: generation.recipe,
              },
              wav,
            )
            setTracks((current) =>
              current.map((track) =>
                track.id === id && track.state === 'ready'
                  ? { ...track, file: entry.file }
                  : track,
              ),
            )
          } catch (error) {
            setSaveError(
              t('media.generate.saveFailed', {
                title: songTitle,
                message: error instanceof Error ? error.message : String(error),
              }),
            )
          }
        }
      } catch (error) {
        setTracks((current) => current.filter((track) => track.id !== id))
        setGenerateError(error instanceof Error ? error.message : String(error))
      }
    })()
  }

  async function chooseFolder() {
    setFolderError(null)
    // The OS folder picker (dialog plugin) + a Rust dir listing — WKWebView has no
    // File System Access API.
    try {
      const dir = await invoke<string | null>('plugin:dialog|open', {
        options: { directory: true, multiple: false },
      })
      if (!dir) return // the user dismissed the picker
      const names = await invoke<string[]>('list_audio_files', { dir })
      setFolderPath(dir)
      setFolderName(dir.replace(/\/+$/, '').split('/').pop() || dir)
      setFiles(names.map((name) => ({ name })))
      setHighlight(0)
    } catch (error) {
      setFolderError(error instanceof Error ? error.message : String(error))
    }
  }

  async function openSongsFolder() {
    setSaveError(null)
    // The Rust shell owns the folder path and reveals it in Finder (the webview
    // can't), so the webview just asks — no path crosses the boundary.
    try {
      await invoke('open_songs_folder')
    } catch (error) {
      setSaveError(
        t('media.generate.openFolderFailed', {
          message: error instanceof Error ? error.message : String(error),
        }),
      )
    }
  }

  function changeGenerationMode(next: GenerationMode) {
    setGenerationMode(next)
    updateAppSettings({ generationMode: next })
  }

  function recallSongRecipe(track: GeneratedTrack & { state: 'ready' }) {
    const parsed = parseGenerationRecipe(track.recipe)
    if (parsed.status === 'unsupported') {
      setRecipeNotice(t('media.generate.recipeUnsupported'))
      return
    }
    if (parsed.status === 'invalid') {
      setRecipeNotice(t('media.generate.recipeInvalid'))
      return
    }
    const { recipe } = parsed
    if (!ENGINE_LENGTHS[recipe.engine].includes(recipe.seconds)) {
      setRecipeNotice(t('media.generate.recipeInvalid'))
      return
    }

    setPrompt(recipe.prompt)
    setEngine(recipe.engine)
    setSeconds(recipe.seconds)
    const availableNames = new Set(
      recipe.engine === 'track'
        ? adaptersForKind(loras, 'track').map((adapter) => adapter.name)
        : [],
    )
    const recalledLoras = recipe.loras.filter((choice) => availableNames.has(choice.name))
    const unavailable = recipe.loras
      .filter((choice) => !availableNames.has(choice.name))
      .map((choice) => choice.name)
    trackStack.replace(recalledLoras)

    if (recipe.sa3) {
      setSteering({
        negativePrompt: recipe.sa3.negativePrompt,
        guidance: recipe.sa3.cfg === undefined ? 'off' : 'on',
        cfg: recipe.sa3.cfg ?? CFG_DEFAULT,
        apg: recipe.sa3.apg ?? APG_DEFAULT,
        seed: { mode: 'fixed', value: String(recipe.sa3.seed) },
      })
      changeGenerationMode('advanced')
    } else {
      changeGenerationMode('basic')
    }
    setGenerateError(null)
    setRecipeNotice(
      unavailable.length > 0
        ? t('media.generate.recipeUnavailableLoras', { names: unavailable.join(', ') })
        : t('media.generate.settingsApplied', { title: track.title }),
    )
  }

  const lengths = ENGINE_LENGTHS[engine]
  const fixedSeedInvalid =
    generationMode === 'advanced' &&
    engine === 'track' &&
    steering.seed.mode === 'fixed' &&
    parseFixedSeed(steering.seed.value) === null
  // The rack offers only base-matched adapters. Magenta keeps adapter management
  // reachable but has no compatible generation path and sends no LoRA fields.

  function loadButtons(onLoad: (deck: DeckId) => void, name: string) {
    return (['a', 'b'] as const).map((deck) => (
      <Button
        key={deck}
        aria-label={t('media.loadTo', { name, deck: deck.toUpperCase() })}
        onClick={() => onLoad(deck)}
      >
        {t('media.loadShort', { deck: deck.toUpperCase() })}
      </Button>
    ))
  }

  /** Drag the top grip to resize: dragging up grows the tray. Pointer capture
   * keeps the gesture alive if the cursor leaves the thin handle. */
  function startResize(event: ReactPointerEvent<HTMLDivElement>) {
    event.preventDefault()
    const startY = event.clientY
    const startHeight = height
    let latest = startHeight
    event.currentTarget.setPointerCapture(event.pointerId)
    setDragging(true)
    function onMove(move: PointerEvent) {
      latest = startHeight + (startY - move.clientY)
      onResize(latest, false)
    }
    function onUp() {
      setDragging(false)
      onResize(latest, true)
      window.removeEventListener('pointermove', onMove)
      window.removeEventListener('pointerup', onUp)
    }
    window.addEventListener('pointermove', onMove)
    window.addEventListener('pointerup', onUp)
  }

  const collapseHint = t('media.collapseHint', { shortcut: TOGGLE_SHORTCUT })
  const expandHint = t('media.expandHint', { shortcut: TOGGLE_SHORTCUT })
  // Collapsed, the whole header bar is the expand target (click or Enter/Space);
  // open, the header is inert chrome and only the chevron button toggles.
  const collapsedBarProps = open
    ? {}
    : {
        role: 'button' as const,
        tabIndex: 0,
        'aria-expanded': false,
        'aria-label': t('media.expand'),
        title: expandHint,
        onClick: onToggle,
        onKeyDown: (event: ReactKeyboardEvent<HTMLDivElement>) => {
          if (event.key === 'Enter' || event.key === ' ') {
            event.preventDefault()
            onToggle()
          }
        },
      }

  return (
    <Panel
      className={`media${open ? '' : ' media--collapsed'}`}
      aria-label={t('media.title')}
    >
      {open && (
        <div
          className="media__resize"
          role="separator"
          aria-orientation="horizontal"
          aria-label={t('media.resize')}
          onPointerDown={startResize}
        />
      )}
      <div className="media__header" {...collapsedBarProps}>
        <h2 className="media__title">{t('media.title')}</h2>
        {/* Collapsed, the tray is a slim labelled bar (the whole bar toggles):
            the tabs and per-tab actions fold away, leaving just the title and a
            plain expand chevron. */}
        {open && (
          <div className="media__tabs" role="tablist">
            {(['crates', 'generate', 'samples', 'folder'] as const).map((name) => (
              <Button
                key={name}
                lit={tab === name}
                role="tab"
                aria-selected={tab === name}
                aria-label={t(`media.tabs.${name}`)}
                onClick={() => {
                  setTab(name)
                  setHighlight(0)
                }}
              >
                {t(`media.tabs.${name}`)}
              </Button>
            ))}
          </div>
        )}
        {/* The right cluster: per-tab utility actions (Open Folder, so far)
            sit next to the collapse toggle, which is always present. */}
        <div className="media__header-end">
          {open && (tab === 'generate' || tab === 'samples') && (
            <div className="media__header-actions">
              <Button
                onClick={() =>
                  void (tab === 'generate' ? openSongsFolder() : openSamplesFolder())
                }
              >
                {t(
                  tab === 'generate'
                    ? 'media.generate.openFolder'
                    : 'media.samples.openFolder',
                )}
              </Button>
            </div>
          )}
          {open && (
            <Button
              lit={pianoWindowOpen}
              aria-pressed={pianoWindowOpen}
              aria-label={
                pianoWindowOpen ? t('media.pianoWindow.close') : t('media.pianoWindow.open')
              }
              title={
                pianoWindowOpen ? t('media.pianoWindow.close') : t('media.pianoWindow.open')
              }
              onClick={togglePianoWindow}
            >
              <PianoIcon />
            </Button>
          )}
          {open ? (
            <Button
              onClick={onToggle}
              aria-expanded={true}
              aria-label={t('media.collapse')}
              title={collapseHint}
            >
              ▾
            </Button>
          ) : (
            <span className="media__chevron" aria-hidden="true">
              ▴
            </span>
          )}
        </div>
      </div>

      <div
        className={`media__content${dragging ? ' media__content--dragging' : ''}`}
        style={{ height: open ? height : 0 }}
        aria-hidden={!open}
        inert={!open}
      >
      {tab === 'crates' && (
        <CrateBrowser
          presets={presets}
          onLoad={onLoadPreset}
          onDelete={onDeletePreset}
          onImport={onImportPresets}
        />
      )}

      {tab === 'generate' && (
        <div className="media__generate">
          <div className="media__generate-row">
            <div className="media__title-field">
              <TextField
                label={t('media.generate.title')}
                value={title}
                placeholder={t('media.generate.titlePlaceholder')}
                onChange={(event) => setTitle(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === 'Enter') generateTrack()
                }}
              />
            </div>
            <TextField
              label={t('media.generate.prompt')}
              value={prompt}
              onChange={(event) => setPrompt(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === 'Enter') generateTrack()
              }}
            />
            <Select
              label={t('media.generate.engine')}
              value={engine}
              options={TRACK_ENGINES.map((name) => ({
                value: name,
                label: t(`media.generate.engines.${name}`),
              }))}
              onChange={(value) => {
                const next = value as SongGenerationEngine
                setEngine(next)
                // Each engine has its own ceiling; snap into range.
                if (!ENGINE_LENGTHS[next].includes(seconds)) {
                  setSeconds(ENGINE_LENGTHS[next][1])
                }
              }}
            />
            <Select
              label={t('media.generate.length')}
              value={String(seconds)}
              options={lengths.map((length) => ({
                value: String(length),
                label: formatLength(length),
              }))}
              onChange={(value) => setSeconds(Number(value))}
            />
            <Button disabled={!prompt.trim() || fixedSeedInvalid} onClick={generateTrack}>
              {t('media.generate.action')}
            </Button>
          </div>
          <div className="media__generation-options">
            <LoraControl
              adapters={loras}
              kind={engine}
              value={trackStack.stack}
              onToggle={trackStack.toggle}
              onStrength={trackStack.setStrength}
              onToggleBypass={trackStack.toggleBypass}
            />
            {engine === 'track' ? (
              <SegmentedControl
                label={t('media.generate.mode.label')}
                value={generationMode}
                options={[
                  { value: 'basic', label: t('media.generate.mode.basic') },
                  { value: 'advanced', label: t('media.generate.mode.advanced') },
                ]}
                onChange={changeGenerationMode}
              />
            ) : null}
          </div>
          {engine === 'track' && generationMode === 'advanced' ? (
            <Sa3AdvancedControls
              value={steering}
              onChange={setSteering}
              onSubmit={generateTrack}
            />
          ) : null}
          {tracks.length === 0 ? (
            <p className="media__empty">{t('media.generate.empty')}</p>
          ) : (
            <ul className="media__list">
              {tracks.map((track) => {
                // Composed takes (those carrying a prompt) get a #id to tell same-title
                // siblings apart; an imported file shows just its name. The title
                // ellipsises in the row; the tag never shrinks.
                const composed = track.prompt != null
                const rowLabel = trackLabel(track)
                return (
                  <li
                    key={track.id}
                    className={`media__item${
                      track.id === highlightedReadyId
                        ? ' media__item--highlighted'
                        : ''
                    }`}
                  >
                    <span className="media__name">
                      {track.state === 'pending' && (
                        <span className="media__spinner" aria-hidden="true" />
                      )}
                      <span className="media__name-text">
                        {track.state === 'pending'
                          ? t('media.generate.pending', { title: track.title })
                          : track.title}
                      </span>
                      {track.state === 'ready' && composed && (
                        <span className="media__name-tag">{`#${track.id}`}</span>
                      )}
                      {track.state === 'ready' && track.prompt != null && (
                        <button
                          type="button"
                          className={`media__prompt${
                            expandedId === track.id ? ' media__prompt--expanded' : ''
                          }`}
                          aria-expanded={expandedId === track.id}
                          aria-label={t('media.generate.inspect', { name: rowLabel })}
                          title={prettyPrompt(track.prompt)}
                          onClick={() =>
                            setExpandedId((current) =>
                              current === track.id ? null : track.id,
                            )
                          }
                        >
                          {prettyPrompt(track.prompt)}
                        </button>
                      )}
                    </span>
                    <span className="media__meta">
                      {track.model == null
                        ? t('media.generate.imported')
                        : t(`media.generate.engines.${track.model}`)}
                    </span>
                    {track.state === 'ready' && hasVersionedRecipe(track.recipe) && (
                      <Button
                        aria-label={t('media.generate.reuseSettingsFor', { name: rowLabel })}
                        onClick={() => recallSongRecipe(track)}
                      >
                        {t('media.generate.reuseSettings')}
                      </Button>
                    )}
                    {track.state === 'ready' && (
                      <Button
                        aria-label={
                          previewingId === track.id
                            ? t('media.previewStop')
                            : t('media.preview', { name: rowLabel })
                        }
                        lit={previewingId === track.id}
                        onClick={() => void cueTrack(track)}
                      >
                        🎧
                      </Button>
                    )}
                    {track.state === 'ready' &&
                      loadButtons(
                        (deck) => void loadGeneratedTrack(deck, track),
                        rowLabel,
                      )}
                    {track.state === 'ready' && (
                      <Button
                        aria-label={t('media.remove', { name: rowLabel })}
                        onClick={() => void removeTrack(track)}
                      >
                        ✕
                      </Button>
                    )}
                  </li>
                )
              })}
            </ul>
          )}
          {generateError && (
            <p className="media__error" role="alert">
              {t('media.generate.failed', { message: generateError })}
            </p>
          )}
          {saveError && (
            <p className="media__error" role="alert">
              {saveError}
            </p>
          )}
          {recipeNotice && (
            <p className="media__generation-notice" role="status">
              {recipeNotice}
            </p>
          )}
        </div>
      )}

      {tab === 'samples' && (
        <div className="media__generate">
          <div className="media__generate-row">
            <div className="media__title-field">
              <TextField
                label={t('media.generate.title')}
                value={sampleTitle}
                placeholder={t('media.generate.titlePlaceholder')}
                onChange={(event) => setSampleTitle(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === 'Enter') composeSample()
                }}
              />
            </div>
            <TextField
              label={t('media.samples.prompt')}
              value={samplePrompt}
              onChange={(event) => setSamplePrompt(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === 'Enter') composeSample()
              }}
            />
            <Select
              label={t('media.generate.engine')}
              value={sampleEngine}
              options={SAMPLE_ENGINES.map((name) => ({
                value: name,
                label: t(`media.generate.engines.${name}`),
              }))}
              onChange={(value) => {
                const next = value as SampleEngine
                setSampleEngine(next)
                if (!ENGINE_LENGTHS[next].includes(sampleSeconds)) {
                  setSampleSeconds(ENGINE_LENGTHS[next][1])
                }
              }}
            />
            <Select
              label={t('media.generate.length')}
              value={String(sampleSeconds)}
              options={ENGINE_LENGTHS[sampleEngine].map((length) => ({
                value: String(length),
                label: formatLength(length),
              }))}
              onChange={(value) => setSampleSeconds(Number(value))}
            />
            <Button
              lit={sampleOneShot}
              aria-pressed={sampleOneShot}
              aria-label={t('media.samples.playbackMode')}
              onClick={() => setSampleOneShot((current) => !current)}
            >
              {t(sampleOneShot ? 'media.samples.oneShot' : 'media.samples.loop')}
            </Button>
            <Button disabled={!samplePrompt.trim()} onClick={composeSample}>
              {t('media.generate.action')}
            </Button>
          </div>
          <LoraControl
            adapters={loras}
            kind={sampleEngine}
            value={sampleStack.stack}
            onToggle={sampleStack.toggle}
            onStrength={sampleStack.setStrength}
            onToggleBypass={sampleStack.toggleBypass}
          />
          {samples.length === 0 ? (
            <p className="media__empty">{t('media.samples.empty')}</p>
          ) : (
            <ul className="media__list">
              {samples.map((sample) => {
                const composed = sample.prompt != null
                const rowLabel = sampleLabel(sample)
                return (
                  <li
                    key={sample.id}
                    className={`media__item${
                      sample.id === highlightedSampleId
                        ? ' media__item--highlighted'
                        : ''
                    }`}
                  >
                    <span className="media__name">
                      {sample.state === 'pending' && (
                        <span className="media__spinner" aria-hidden="true" />
                      )}
                      <span className="media__name-text">
                        {sample.state === 'pending'
                          ? t('media.generate.pending', { title: sample.title })
                          : sample.title}
                      </span>
                      {sample.state === 'ready' && composed && (
                        <span className="media__name-tag">{`#${sample.id}`}</span>
                      )}
                      {sample.state === 'ready' && sample.prompt != null && (
                        <button
                          type="button"
                          className={`media__prompt${
                            expandedId === sample.id ? ' media__prompt--expanded' : ''
                          }`}
                          aria-expanded={expandedId === sample.id}
                          aria-label={t('media.generate.inspect', { name: rowLabel })}
                          title={prettyPrompt(sample.prompt)}
                          onClick={() =>
                            setExpandedId((current) =>
                              current === sample.id ? null : sample.id,
                            )
                          }
                        >
                          {prettyPrompt(sample.prompt)}
                        </button>
                      )}
                    </span>
                    <span className="media__meta">
                      {`${sampleModelLabel(sample.model)} · ${t(
                        sample.oneShot ? 'media.samples.oneShot' : 'media.samples.loop',
                      )}`}
                    </span>
                    {sample.state === 'ready' && (
                      <Button
                        aria-label={
                          previewingId === sample.id
                            ? t('media.previewStop')
                            : t('media.preview', { name: rowLabel })
                        }
                        lit={previewingId === sample.id}
                        onClick={() => void cueSample(sample)}
                      >
                        🎧
                      </Button>
                    )}
                    {sample.state === 'ready' &&
                      loadButtons((deck) => void loadSample(deck, sample), rowLabel)}
                    {sample.state === 'ready' && (
                      <Button
                        aria-label={t('media.remove', { name: rowLabel })}
                        onClick={() => void removeSample(sample)}
                      >
                        ✕
                      </Button>
                    )}
                  </li>
                )
              })}
            </ul>
          )}
          {sampleError && (
            <p className="media__error" role="alert">
              {t('media.samples.failed', { message: sampleError })}
            </p>
          )}
          {sampleSaveError && (
            <p className="media__error" role="alert">
              {sampleSaveError}
            </p>
          )}
        </div>
      )}

      {tab === 'folder' && (
        <div className="media__folder">
          <div className="media__folder-row">
            <Button onClick={() => void chooseFolder()}>
              {t('media.folder.choose')}
            </Button>
            {folderName && (
              <span className="media__folder-name">{folderName}</span>
            )}
          </div>
          {folderName && files.length === 0 && (
            <p className="media__empty">
              {t('media.folder.empty', { name: folderName })}
            </p>
          )}
          {files.length > 0 && (
            <ul className="media__list">
              {files.map((file, index) => (
                <li
                  key={file.name}
                  className={`media__item${
                    index === Math.min(highlight, files.length - 1)
                      ? ' media__item--highlighted'
                      : ''
                  }`}
                >
                  <button
                    className="media__name media__name--button"
                    aria-label={t('media.highlight', { name: file.name })}
                    aria-current={index === Math.min(highlight, files.length - 1)}
                    onClick={() => setHighlight(index)}
                  >
                    <span className="media__name-text">{file.name}</span>
                  </button>
                  {loadButtons((deck) => void loadFolderFile(deck, file), file.name)}
                </li>
              ))}
            </ul>
          )}
          {folderError && (
            <p className="media__error" role="alert">
              {folderError}
            </p>
          )}
        </div>
      )}
      </div>
    </Panel>
  )
}
