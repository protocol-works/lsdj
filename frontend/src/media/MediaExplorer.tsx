import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import type {
  DragEvent as ReactDragEvent,
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
  subscribeMcpGeneration,
  togglePianoWindow,
  type DeckSnap,
} from '../audio/nativeEngine'
import { useInterfaceStore } from '../audio/interfaceStore'
import { useControlBus } from '../control/busContext'
import { CrateBrowser } from '../crates/CrateBrowser'
import {
  postMagentaRender,
  postSa3Generate,
  postSa3GenerateWithSource,
} from '../generation/client'
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
  SOURCE_MODES,
  SOURCE_NOISE_DEFAULT,
  buildSongGeneration,
  clipSeconds,
  mintSa3Seed,
  parseFixedSeed,
  parseGenerationRecipe,
  sourceDraftOf,
  sourceWindow,
  withSource,
  type GenerationMode,
  type Sa3SteeringDraft,
  type SongGenerationBuild,
  type SongGenerationEngine,
  type SongGenerationRecipeV1,
  type SourceDraft,
  type TrackGenerationRequest,
} from '../generation/songGeneration'
import { Sa3AdvancedControls } from '../generation/Sa3AdvancedControls'
import { loadAppSettings, updateAppSettings } from '../persistence'
import { LoraControl } from '../ui/LoraControl'
import type { StylePreset } from '../presets'
import { matchesSearch } from '../search'
import { ActionMenu, type ActionMenuItem } from '../ui/ActionMenu'
import { Button } from '../ui/Button'
import { ChecklistPanel } from '../ui/ChecklistPanel'
import { Panel } from '../ui/Panel'
import { PianoIcon } from '../ui/PianoIcon'
import { Rating } from '../ui/Rating'
import { Select } from '../ui/Select'
import { SegmentedControl } from '../ui/SegmentedControl'
import { Slider } from '../ui/Slider'
import { TextField } from '../ui/TextField'
import { insertFile, moveFile, removeFile } from './playlistOrder'
import { randomSongTitle } from './songTitle'
import { sortRows, type MediaSort } from './sortRows'
import { CropPanel } from './CropPanel'
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
  /** 1–5; absent when unrated. */
  rating?: number
  /** Absent when untagged. */
  tags?: string[]
  /** Background analysis (Rust `analysis::media::TrackInfo`); absent until it
   * runs, and when it wasn't confident. `key` is Camelot ("8A"). */
  bpm?: number
  key?: string
  keyName?: string
}

// One row of the on-disk sample registry (Rust `samples::SampleEntry`, camelCase):
// like a SongEntry plus `oneShot`, the loop-vs-one-shot verdict reload needs.
type SampleEntry = {
  file: string
  title: string
  prompt: string | null
  model: string | null
  oneShot: boolean
  rating?: number
  tags?: string[]
  bpm?: number
  key?: string
  keyName?: string
}

type GeneratedTrack =
  | {
      id: number
      state: 'pending'
      title: string
      prompt: string
      model: TrackEngine
      // Absent for an MCP agent's generation — the pending row is only a mirror
      // of the shell-side job, retired on done/error, never flipped to ready.
      recipe?: SongGenerationRecipeV1
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
      // 0 = unrated, else 1–5 — the registry's rating.
      rating: number
      // The registry's tags. Carried for the tag chips (curation phase 4).
      tags: string[]
      // The registry's analysis, as on SongEntry.
      bpm?: number
      key?: string
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
      // `magenta` only for an MCP agent's generation (the in-app Samples tab
      // keeps the SFX/Music engines).
      model: SampleEngine | 'magenta'
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
      // As on a GeneratedTrack: 0 = unrated, the registry's tags and analysis.
      rating: number
      tags: string[]
      bpm?: number
      key?: string
    }

// One playlist (Rust `playlists::Playlist`): an ordered list of song files, already
// reconciled against the songs folder and deduped by the shell.
type Playlist = { name: string; files: string[] }

// The sort in a playlist view: the stored order, or a library sort applied to the
// view only (the stored order changes only by reordering).
type PlaylistSort = 'playlist' | MediaSort

/** Name order as the shell lists playlists (ignoring case). */
function byPlaylistName(a: Playlist, b: Playlist): number {
  return a.name.localeCompare(b.name, undefined, { sensitivity: 'base' })
}

/** Whether a drag is over the lower half of `element` — a drop lands after it. */
function dropsAfter(event: ReactDragEvent<HTMLElement>): boolean {
  const box = event.currentTarget.getBoundingClientRect()
  return event.clientY > box.top + box.height / 2
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

/** A row's analysis for its meta line (" · 124 BPM · 8A"), '' with no verdict. */
function analysisMeta(
  row: { state: string; bpm?: number; key?: string },
  bpmLabel: (bpm: number) => string,
): string {
  const parts = [row.bpm == null ? null : bpmLabel(Math.round(row.bpm)), row.key]
  return parts.map((part) => (part ? ` · ${part}` : '')).join('')
}

function formatLength(seconds: number): string {
  const whole = Math.round(seconds)
  return `${Math.floor(whole / 60)}:${String(whole % 60).padStart(2, '0')}`
}

/** A recalled recipe's length for the Length menu: kept as is when the engine
 * can render it (the menu grows a custom option), else the nearest preset. */
function recallLength(engine: TrackEngine, seconds: number): number {
  const presets = ENGINE_LENGTHS[engine]
  if (seconds > 0 && seconds <= presets[presets.length - 1]) return seconds
  return presets.reduce((best, length) =>
    Math.abs(length - seconds) < Math.abs(best - seconds) ? length : best,
  )
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
 * saved. The key a live re-list matches an existing row by, so its id (and any
 * in-memory `wav`) survive a refresh instead of churning. */
function fileOf(row: { state: string; file?: string | null }): string | null {
  return row.state === 'ready' ? (row.file ?? null) : null
}

/** Every tag the ready rows carry, alphabetically, once each. */
function tagsOf(rows: Array<{ state: string; tags?: string[] }>): string[] {
  return [...new Set(rows.flatMap((row) => (row.state === 'ready' ? (row.tags ?? []) : [])))].sort(
    (a, b) => a.localeCompare(b),
  )
}

/** Whether a row carries every tag in `filter` (a pending row carries none). */
function hasTags(row: { state: string; tags?: string[] }, filter: string[]): boolean {
  return filter.every((tag) => row.state === 'ready' && (row.tags ?? []).includes(tag))
}

function hasVersionedRecipe(value: unknown): boolean {
  return (
    value != null &&
    typeof value === 'object' &&
    typeof (value as { version?: unknown }).version === 'number'
  )
}

/** The take's length from a versioned recipe, or null (an older or imported song). */
function recipeSeconds(recipe: unknown): number | null {
  const seconds = hasVersionedRecipe(recipe)
    ? (recipe as { seconds?: unknown }).seconds
    : undefined
  return typeof seconds === 'number' ? seconds : null
}

/** A song row's registry-owned fields. A re-list and every command's returned entry
 * rebuild exactly these; only the row's id and in-memory wav are session state. */
function songRowFields(entry: SongEntry) {
  return {
    state: 'ready' as const,
    title: entry.title,
    prompt: entry.prompt,
    model: asTrackEngine(entry.model),
    file: entry.file,
    recipe: entry.recipe,
    rating: entry.rating ?? 0,
    tags: entry.tags ?? [],
    bpm: entry.bpm,
    key: entry.key,
  }
}

/** A sample row's registry-owned fields (see {@link songRowFields}). */
function sampleRowFields(entry: SampleEntry) {
  return {
    state: 'ready' as const,
    title: entry.title,
    prompt: entry.prompt,
    model: entry.model,
    file: entry.file,
    oneShot: entry.oneShot,
    rating: entry.rating ?? 0,
    tags: entry.tags ?? [],
    bpm: entry.bpm,
    key: entry.key,
  }
}

/** Re-list one library (songs or samples) from its on-disk registry, reconciled
 * against the folder by the Rust shell (hand-added files appear; deleted files drop
 * out). Every row is rebuilt from its registry entry, so an out-of-band write (an MCP
 * rating, a resync after a failed command) always shows; a row already held for the
 * file hands `toRow` only its id + in-memory wav, so a live re-list never churns. A
 * row whose file vanished is dropped; an in-session take not yet on disk is kept.
 * `ref` is read after the fetch resolves (freshest), and the id mint (`toRow`) runs
 * OUTSIDE the state updater — StrictMode replays updaters, so they must be pure. A
 * no-op outside Tauri. */
function reListLibrary<
  R extends { id: number; state: string; file?: string | null; wav?: ArrayBuffer },
  E extends { file: string },
>(
  command: string,
  ref: { current: R[] },
  setRows: (next: (current: R[]) => R[]) => void,
  toRow: (entry: E, kept: { id: number; wav?: ArrayBuffer } | undefined) => R,
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
    const restored = entries.map((entry) => {
      const prior = byFile.get(entry.file)
      return toRow(entry, prior && { id: prior.id, wav: prior.wav })
    })
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
  const interfaceState = useInterfaceStore()
  const pianoWindowOpen = interfaceState?.pianoWindowOpen ?? false
  const [tab, setTab] = useState<MediaTab>('crates')
  const [search, setSearch] = useState('')
  // Tags every listed row must carry. One set for both tabs; each tab applies only
  // the tags its own rows use, so a samples-only tag never empties the songs list.
  const [tagFilter, setTagFilter] = useState<string[]>([])
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
  // A library song the next Track take grows from (audio-to-audio), or none.
  const [source, setSource] = useState<SourceDraft | null>(null)
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
  const highlightedRow = useRef<HTMLLIElement>(null)
  // The take whose prompt is expanded to full text, or null. One at a time; the
  // rest stay truncated to one line.
  const [expandedId, setExpandedId] = useState<number | null>(null)
  // The row whose title is open in the inline rename editor, or null. One at a time,
  // like expandedId.
  const [editingId, setEditingId] = useState<number | null>(null)
  // The row whose tag panel is open, or null — one at a time, like editingId.
  // `focusAdd` is the `t` fast path: open on the add field, not the first tag.
  const [tagging, setTagging] = useState<{ id: number; focusAdd: boolean } | null>(null)
  // The row whose "Add to playlist…" panel is open, or null — one at a time.
  const [playlistPanelId, setPlaylistPanelId] = useState<number | null>(null)
  // The row whose Crop… panel is open, or null.
  const [croppingId, setCroppingId] = useState<number | null>(null)
  // The menu wrapper of the row whose tag or playlist panel is open, that panel's
  // anchor (ActionMenu keeps its trigger ref internal).
  const panelAnchorRef = useRef<HTMLSpanElement>(null)
  // Generate/Samples order, a persisted preference.
  const [mediaSort, setMediaSort] = useState<MediaSort>(
    () => loadAppSettings().mediaSort ?? 'newest',
  )
  // The playlists (the shell's list, adopted after every write) and the Songs tab's
  // selected source: 'all' or a playlist name, persisted.
  const [playlists, setPlaylists] = useState<Playlist[]>([])
  const [mediaSource, setMediaSource] = useState(() => loadAppSettings().mediaSource ?? 'all')
  // A playlist view's sort, session-only; every playlist opens in its own order.
  const [playlistSort, setPlaylistSort] = useState<PlaylistSort>('playlist')
  // The inline name editors: a new playlist in the column, and the open playlist's
  // rename in its header.
  const [creatingPlaylist, setCreatingPlaylist] = useState(false)
  const [renamingPlaylist, setRenamingPlaylist] = useState(false)
  // The drag-and-drop targets under the pointer: a playlist in the column, and a row
  // (by file) in a playlist view with the side the drop lands on.
  const [dropSource, setDropSource] = useState<string | null>(null)
  const [dropRow, setDropRow] = useState<{ file: string; after: boolean } | null>(null)
  // Which row each deck was loaded with from here (row id captured at load time —
  // titles change under a rename), per library: a deck holds one track but may hold
  // a sample in a loop slot alongside it. Session-only.
  const [trackOnDeck, setTrackOnDeck] = useState<Partial<Record<DeckId, number>>>({})
  const [sampleOnDeck, setSampleOnDeck] = useState<Partial<Record<DeckId, number>>>({})
  // Library files loaded onto a deck this session, shown muted ("played") so a set
  // reads as done / playing / next. Session-only by design.
  const [playedFiles, setPlayedFiles] = useState<ReadonlySet<string>>(() => new Set())
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

  // The selected playlist, or null for All songs (also when the persisted name no
  // longer names a playlist). A playlist view lists its files' rows in playlist order;
  // a file with no row is skipped, and a pending row (no file) never shows.
  const playlist = playlists.find((entry) => entry.name === mediaSource) ?? null
  const songRows = useMemo(() => {
    if (playlist == null) return tracks
    const byFile = new Map(tracks.map((track) => [fileOf(track), track]))
    return playlist.files.flatMap((file) => byFile.get(file) ?? [])
  }, [playlist, tracks])
  const songTags = tagsOf(songRows)
  const songTagFilter = tagFilter.filter((tag) => songTags.includes(tag))
  const filteredTracks = songRows.filter((track) =>
    hasTags(track, songTagFilter) &&
    matchesSearch(
      search,
      track.title,
      track.prompt,
      fileOf(track),
      track.model,
      track.model == null
        ? t('media.generate.imported')
        : t(`media.generate.engines.${track.model}`),
      // As `#drone`, so `#drone` matches tags only and `drone` tags and text alike.
      ...(track.state === 'ready' ? track.tags.map((tag) => '#' + tag) : []),
    ),
  )
  // The sorted, filtered order is both what renders and what the rotary walks.
  const sortedTracks =
    playlist == null
      ? sortRows(filteredTracks, mediaSort)
      : playlistSort === 'playlist'
        ? filteredTracks
        : sortRows(filteredTracks, playlistSort)
  const ready = sortedTracks.filter(
    (track): track is GeneratedTrack & { state: 'ready' } =>
      track.state === 'ready',
  )
  const highlightedTrack =
    ready.length === 0 ? null : ready[Math.min(highlight, ready.length - 1)]
  const highlightedReadyId = highlightedTrack?.id ?? null

  const sampleTags = tagsOf(samples)
  const sampleTagFilter = tagFilter.filter((tag) => sampleTags.includes(tag))
  const filteredSamples = samples.filter((sample) =>
    hasTags(sample, sampleTagFilter) &&
    matchesSearch(
      search,
      sample.title,
      sample.prompt,
      fileOf(sample),
      sample.model,
      sampleModelLabel(sample.model),
      t(sample.oneShot ? 'media.samples.oneShot' : 'media.samples.loop'),
      ...(sample.state === 'ready' ? sample.tags.map((tag) => '#' + tag) : []),
    ),
  )
  const sortedSamples = sortRows(filteredSamples, mediaSort)
  const readySamples = sortedSamples.filter(
    (sample): sample is GeneratedSample & { state: 'ready' } =>
      sample.state === 'ready',
  )
  const highlightedSample =
    readySamples.length === 0
      ? null
      : readySamples[Math.min(highlight, readySamples.length - 1)]
  const highlightedSampleId = highlightedSample?.id ?? null
  const filteredFiles = files.filter((file) => matchesSearch(search, file.name))
  const highlightedFileIndex =
    filteredFiles.length === 0 ? -1 : Math.min(highlight, filteredFiles.length - 1)
  const highlightedFileName =
    highlightedFileIndex < 0 ? null : filteredFiles[highlightedFileIndex].name

  // Controller browsing changes the highlight without moving DOM focus, so keep
  // the selected row inside the tray's scrollable viewport. Crates does the same
  // inside CrateBrowser, which owns that tab's independent highlight.
  useLayoutEffect(() => {
    if (tab === 'crates') return
    highlightedRow.current?.scrollIntoView?.({ block: 'nearest' })
  }, [tab, highlightedReadyId, highlightedSampleId, highlightedFileName])

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
      if (!loaded) {
        setGenerateError(t('media.undecodable', { title: label }))
        return
      }
      setTrackOnDeck((current) => ({ ...current, [deck]: track.id }))
      markPlayed(track.file)
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
      // The shell strips the file from every playlist too.
      refreshPlaylists()
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
      if (!loaded) {
        setSampleError(t('media.samples.loadFailed', { title: label }))
        return
      }
      setSampleOnDeck((current) => ({ ...current, [deck]: sample.id }))
      markPlayed(sample.file)
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
  // startup, by the folder watcher, and to resync after a failed rate/rename.
  const refreshSongs = useCallback(
    () =>
      reListLibrary<GeneratedTrack, SongEntry>(
        'list_generated_songs',
        tracksRef,
        setTracks,
        (entry, kept) => ({
          id: kept?.id ?? nextIdRef.current++,
          wav: kept?.wav,
          ...songRowFields(entry),
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
        (entry, kept) => ({
          id: kept?.id ?? nextIdRef.current++,
          wav: kept?.wav,
          ...sampleRowFields(entry),
        }),
      ),
    [],
  )

  const refreshPlaylists = useCallback(() => {
    if (!isTauri()) return
    invoke<Playlist[]>('list_playlists')
      .then((list) => setPlaylists(list ?? []))
      .catch(() => {}) // a failed scan just means no refresh, as for the libraries
  }, [])

  /** Put a playlist the shell returned (or an optimistic one) in place of
   * `previousName`, keeping the column in name order. */
  function adoptPlaylist(previousName: string, stored: Playlist) {
    setPlaylists((current) =>
      [
        ...current.filter((entry) => entry.name !== previousName && entry.name !== stored.name),
        stored,
      ].sort(byPlaylistName),
    )
  }

  function playlistFailed(name: string, error: unknown) {
    setSaveError(
      t('media.playlists.failed', {
        name,
        message: error instanceof Error ? error.message : String(error),
      }),
    )
    refreshPlaylists()
  }

  /** Create or replace a playlist with exactly `files` — create, add, remove and
   * reorder alike send the whole list. Optimistic, then adopts the stored playlist
   * (a new name comes back sanitised); a failure surfaces and re-lists. Resolves to
   * the stored playlist, or undefined on failure. */
  function savePlaylist(name: string, files: string[]): Promise<Playlist | undefined> {
    setSaveError(null)
    adoptPlaylist(name, { name, files })
    return invoke<Playlist>('save_playlist', { name, files })
      .then((stored) => {
        adoptPlaylist(name, stored)
        return stored
      })
      .catch((error: unknown) => {
        playlistFailed(name, error)
        return undefined
      })
  }

  // Stable for the memoised source Select (see ui/Select).
  const selectSource = useCallback((source: string) => {
    setMediaSource(source)
    setPlaylistSort('playlist')
    setRenamingPlaylist(false)
    setHighlight(0)
    updateAppSettings({ mediaSource: source })
  }, [])

  /** A name that already names a playlist (ignoring case, as the shell's collision
   * check does) — saving under it would replace that playlist's songs. */
  const playlistNamed = (name: string) =>
    playlists.find((entry) => entry.name.toLowerCase() === name.toLowerCase())

  function createPlaylist(name: string) {
    const existing = playlistNamed(name)
    if (existing) {
      selectSource(existing.name)
      return
    }
    void savePlaylist(name, []).then((stored) => stored && selectSource(stored.name))
  }

  function renamePlaylist(from: string, to: string) {
    setSaveError(null)
    invoke<Playlist>('rename_playlist', { from, to })
      .then((stored) => {
        adoptPlaylist(from, stored)
        if (mediaSource === from) selectSource(stored.name)
      })
      .catch((error: unknown) => playlistFailed(from, error))
  }

  function deletePlaylist(name: string) {
    setSaveError(null)
    invoke('delete_playlist', { name })
      .then(() => {
        setPlaylists((current) => current.filter((entry) => entry.name !== name))
        selectSource('all')
      })
      .catch((error: unknown) => playlistFailed(name, error))
  }

  /** Add a dragged song file to a playlist (once — a repeat drop is a no-op). Only a
   * file the songs library holds is accepted; drag data can come from anywhere. */
  function dropOnPlaylist(target: Playlist, file: string) {
    if (target.files.includes(file) || !tracks.some((track) => fileOf(track) === file)) return
    void savePlaylist(target.name, insertFile(target.files, file))
  }

  /** A row of the open playlist dropped on another: it moves to the drop point.
   * Anything else dragged in (not one of its rows) is ignored. */
  function dropOnRow(target: Playlist, rowFile: string, after: boolean, file: string) {
    const from = target.files.indexOf(file)
    if (from < 0) return
    const at = target.files.indexOf(rowFile) + (after ? 1 : 0)
    // `at` counts the dragged row itself when it sits above the drop point.
    const to = at > from ? at - 1 : at
    if (to !== from) void savePlaylist(target.name, moveFile(target.files, from, to))
  }

  /** Set fields on a library row optimistically, persist them through the shell, and
   * adopt the row the shell returns (its normalised values). A failure surfaces
   * `failedKey` and re-lists to resync. A take with no file (outside Tauri, or a
   * failed save) has no registry row, so it only changes in memory — the same branch
   * removeTrack takes. */
  function mutateTrack(
    track: GeneratedTrack & { state: 'ready' },
    fields: { rating?: number; title?: string; tags?: string[] },
    command: string,
    args: Record<string, unknown>,
    failedKey: string,
  ) {
    const patch = (next: Partial<GeneratedTrack & { state: 'ready' }>) =>
      setTracks((current) =>
        current.map((row) =>
          row.id === track.id && row.state === 'ready' ? { ...row, ...next } : row,
        ),
      )
    patch(fields)
    if (!isTauri() || !track.file) return
    setSaveError(null)
    invoke<SongEntry>(command, { name: track.file, ...args })
      .then((entry) => patch(songRowFields(entry)))
      .catch((error: unknown) => {
        setSaveError(
          t(failedKey, {
            title: track.title,
            message: error instanceof Error ? error.message : String(error),
          }),
        )
        refreshSongs()
      })
  }

  /** {@link mutateTrack} for the samples library. */
  function mutateSample(
    sample: GeneratedSample & { state: 'ready' },
    fields: { rating?: number; title?: string; tags?: string[] },
    command: string,
    args: Record<string, unknown>,
    failedKey: string,
  ) {
    const patch = (next: Partial<GeneratedSample & { state: 'ready' }>) =>
      setSamples((current) =>
        current.map((row) =>
          row.id === sample.id && row.state === 'ready' ? { ...row, ...next } : row,
        ),
      )
    patch(fields)
    if (!isTauri() || !sample.file) return
    setSampleSaveError(null)
    invoke<SampleEntry>(command, { name: sample.file, ...args })
      .then((entry) => patch(sampleRowFields(entry)))
      .catch((error: unknown) => {
        setSampleSaveError(
          t(failedKey, {
            title: sample.title,
            message: error instanceof Error ? error.message : String(error),
          }),
        )
        refreshSamples()
      })
  }

  // 0 clears: the shell stores an unrated row as an absent rating (null on the wire).
  const rateTrack = (track: GeneratedTrack & { state: 'ready' }, rating: number) =>
    mutateTrack(
      track,
      { rating },
      'rate_generated_song',
      { rating: rating || null },
      'media.rating.failed',
    )
  const rateSample = (sample: GeneratedSample & { state: 'ready' }, rating: number) =>
    mutateSample(
      sample,
      { rating },
      'rate_generated_sample',
      { rating: rating || null },
      'media.rating.failed',
    )
  const renameTrack = (track: GeneratedTrack & { state: 'ready' }, title: string) =>
    mutateTrack(track, { title }, 'rename_generated_song', { title }, 'media.rename.failed')
  const renameSample = (sample: GeneratedSample & { state: 'ready' }, title: string) =>
    mutateSample(sample, { title }, 'rename_generated_sample', { title }, 'media.rename.failed')

  // The whole list goes to the shell every time; the row adopts its normalised tags.
  const tagTrack = (track: GeneratedTrack & { state: 'ready' }, tags: string[]) =>
    mutateTrack(track, { tags }, 'tag_generated_song', { tags }, 'media.tags.failed')
  const tagSample = (sample: GeneratedSample & { state: 'ready' }, tags: string[]) =>
    mutateSample(sample, { tags }, 'tag_generated_sample', { tags }, 'media.tags.failed')

  /** Rename the row open in the editor. Ids are unique across both libraries (one
   * mint), so the editing id names exactly one row. */
  function commitRename(title: string) {
    const track = tracks.find((row) => row.id === editingId)
    if (track?.state === 'ready') renameTrack(track, title)
    const sample = samples.find((row) => row.id === editingId)
    if (sample?.state === 'ready') renameSample(sample, title)
  }

  /** Replace the tagged row's tags (see {@link commitRename} for the id lookup). */
  function commitTags(id: number, tags: string[]) {
    const track = tracks.find((row) => row.id === id)
    if (track?.state === 'ready') tagTrack(track, tags)
    const sample = samples.find((row) => row.id === id)
    if (sample?.state === 'ready') tagSample(sample, tags)
  }

  function markPlayed(file: string | null) {
    if (file) setPlayedFiles((current) => new Set(current).add(file))
  }

  // Restore the lists at startup; the folder watcher keeps them live thereafter.
  useEffect(() => {
    refreshSongs()
    refreshSamples()
    refreshPlaylists()
  }, [refreshSongs, refreshSamples, refreshPlaylists])

  // Live-reload from the Rust folder watcher (ADR-0022): a deck auto-saving a sample,
  // or a file dropped in / deleted by hand, fires `library://changed`; re-list the
  // named library; an MCP agent's playlist write names 'playlists'. A songs change
  // re-lists the playlists too, since the shell reconciles them against the folder.
  // A no-op outside Tauri (the event bridge is absent).
  useEffect(
    () =>
      subscribeLibraryChanged((library) => {
        if (library === 'samples') {
          refreshSamples()
          return
        }
        if (library === 'songs') refreshSongs()
        refreshPlaylists()
      }),
    [refreshSongs, refreshSamples, refreshPlaylists],
  )

  // An MCP agent's generation (Rust emits mcp://generation): mirror the same
  // pending row the in-app compose flows show, so the co-DJ's multi-second work
  // is visible. done/error retires the row; the re-list (here and from the
  // folder watcher) surfaces the finished take. `job` → row id lives in a ref —
  // the pairing is bookkeeping, not render state.
  const mcpJobsRef = useRef(new Map<number, number>())
  useEffect(
    () =>
      subscribeMcpGeneration((event) => {
        if (event.phase === 'start') {
          const id = nextIdRef.current++
          mcpJobsRef.current.set(event.job, id)
          if (event.kind === 'track') {
            setTracks((current) => [
              {
                id,
                state: 'pending',
                title: event.title,
                prompt: event.prompt,
                model: 'track',
              },
              ...current,
            ])
          } else {
            setSamples((current) => [
              {
                id,
                state: 'pending',
                title: event.title,
                prompt: event.prompt,
                model: event.kind === 'magenta' ? 'magenta' : (event.kind as SampleEngine),
                oneShot: event.oneShot,
              },
              ...current,
            ])
          }
          return
        }
        const id = mcpJobsRef.current.get(event.job)
        if (id == null) return
        mcpJobsRef.current.delete(event.job)
        if (event.kind === 'track') {
          setTracks((current) => current.filter((track) => track.id !== id))
          if (event.phase === 'done') refreshSongs()
        } else {
          setSamples((current) => current.filter((sample) => sample.id !== id))
          if (event.phase === 'done') refreshSamples()
        }
      }),
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
            : filteredFiles.length
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
          void loadFolderFile(intent.deck, filteredFiles[index])
        }
      }
    }),
  )

  // Keys 1–5 rate the highlighted row and 0 clears it: audition with 🎧, press a
  // number; `t` opens its tag panel on the add field. Handled here, not in
  // shortcuts.ts (focus-only). Typing is left alone —
  // the search, the compose fields, the rename editor — but a focused star radio
  // isn't typing, so rating with the mouse doesn't disarm the keys.
  useEffect(() => {
    if (!open || (tab !== 'generate' && tab !== 'samples')) return
    function onKeyDown(event: KeyboardEvent) {
      if (!/^[0-5t]$/.test(event.key)) return
      if (event.ctrlKey || event.metaKey || event.altKey || event.shiftKey) return
      const target = event.target
      if (
        (target instanceof HTMLInputElement &&
          target.type !== 'radio' &&
          target.type !== 'checkbox') ||
        target instanceof HTMLSelectElement ||
        target instanceof HTMLTextAreaElement ||
        (target instanceof HTMLElement && target.isContentEditable)
      ) {
        return
      }
      if (event.key === 't') {
        const row = tab === 'generate' ? highlightedTrack : highlightedSample
        if (!row) return
        // Or the `t` lands in the add field the panel focuses.
        event.preventDefault()
        setPlaylistPanelId(null)
        setTagging({ id: row.id, focusAdd: true })
        return
      }
      const rating = Number(event.key)
      if (tab === 'generate') {
        if (highlightedTrack) rateTrack(highlightedTrack, rating)
      } else if (highlightedSample) {
        rateSample(highlightedSample, rating)
      }
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  })

  /** The deck this row was loaded onto from here and that still holds it, or null.
   * "Still holds" is the store's read-back of the label the row was loaded with —
   * the deck's track for a song, a loop slot for a sample — so an eject, another
   * load onto that deck, or a rename (the deck keeps the old label) retires it.
   * Without a store snapshot (no shell) the load itself is the only evidence. */
  function deckHolding(
    onDeck: Partial<Record<DeckId, number>>,
    rowId: number,
    holds: (deck: DeckSnap) => boolean,
  ): DeckId | null {
    return (
      (['a', 'b'] as const).find((deck) => {
        if (onDeck[deck] !== rowId) return false
        const snap = interfaceState?.decks[deck === 'a' ? 0 : 1]
        return interfaceState == null || (snap != null && holds(snap))
      }) ?? null
    )
  }

  const sortOptions = useMemo(
    () =>
      (['newest', 'rating', 'title', 'bpm', 'key'] as const).map((value) => ({
        value,
        label: t(`media.sort.${value}`),
      })),
    [t],
  )
  // In a playlist view "Newest" would just be the playlist order, so it gives way.
  const playlistSortOptions = useMemo(
    () =>
      (['playlist', 'rating', 'title', 'bpm', 'key'] as const).map((value) => ({
        value,
        label: t(`media.sort.${value}`),
      })),
    [t],
  )
  const sourceOptions = useMemo(
    () => [
      { value: 'all', label: t('media.playlists.all') },
      ...playlists.map((entry) => ({ value: entry.name, label: entry.name })),
    ],
    [playlists, t],
  )
  // Stable for the memoised Select (see ui/Select).
  const changeSort = useCallback((value: string) => {
    const next = value as MediaSort
    setMediaSort(next)
    setHighlight(0)
    updateAppSettings({ mediaSort: next })
  }, [])
  // Never persisted: a playlist's own order is its default.
  const changePlaylistSort = useCallback((value: string) => {
    setPlaylistSort(value as PlaylistSort)
    setHighlight(0)
  }, [])
  const playlistView = tab === 'generate' && playlist != null
  const starLabel = (count: number) => t('media.rating.star', { count })

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
                  rating: 0,
                  tags: [],
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
    let generation: SongGenerationBuild
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
    const requestSource = requestEngine === 'track' ? source : null
    const window = requestSource ? sourceWindow(requestSource) : null
    if (typeof window === 'string') {
      setGenerateError(t(`media.generate.source.errors.${window}`))
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
        let wav: ArrayBuffer
        if (requestSource && window) {
          // The shell cuts the window to the WAV shape SA3 takes; its length
          // decides the output length (remix/replace) or where continuing starts.
          const clip = await invoke<ArrayBuffer>('song_source_clip', {
            name: requestSource.file,
            start: window.start,
            end: window.end ?? null,
          })
          const clipLength = clipSeconds(clip)
          const sourced = withSource(generation, requestSource, window.start, clipLength)
          if (typeof sourced === 'string') {
            throw new Error(
              t(`media.generate.source.errors.${sourced}`, {
                seconds: clipLength.toFixed(1),
              }),
            )
          }
          generation = sourced
          wav = await postSa3GenerateWithSource(
            sourced.request as TrackGenerationRequest,
            clip,
          )
        } else {
          wav =
            'kind' in generation.request
              ? await postSa3Generate(generation.request)
              : await postMagentaRender(generation.request)
        }
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
                  rating: 0,
                  tags: [],
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

    setPrompt(recipe.prompt)
    setEngine(recipe.engine)
    // The MCP agent asks for any length (142 s, 213 s…), so a recipe's length
    // is kept as an extra Length option rather than refused for not being a preset.
    setSeconds(recallLength(recipe.engine, recipe.seconds))
    const sourceFile = recipe.source?.file
    setSource(
      recipe.source
        ? sourceDraftOf(
            recipe.source,
            tracks.find((row) => row.state === 'ready' && row.file === sourceFile)?.title ??
              recipe.source.file,
          )
        : null,
    )
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

  // A recalled off-preset length (an MCP take) shows as its own option.
  const lengths = ENGINE_LENGTHS[engine].includes(seconds)
    ? ENGINE_LENGTHS[engine]
    : [...ENGINE_LENGTHS[engine], seconds].sort((a, b) => a - b)
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

  /** The inline rename editor that replaces a row's name. Enter and blur commit,
   * Escape cancels; Enter/Escape just blur, so blur is the single commit path (an
   * unmount can't commit twice). A trimmed-unchanged or empty title is a silent
   * cancel — which is how Escape cancels: it puts the original title back into the
   * (uncontrolled) field before blurring. */
  function renameEditor(name: string, currentTitle: string) {
    return (
      <TextField
        labelHidden
        label={t('media.rename.label', { name })}
        defaultValue={currentTitle}
        autoFocus
        onKeyDown={(event) => {
          if (event.key === 'Escape') event.currentTarget.value = currentTitle
          if (event.key === 'Enter' || event.key === 'Escape') event.currentTarget.blur()
        }}
        onBlur={(event) => {
          const next = event.currentTarget.value.trim()
          setEditingId(null)
          if (next && next !== currentTitle) commitRename(next)
        }}
      />
    )
  }

  /** {@link renameEditor} for a playlist name (a new one, or the open one's rename):
   * `onDone` gets the trimmed text on blur and treats empty or unchanged as a
   * cancel. (The row editor stays separate: routing its commit through a callback
   * argument trips the compiler's ref-in-render check.) */
  function playlistNameEditor(current: string, onDone: (next: string) => void) {
    return (
      <TextField
        labelHidden
        label={t('media.playlists.nameLabel')}
        defaultValue={current}
        autoFocus
        onKeyDown={(event) => {
          if (event.key === 'Escape') event.currentTarget.value = current
          if (event.key === 'Enter' || event.key === 'Escape') event.currentTarget.blur()
        }}
        onBlur={(event) => onDone(event.currentTarget.value.trim())}
      />
    )
  }

  /** The row's "On A" badge while a deck holds it, else nothing. */
  function onDeckBadge(deck: DeckId | null) {
    return (
      deck && (
        <span className="media__on-deck">
          {t('media.onDeck', { deck: deck.toUpperCase() })}
        </span>
      )
    )
  }

  function toggleTagFilter(tag: string) {
    setTagFilter((current) =>
      current.includes(tag) ? current.filter((each) => each !== tag) : [...current, tag],
    )
    setHighlight(0)
  }

  /** Tags as filter toggles — a row's own chips, and the bar above a list. */
  function tagChips(tags: string[]) {
    return tags.map((tag) => (
      <button
        key={tag}
        type="button"
        className={`media__tag${tagFilter.includes(tag) ? ' media__tag--on' : ''}`}
        aria-label={t('media.tags.filterBy', { tag })}
        aria-pressed={tagFilter.includes(tag)}
        onClick={() => toggleTagFilter(tag)}
      >
        {`#${tag}`}
      </button>
    ))
  }

  /** Every tag the tab's rows carry, with a clear control once one is on. */
  function tagFilterBar(tags: string[], active: string[]) {
    if (tags.length === 0) return null
    return (
      <div className="media__tag-filter" role="group" aria-label={t('media.tags.filterLabel')}>
        {tagChips(tags)}
        {active.length > 0 && (
          <Button
            onClick={() => {
              setTagFilter([])
              setHighlight(0)
            }}
          >
            {t('media.tags.clearFilter')}
          </Button>
        )}
      </div>
    )
  }

  /** The no-results line names the search and the tag filter alike. */
  function noResultsQuery(active: string[]) {
    return [search.trim(), ...active.map((tag) => '#' + tag)].filter(Boolean).join(' ')
  }

  /** A ready row's ⋯ menu; `playlistItems` (a saved song's) follow Tags…. The
   * wrapper is the tag or playlist panel's anchor while this row's panel is open. */
  function rowMenu(
    id: number,
    name: string,
    onDelete: () => void,
    onReuse?: () => void,
    playlistItems: ActionMenuItem[] = [],
  ) {
    const items: ActionMenuItem[] = [
      { label: t('media.menu.rename'), onSelect: () => setEditingId(id) },
      { label: t('media.menu.tags'), onSelect: () => setTagging({ id, focusAdd: false }) },
      { label: t('media.menu.crop'), onSelect: () => setCroppingId(id) },
      ...playlistItems,
    ]
    if (onReuse) items.push({ label: t('media.generate.reuseSettings'), onSelect: onReuse })
    items.push({ label: t('media.menu.delete'), onSelect: onDelete, danger: true })
    return (
      <span
        className="media__menu"
        ref={
          tagging?.id === id || playlistPanelId === id || croppingId === id
            ? panelAnchorRef
            : null
        }
      >
        <ActionMenu label={t('media.menu.label', { name })} items={items} />
      </span>
    )
  }

  // The open tag panel's row — only while it renders in the visible tab — and the
  // vocabulary it offers: every tag across both libraries, alphabetical.
  const tabRows: Array<GeneratedTrack | GeneratedSample> =
    tab === 'generate' ? tracks : tab === 'samples' ? samples : []
  const taggingRow = tabRows.find((row) => row.id === tagging?.id)
  const croppingRow = tabRows.find((row) => row.id === croppingId)
  // The open "Add to playlist…" panel's song (a saved one, on the Songs tab).
  const playlistPanelRow = tracks.find((row) => row.id === playlistPanelId)
  const playlistPanelFile =
    tab === 'generate' && playlistPanelRow?.state === 'ready' ? playlistPanelRow.file : null

  /** "Use as source": the next Track take grows from this saved song. */
  function sourceMenuItems(track: GeneratedTrack): ActionMenuItem[] {
    if (track.state !== 'ready' || track.file == null) return []
    const { file, title: songTitle } = track
    return [
      {
        label: t('media.menu.useAsSource'),
        onSelect: () => {
          setSource({
            file,
            title: songTitle,
            mode: 'continue',
            start: '',
            end: '',
            noiseLevel: SOURCE_NOISE_DEFAULT,
            replaceStart: '',
            replaceEnd: '',
          })
          setEngine('track')
          if (!ENGINE_LENGTHS.track.includes(seconds)) setSeconds(ENGINE_LENGTHS.track[1])
          setGenerateError(null)
        },
      },
    ]
  }

  /** A saved song's playlist menu items: "Add to playlist…" everywhere, and in a
   * playlist view Move up / Move down (in the stored order, so only while the view
   * shows it) and Remove from playlist. */
  function songPlaylistItems(id: number, file: string | null): ActionMenuItem[] {
    if (file == null) return []
    const items: ActionMenuItem[] = [
      {
        label: t('media.menu.addToPlaylist'),
        onSelect: () => {
          setTagging(null)
          setPlaylistPanelId(id)
        },
      },
    ]
    if (!playlistView) return items
    const index = playlist.files.indexOf(file)
    const inOrder = playlistSort === 'playlist'
    items.push(
      {
        label: t('media.menu.moveUp'),
        disabled: !inOrder || index <= 0,
        onSelect: () => void savePlaylist(playlist.name, moveFile(playlist.files, index, index - 1)),
      },
      {
        label: t('media.menu.moveDown'),
        disabled: !inOrder || index >= playlist.files.length - 1,
        onSelect: () => void savePlaylist(playlist.name, moveFile(playlist.files, index, index + 1)),
      },
      {
        label: t('media.menu.removeFromPlaylist'),
        onSelect: () => void savePlaylist(playlist.name, removeFile(playlist.files, file)),
      },
    )
    return items
  }
  // The open playlist's total length, once every song in it carries one.
  const playlistSeconds = songRows.map((row) =>
    row.state === 'ready' ? recipeSeconds(row.recipe) : null,
  )
  const playlistLength =
    playlistSeconds.length > 0 && playlistSeconds.every((value) => value != null)
      ? playlistSeconds.reduce<number>((sum, value) => sum + (value ?? 0), 0)
      : null
  const tagVocabulary =
    taggingRow?.state === 'ready'
      ? [
          ...new Set(
            [...tracks, ...samples].flatMap((row) => (row.state === 'ready' ? row.tags : [])),
          ),
        ].sort((a, b) => a.localeCompare(b))
      : []

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
  const searchScope = t(`media.search.scopes.${tab}`)
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
        {/* The right cluster: search and per-tab utility actions sit next to the
            collapse toggle, which is always present. */}
        <div className="media__header-end">
          {open && (
            <div className="media__header-actions">
              <div className="media__search" role="search">
                <TextField
                  type="search"
                  labelHidden
                  label={t('media.search.label', { scope: searchScope })}
                  placeholder={t('media.search.placeholder', { scope: searchScope })}
                  value={search}
                  onChange={(event) => {
                    setSearch(event.target.value)
                    setHighlight(0)
                  }}
                />
                {search && (
                  <Button
                    aria-label={t('media.search.clear')}
                    title={t('media.search.clear')}
                    onClick={() => {
                      setSearch('')
                      setHighlight(0)
                    }}
                  >
                    ✕
                  </Button>
                )}
              </div>
              {(tab === 'generate' || tab === 'samples') && (
                <div className="media__sort">
                  <Select
                    label={t('media.sort.label')}
                    value={playlistView ? playlistSort : mediaSort}
                    options={playlistView ? playlistSortOptions : sortOptions}
                    onChange={playlistView ? changePlaylistSort : changeSort}
                  />
                </div>
              )}
              {(tab === 'generate' || tab === 'samples') && (
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
              )}
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
          filter={search}
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
          {engine === 'track' && source ? (
            <div
              className="media__generation-options"
              role="group"
              aria-label={t('media.generate.source.label', { title: source.title })}
            >
              <SegmentedControl
                label={t('media.generate.source.label', { title: source.title })}
                value={source.mode}
                options={SOURCE_MODES.map((mode) => ({
                  value: mode,
                  label: t(`media.generate.source.modes.${mode}`),
                }))}
                onChange={(mode) => setSource({ ...source, mode })}
              />
              <TextField
                type="number"
                min={0}
                step="any"
                label={t('media.generate.source.from')}
                value={source.start}
                onChange={(event) => setSource({ ...source, start: event.target.value })}
              />
              <TextField
                type="number"
                min={0}
                step="any"
                label={t('media.generate.source.to')}
                placeholder={t('media.generate.source.toEnd')}
                value={source.end}
                onChange={(event) => setSource({ ...source, end: event.target.value })}
              />
              {source.mode === 'remix' ? (
                <Slider
                  label={t('media.generate.source.noise')}
                  min={0.1}
                  max={1}
                  step={0.05}
                  value={source.noiseLevel}
                  onChange={(noiseLevel) => setSource({ ...source, noiseLevel })}
                />
              ) : null}
              {source.mode === 'replace' ? (
                <>
                  <TextField
                    type="number"
                    min={0}
                    step="any"
                    label={t('media.generate.source.replaceFrom')}
                    value={source.replaceStart}
                    onChange={(event) =>
                      setSource({ ...source, replaceStart: event.target.value })
                    }
                  />
                  <TextField
                    type="number"
                    min={0}
                    step="any"
                    label={t('media.generate.source.replaceTo')}
                    value={source.replaceEnd}
                    onChange={(event) =>
                      setSource({ ...source, replaceEnd: event.target.value })
                    }
                  />
                </>
              ) : null}
              <Button onClick={() => setSource(null)}>{t('media.generate.source.clear')}</Button>
              <p className="media__generation-hint">
                {t(`media.generate.source.hints.${source.mode}`)}
              </p>
            </div>
          ) : null}
          <div className="media__songs">
            <nav className="media__sources" aria-label={t('media.playlists.sources')}>
              <Button
                lit={playlist == null}
                aria-pressed={playlist == null}
                onClick={() => selectSource('all')}
              >
                {t('media.playlists.all')}
              </Button>
              {playlists.map((entry) => (
                // The wrapper is the drop target (Button takes no class); it lights
                // while a dragged song is over it.
                <div
                  key={entry.name}
                  className={`media__source${
                    dropSource === entry.name ? ' media__source--drop' : ''
                  }`}
                  onDragOver={(event) => {
                    event.preventDefault()
                    setDropSource(entry.name)
                  }}
                  onDragLeave={() => setDropSource(null)}
                  onDrop={(event) => {
                    event.preventDefault()
                    setDropSource(null)
                    dropOnPlaylist(entry, event.dataTransfer.getData('text/plain'))
                  }}
                >
                  <Button
                    lit={playlist === entry}
                    aria-pressed={playlist === entry}
                    onClick={() => selectSource(entry.name)}
                  >
                    {entry.name}{' '}
                    <span className="media__source-count">
                      {t('media.playlists.count', { count: entry.files.length })}
                    </span>
                  </Button>
                </div>
              ))}
              {creatingPlaylist ? (
                playlistNameEditor('', (name) => {
                  setCreatingPlaylist(false)
                  if (name) createPlaylist(name)
                })
              ) : (
                <Button onClick={() => setCreatingPlaylist(true)}>
                  {t('media.playlists.new')}
                </Button>
              )}
            </nav>
            <div className="media__songs-list">
              {/* The column's narrow-width stand-in (CSS shows one or the other). */}
              <div className="media__source-select">
                <Select
                  label={t('media.playlists.source')}
                  value={playlist?.name ?? 'all'}
                  options={sourceOptions}
                  onChange={selectSource}
                />
              </div>
              {playlist != null && (
                <div className="media__playlist-header">
                  {renamingPlaylist ? (
                    playlistNameEditor(playlist.name, (name) => {
                      setRenamingPlaylist(false)
                      if (name && name !== playlist.name) renamePlaylist(playlist.name, name)
                    })
                  ) : (
                    <span
                      className="media__playlist-name"
                      onDoubleClick={() => setRenamingPlaylist(true)}
                    >
                      {playlist.name}
                    </span>
                  )}
                  <span className="media__meta">
                    {`${t('media.playlists.count', { count: songRows.length })}${
                      playlistLength == null ? '' : ` · ${formatLength(playlistLength)}`
                    }`}
                  </span>
                  <ActionMenu
                    label={t('media.playlists.menuLabel')}
                    items={[
                      {
                        label: t('media.menu.renamePlaylist'),
                        onSelect: () => setRenamingPlaylist(true),
                      },
                      {
                        label: t('media.menu.deletePlaylist'),
                        onSelect: () => deletePlaylist(playlist.name),
                        danger: true,
                      },
                    ]}
                  />
                </div>
              )}
          {tagFilterBar(songTags, songTagFilter)}
          {playlist != null && songRows.length === 0 ? (
            <p className="media__empty">{t('media.playlists.empty')}</p>
          ) : tracks.length === 0 ? (
            <p className="media__empty">{t('media.generate.empty')}</p>
          ) : filteredTracks.length === 0 ? (
            <p className="media__empty" role="status">
              {t('media.search.noResults', { query: noResultsQuery(songTagFilter) })}
            </p>
          ) : (
            <ul className="media__list">
              {sortedTracks.map((track) => {
                // Composed takes (those carrying a prompt) get a #id to tell same-title
                // siblings apart; an imported file shows just its name. The title
                // ellipsises in the row; the tag never shrinks.
                const composed = track.prompt != null
                const rowLabel = trackLabel(track)
                const onDeck =
                  track.state === 'ready'
                    ? deckHolding(trackOnDeck, track.id, (deck) => deck.track?.title === rowLabel)
                    : null
                const played =
                  onDeck == null && track.state === 'ready' && track.file != null
                    ? playedFiles.has(track.file)
                    : false
                const length = track.state === 'ready' ? recipeSeconds(track.recipe) : null
                // In a playlist view every row is saved and a drop target for reordering.
                const playlistFile = playlist != null ? fileOf(track) : null
                const dropSide =
                  playlistFile != null && dropRow?.file === playlistFile
                    ? dropRow.after
                      ? ' media__item--drop-after'
                      : ' media__item--drop-before'
                    : ''
                return (
                  <li
                    key={track.id}
                    ref={track.id === highlightedReadyId ? highlightedRow : null}
                    className={`media__item${
                      track.id === highlightedReadyId
                        ? ' media__item--highlighted'
                        : ''
                    }${played ? ' media__item--played' : ''}${dropSide}`}
                    // A saved song drags as a whole row — onto a playlist, or within one.
                    // The row, not the name button: WebKit ignores `draggable` on form
                    // controls, so a button-sourced drag never starts in the WKWebView.
                    draggable={fileOf(track) != null}
                    onDragStart={(event) => {
                      const file = fileOf(track)
                      if (file == null) return
                      event.dataTransfer.setData('text/plain', file)
                      event.dataTransfer.effectAllowed = 'copyMove'
                    }}
                    onDragEnd={() => {
                      setDropSource(null)
                      setDropRow(null)
                    }}
                    // Outside a playlist view nothing is accepted (no preventDefault).
                    onDragOver={(event) => {
                      if (playlistFile == null) return
                      event.preventDefault()
                      const after = dropsAfter(event)
                      if (dropRow?.file !== playlistFile || dropRow.after !== after) {
                        setDropRow({ file: playlistFile, after })
                      }
                    }}
                    onDragLeave={(event) => {
                      if (!event.currentTarget.contains(event.relatedTarget as Node)) {
                        setDropRow(null)
                      }
                    }}
                    onDrop={(event) => {
                      if (playlist == null || playlistFile == null) return
                      event.preventDefault()
                      setDropRow(null)
                      dropOnRow(
                        playlist,
                        playlistFile,
                        dropsAfter(event),
                        event.dataTransfer.getData('text/plain'),
                      )
                    }}
                  >
                    {/* The reorder affordance: the whole row drags, the grip says so.
                        Only while the stored order is showing — a rating/title sort is a
                        view, and Move up/down are disabled there too. */}
                    {playlistFile != null && playlistSort === 'playlist' && (
                      <span
                        className="media__grip"
                        aria-hidden="true"
                        title={t('media.playlists.reorderHint')}
                      >
                        ⋮⋮
                      </span>
                    )}
                    <span className="media__name">
                      {track.state === 'pending' && (
                        <span className="media__spinner" aria-hidden="true" />
                      )}
                      {track.state === 'pending' ? (
                        <span className="media__name-text">
                          {t('media.generate.pending', { title: track.title })}
                        </span>
                      ) : editingId === track.id ? (
                        renameEditor(rowLabel, track.title)
                      ) : (
                        // A click highlights (the rotary/LOAD/number-key target, as in
                        // the Folder tab); a double-click opens the rename editor.
                        <button
                          type="button"
                          className="media__name--button"
                          aria-label={t('media.highlight', { name: rowLabel })}
                          aria-current={track.id === highlightedReadyId}
                          onClick={() => setHighlight(ready.indexOf(track))}
                          onDoubleClick={() => setEditingId(track.id)}
                        >
                          <span className="media__name-text">{track.title}</span>
                        </button>
                      )}
                      {track.state === 'ready' && composed && (
                        <span className="media__name-tag">{`#${track.id}`}</span>
                      )}
                      {onDeckBadge(onDeck)}
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
                      {track.state === 'ready' && tagChips(track.tags)}
                    </span>
                    {track.state === 'ready' && (
                      <Rating
                        value={track.rating}
                        onChange={(rating) => rateTrack(track, rating)}
                        label={t('media.rating.label', { name: rowLabel })}
                        starLabel={starLabel}
                        clearHint={t('media.rating.clearHint')}
                      />
                    )}
                    <span className="media__meta">
                      {`${
                        track.model == null
                          ? t('media.generate.imported')
                          : t(`media.generate.engines.${track.model}`)
                      }${length == null ? '' : ` · ${formatLength(length)}`}${analysisMeta(
                        track,
                        (bpm) => t('media.analysis.bpm', { bpm }),
                      )}`}
                    </span>
                    {track.state === 'ready' &&
                      rowMenu(
                        track.id,
                        rowLabel,
                        () => void removeTrack(track),
                        hasVersionedRecipe(track.recipe)
                          ? () => recallSongRecipe(track)
                          : undefined,
                        [
                          ...songPlaylistItems(track.id, track.file),
                          ...sourceMenuItems(track),
                        ],
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
                    {/* The only bare ✕ in the explorer: out of this playlist, never a
                        delete (that lives in the ⋯ menu). */}
                    {playlist != null && playlistFile != null && (
                      <Button
                        aria-label={t('media.playlists.removeFrom', {
                          name: rowLabel,
                          playlist: playlist.name,
                        })}
                        onClick={() =>
                          void savePlaylist(playlist.name, removeFile(playlist.files, playlistFile))
                        }
                      >
                        ✕
                      </Button>
                    )}
                  </li>
                )
              })}
            </ul>
          )}
            </div>
          </div>
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
          {tagFilterBar(sampleTags, sampleTagFilter)}
          {samples.length === 0 ? (
            <p className="media__empty">{t('media.samples.empty')}</p>
          ) : filteredSamples.length === 0 ? (
            <p className="media__empty" role="status">
              {t('media.search.noResults', { query: noResultsQuery(sampleTagFilter) })}
            </p>
          ) : (
            <ul className="media__list">
              {sortedSamples.map((sample) => {
                const composed = sample.prompt != null
                const rowLabel = sampleLabel(sample)
                const onDeck =
                  sample.state === 'ready'
                    ? deckHolding(sampleOnDeck, sample.id, (deck) =>
                        deck.loopLabels.includes(rowLabel),
                      )
                    : null
                const played =
                  onDeck == null && sample.state === 'ready' && sample.file != null
                    ? playedFiles.has(sample.file)
                    : false
                return (
                  <li
                    key={sample.id}
                    ref={sample.id === highlightedSampleId ? highlightedRow : null}
                    className={`media__item${
                      sample.id === highlightedSampleId
                        ? ' media__item--highlighted'
                        : ''
                    }${played ? ' media__item--played' : ''}`}
                  >
                    <span className="media__name">
                      {sample.state === 'pending' && (
                        <span className="media__spinner" aria-hidden="true" />
                      )}
                      {sample.state === 'pending' ? (
                        <span className="media__name-text">
                          {t('media.generate.pending', { title: sample.title })}
                        </span>
                      ) : editingId === sample.id ? (
                        renameEditor(rowLabel, sample.title)
                      ) : (
                        <button
                          type="button"
                          className="media__name--button"
                          aria-label={t('media.highlight', { name: rowLabel })}
                          aria-current={sample.id === highlightedSampleId}
                          onClick={() => setHighlight(readySamples.indexOf(sample))}
                          onDoubleClick={() => setEditingId(sample.id)}
                        >
                          <span className="media__name-text">{sample.title}</span>
                        </button>
                      )}
                      {sample.state === 'ready' && composed && (
                        <span className="media__name-tag">{`#${sample.id}`}</span>
                      )}
                      {onDeckBadge(onDeck)}
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
                      {sample.state === 'ready' && tagChips(sample.tags)}
                    </span>
                    {sample.state === 'ready' && (
                      <Rating
                        value={sample.rating}
                        onChange={(rating) => rateSample(sample, rating)}
                        label={t('media.rating.label', { name: rowLabel })}
                        starLabel={starLabel}
                        clearHint={t('media.rating.clearHint')}
                      />
                    )}
                    <span className="media__meta">
                      {`${sampleModelLabel(sample.model)} · ${t(
                        sample.oneShot ? 'media.samples.oneShot' : 'media.samples.loop',
                      )}${analysisMeta(sample, (bpm) => t('media.analysis.bpm', { bpm }))}`}
                    </span>
                    {sample.state === 'ready' &&
                      rowMenu(sample.id, rowLabel, () => void removeSample(sample))}
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
          {files.length > 0 && filteredFiles.length === 0 && (
            <p className="media__empty" role="status">
              {t('media.search.noResults', { query: search.trim() })}
            </p>
          )}
          {filteredFiles.length > 0 && (
            <ul className="media__list">
              {filteredFiles.map((file, index) => (
                <li
                  key={file.name}
                  ref={index === highlightedFileIndex ? highlightedRow : null}
                  className={`media__item${
                    index === highlightedFileIndex
                      ? ' media__item--highlighted'
                      : ''
                  }`}
                >
                  <button
                    className="media__name media__name--button"
                    aria-label={t('media.highlight', { name: file.name })}
                    aria-current={index === highlightedFileIndex}
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
      {playlistPanelFile != null && (
        // Keyed by row, like the tag panel.
        <ChecklistPanel
          key={playlistPanelId}
          open
          anchorRef={panelAnchorRef}
          onClose={() => {
            setPlaylistPanelId(null)
            panelAnchorRef.current?.querySelector('button')?.focus()
          }}
          label={t('media.playlists.addLabel', {
            name: playlistPanelRow ? trackLabel(playlistPanelRow) : '',
          })}
          options={playlists.map((entry) => ({
            value: entry.name,
            label: entry.name,
            checked: entry.files.includes(playlistPanelFile),
          }))}
          onToggle={(name) => {
            const entry = playlists.find((candidate) => candidate.name === name)
            if (!entry) return
            void savePlaylist(
              name,
              entry.files.includes(playlistPanelFile)
                ? removeFile(entry.files, playlistPanelFile)
                : insertFile(entry.files, playlistPanelFile),
            )
          }}
          add={{
            label: t('media.playlists.addNew'),
            placeholder: t('media.playlists.newPlaceholder'),
            // An existing name gains the song rather than being replaced by it.
            onAdd: (text) => {
              const existing = playlistNamed(text)
              void savePlaylist(
                existing?.name ?? text,
                existing ? insertFile(existing.files, playlistPanelFile) : [playlistPanelFile],
              )
            },
          }}
          emptyText={t('media.playlists.none')}
        />
      )}
      {croppingRow?.state === 'ready' && croppingRow.file != null && (
        <CropPanel
          key={croppingRow.id}
          library={'oneShot' in croppingRow ? 'samples' : 'songs'}
          file={croppingRow.file}
          name={'oneShot' in croppingRow ? sampleLabel(croppingRow) : trackLabel(croppingRow)}
          anchorRef={panelAnchorRef}
          onClose={() => {
            setCroppingId(null)
            panelAnchorRef.current?.querySelector('button')?.focus()
          }}
          onPreview={async (wav) => {
            await onPreview(wav)
            // The phones now carry the crop, not a row's preview.
            setPreviewingId(null)
          }}
          onStopPreview={onStopPreview}
          onSaved={() => void ('oneShot' in croppingRow ? refreshSamples() : refreshSongs())}
        />
      )}
      {taggingRow?.state === 'ready' && (
        // Keyed by row so moving to another row re-anchors and refocuses.
        <ChecklistPanel
          key={taggingRow.id}
          open
          anchorRef={panelAnchorRef}
          onClose={() => {
            setTagging(null)
            // The anchor is a wrapper, so hand focus back to its ⋯ trigger.
            panelAnchorRef.current?.querySelector('button')?.focus()
          }}
          label={t('media.tags.label', {
            name: 'oneShot' in taggingRow ? sampleLabel(taggingRow) : trackLabel(taggingRow),
          })}
          options={tagVocabulary.map((tag) => ({
            value: tag,
            label: `#${tag}`,
            checked: taggingRow.tags.includes(tag),
          }))}
          onToggle={(tag) =>
            commitTags(
              taggingRow.id,
              taggingRow.tags.includes(tag)
                ? taggingRow.tags.filter((current) => current !== tag)
                : [...taggingRow.tags, tag],
            )
          }
          add={{
            label: t('media.tags.add'),
            placeholder: t('media.tags.placeholder'),
            onAdd: (text) => {
              if (!taggingRow.tags.includes(text)) {
                commitTags(taggingRow.id, [...taggingRow.tags, text])
              }
            },
          }}
          emptyText={t('media.tags.empty')}
          initialFocus={tagging?.focusAdd ? 'add' : undefined}
        />
      )}
      </div>
    </Panel>
  )
}
