import { useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'

import type { DeckId } from '../audio/types'
import { FX_KINDS, fxRestPosition, type FxKind } from '../audio/fx'
import { LOOP_LENGTH_OPTIONS, LOOP_SLOT_COUNT } from '../audio/loops'
import {
  styleAddSampleTarget,
  styleAddTarget,
  styleApplyPreset,
  styleFanOut,
  styleMoveTarget,
  styleRemoveTarget,
  styleRenameTarget,
  styleSetCursor,
  styleToggleSelection,
} from '../audio/nativeEngine'
import { useInterfaceStore } from '../audio/interfaceStore'
import { useControlBus } from '../control/busContext'
import { PerformanceDrawer } from './PerformanceDrawer'
import { Button } from '../ui/Button'
import { Knob } from '../ui/Knob'
import { Meter } from '../ui/Meter'
import { Panel } from '../ui/Panel'
import { Select } from '../ui/Select'
import { Stat } from '../ui/Stat'
import { TextField } from '../ui/TextField'
import { TrackOverview, TRACK_OVERVIEW_BUCKETS } from '../ui/TrackOverview'
import { TransportButton } from '../ui/TransportButton'
import { XYPad } from '../ui/XYPad'
import { moveRadial } from '../ui/netGeometry'
import { isDeckOperable, type DeckState } from './deckState'
import { TRACK_RATE_RANGE } from '../audio/track'
import { sweepPosition } from './padWeights'
import {
  MAX_PRESET_NAME_LENGTH,
  MAX_PRESET_TARGETS,
  type StylePreset,
} from '../presets'
import {
  GENERATE_PROMPT_MAX_LENGTH,
  type DeckMode,
  type GenerateEngine,
  type LoopState,
  type SyncResult,
  type TrackState,
} from './useDeck'
import {
  stackForKind,
  useLoras,
  useLoraStack,
  type LoraChoice,
} from '../models/useLoras'
import { LoraControl } from '../ui/LoraControl'
import './deck.css'

// The worker holds ~3s of lead (see backend worker pacing); the meter shows
// health relative to that target.
const BUFFER_TARGET_SECONDS = 3
// One source for the pad cap (mirrors the backend's MAX_STYLE_PROMPTS).
const MAX_TARGETS = MAX_PRESET_TARGETS
// Pad units a single jog tick steers the cursor under SHIFT (tunable on the
// device — see the net hardware checklist).
const CURSOR_JOG_STEP = 0.01
// The delta-gesture overlay (jog reel / cursor steer): how long a pending
// emitted position outlives a disagreeing snapshot before the store's word
// wins, and how close an echo must land to count as caught-up (the f32
// round-trip quantises; same epsilon as the mixer adoption).
const PENDING_SETTLE_MS = 120
function nearPending(a: number, b: number) {
  return Math.abs(a - b) < 1e-3
}

function clamp01(value: number) {
  return Math.min(1, Math.max(0, value))
}

function formatTrackTime(seconds: number): string {
  const whole = Math.floor(seconds)
  return `${Math.floor(whole / 60)}:${String(whole % 60).padStart(2, '0')}`
}

/** The loop's length in beats — only when the region truly is a whole
 * number of them (a tail-clamped loop is not "0 beats"; claiming a
 * count it doesn't have breaks the honesty rule). */
/** Clean beat fractions a halve can produce (M23), longest glyph first. */
const LOOP_BEAT_FRACTIONS: ReadonlyArray<[number, string]> = [
  [0.5, '½'],
  [0.25, '¼'],
  [0.125, '⅛'],
]

/** A loop's length for the readout: a whole beat count, or a clean
 * fraction (½, ¼, ⅛) once halved (M23). Null without a confident grid
 * or for a region that isn't a clean beat length — the honesty rule
 * keeps a count off a free or tail-clamped loop. */
function loopBeatLabel(
  loop: { start: number; end: number },
  grid: { bpm: number } | null,
): string | null {
  if (!grid) return null
  const beats = (loop.end - loop.start) / (60 / grid.bpm)
  const whole = Math.round(beats)
  if (whole >= 1 && Math.abs(beats - whole) < 0.01) return String(whole)
  for (const [value, glyph] of LOOP_BEAT_FRACTIONS) {
    if (Math.abs(beats - value) < 0.01) return glyph
  }
  return null
}

type DeckColumnProps = {
  deckId: DeckId
  state: DeckState
  onPlay: () => void
  onStop: () => void
  onSetModel: (model: string) => void
  onRestart: () => void
  /** Which deck's SHIFT is held (App-tracked). When it equals this deck's id,
   * the jogs steer this deck's cursor — jog A the x axis, jog B the y. */
  shiftedDeck?: DeckId | null
  /** Generating off air (M10 deck prep) — surfaced in the status line. */
  primed?: boolean
  /** Color FX insert state and controls (M12). */
  fx: { kind: FxKind | null; amount: number }
  onSetFx: (kind: FxKind | null) => void
  onSetFxAmount: (amount: number) => void
  /** Freeze pads (M13): slot state and the pad/clear/length actions. */
  loop: LoopState
  onLoopPad: (slot: number) => void
  onClearLoopPad: (slot: number) => void
  onSetLoopSeconds: (seconds: number) => void
  /** Generated pads (M18): fill the first empty slot from a prompt,
   * with the chosen engine, one-shot/loop behaviour, and LoRA stack. */
  onGenerateToPad: (
    prompt: string,
    engine: GenerateEngine,
    oneShot: boolean,
    loras?: LoraChoice[] | null,
  ) => void
  generateError: string | null
  /** Gated tempo readout (M14): null shows an honest dash. */
  bpm: number | null
  /** Style sampling (M15): capture the OTHER deck and register the
   * embedding on this one; resolves to the new target, or null when
   * the other deck has not played enough. */
  onSampleOtherDeck: () => Promise<{ label: string; sample: string } | null>
  /** Whether the other deck is currently producing something to sample. */
  canSample: boolean
  /** Crates (M16): save this deck's pad + FX as a named preset. */
  onSavePreset: (preset: StylePreset) => void
  /** Playback mode (M19, ADR-0013): in 'playback' the style pane swaps
   * for the loaded track's overview and the transport drives it. */
  mode: DeckMode
  track: TrackState | null
  /** The deck-local exit from playback — back to the live stream
   * without a trip to the Media Explorer. */
  onLeavePlayback: () => void
  onSeekTrack: (seconds: number) => void
  /** Varispeed (M20): the tempo knob's rate, clamped upstream. */
  onSetTrackRate: (rate: number) => void
  /** SYNC: match the other deck's tempo; refusals name their reason. */
  onSyncTrack: () => SyncResult
  /** Hot cues and the track loop (M21, ADR-0015): pads mean position
   * on a playback deck. */
  onHotCuePad: (index: number) => void
  onClearHotCue: (index: number) => void
  onLoopIn: () => void
  onLoopOut: () => void
  onLoopExit: () => void
  /** Beat loops (M23): a one-press N-beat loop, and halve/double of the
   * active loop. */
  onBeatLoop: (beats: number) => void
  onHalveLoop: () => void
  onDoubleLoop: () => void
  getTrackPeaks: (
    buckets: number,
  ) => { min: Float32Array; max: Float32Array } | null
}

export function DeckColumn({
  deckId,
  state,
  onPlay,
  onStop,
  onSetModel,
  onRestart,
  shiftedDeck,
  primed = false,
  fx,
  onSetFx,
  onSetFxAmount,
  loop,
  onLoopPad,
  onClearLoopPad,
  onSetLoopSeconds,
  onGenerateToPad,
  generateError,
  bpm,
  onSampleOtherDeck,
  canSample,
  onSavePreset,
  mode,
  track,
  onLeavePlayback,
  onSeekTrack,
  onSetTrackRate,
  onSyncTrack,
  onHotCuePad,
  onClearHotCue,
  onLoopIn,
  onLoopOut,
  onLoopExit,
  onBeatLoop,
  onHalveLoop,
  onDoubleLoop,
  getTrackPeaks,
}: DeckColumnProps) {
  const { t } = useTranslation()
  const deckIndex = deckId === 'a' ? 0 : 1
  // The style pad is a PROJECTION (ADR-0020 phase B): targets, cursor, and
  // the net selection live in the Rust store — hydrated from the shell
  // settings, mutated only through the style_* intents below. The webview
  // renders whatever the store accepted; there is no local copy to revert.
  const storeState = useInterfaceStore()
  const deckSnap = storeState?.decks[deckIndex]
  const targets = useMemo(() => deckSnap?.styleTargets ?? [], [deckSnap])
  const cursor = deckSnap?.cursor ?? { x: 0.5, y: 0.5 }
  // The net: which targets are in the blend mask, projected as a text-keyed
  // set (the shape the pad and the jog handlers consume).
  const selected = useMemo(
    () =>
      new Set(
        targets
          .filter((_, index) => deckSnap?.styleSelected[index] ?? false)
          .map((target) => target.text),
      ),
    [deckSnap, targets],
  )
  // Delta gestures (jog reel, SHIFT-jog cursor steer) accumulate on the last
  // EMITTED position, not the projection: the intent coalescer keeps only
  // the last write per frame and the store echo lags a frame or two, so two
  // jog ticks computed from the same snapshot would swallow each other
  // (lost steps at fast rates). Every cursor/target emit records its value
  // here; an entry drains when the snapshot catches up to it, or after the
  // settle window when it never will (an external writer — MCP, a fan-out —
  // moved the thing mid-gesture and the store's word wins).
  const pendingCursorRef = useRef<{ x: number; y: number; at: number } | null>(null)
  const pendingMovesRef = useRef(
    new Map<string, { x: number; y: number; at: number }>(),
  )
  useEffect(() => {
    const caughtUp = (
      pending: { x: number; y: number; at: number },
      x: number,
      y: number,
    ) =>
      (nearPending(pending.x, x) && nearPending(pending.y, y)) ||
      Date.now() - pending.at > PENDING_SETTLE_MS
    const cursorPending = pendingCursorRef.current
    if (cursorPending && caughtUp(cursorPending, cursor.x, cursor.y)) {
      pendingCursorRef.current = null
    }
    for (const [text, pending] of pendingMovesRef.current) {
      const target = targets.find((candidate) => candidate.text === text)
      if (!target || caughtUp(pending, target.x, target.y)) {
        pendingMovesRef.current.delete(text)
      }
    }
  }, [cursor.x, cursor.y, targets])
  const [sampling, setSampling] = useState(false)
  const [sampleError, setSampleError] = useState<string | null>(null)
  // Generated pads (M18): the prompt, engine, and behaviour for the
  // next generation.
  const [generateDraft, setGenerateDraft] = useState('')
  // SYNC's honest refusal (M20), keyed to the load so a stale verdict
  // never haunts the next track's panel.
  const [syncRefusal, setSyncRefusal] = useState<{
    loadId: number
    reason: Exclude<SyncResult, 'synced'>
  } | null>(null)
  const [generateEngine, setGenerateEngine] = useState<GenerateEngine>('sfx')
  const [generateOneShot, setGenerateOneShot] = useState(true)
  // Per-deck LoRA stack (issue #66): both pad kinds ride the small DiTs, so
  // one rack covers them; Magenta has no adapter path and hides it.
  const loras = useLoras()
  const padStack = useLoraStack()
  const [targetDraft, setTargetDraft] = useState('')
  // In-place prompt editing: which row is open and its draft text.
  const [editing, setEditing] = useState<{ text: string; draft: string } | null>(
    null,
  )
  // After a keyboard-driven commit/cancel, focus returns to this
  // row's ✎ (the input unmounts, which would otherwise drop focus to
  // the body). A ref, not state: the commit/cancel itself re-renders
  // via setEditing, and focusing is imperative — no render to drive.
  const focusAfterEditRef = useRef<string | null>(null)
  const editButtons = useRef(new Map<string, HTMLButtonElement>())
  useEffect(() => {
    if (focusAfterEditRef.current === null) return
    const button = editButtons.current.get(focusAfterEditRef.current)
    // A renamed row exists only once the store echoes the rename — keep the
    // pending focus until the projection renders it.
    if (!button) return
    button.focus()
    focusAfterEditRef.current = null
  })
  const [presetDraft, setPresetDraft] = useState('')

  const connected = state.connection === 'open'
  const operable = isDeckOperable(state)
  const canGenerate =
    connected &&
    Boolean(generateDraft.trim()) &&
    loop.slots.some((slot) => slot.state === 'empty')
  // Pads only ever ride the small DiTs; a stale slot (deleted adapter)
  // drops from the request, never blocks it.
  const fireGenerate = () => {
    if (!canGenerate) return
    const stacked =
      generateEngine === 'magenta' ? [] : stackForKind(padStack.stack, loras, 'sfx')
    onGenerateToPad(generateDraft, generateEngine, generateOneShot, stacked)
  }
  const statusKey =
    mode === 'playback'
      ? track?.ended
        ? 'deck.status.trackEnded'
        : track?.playing
          ? 'deck.status.trackPlaying'
          : 'deck.status.trackPaused'
      : state.switchingModel
    ? 'deck.status.loadingModel'
    : loop.active !== null && connected
      ? 'deck.status.frozen'
      : primed && connected
        ? 'deck.status.primed'
        : state.connection === 'open'
          ? 'deck.status.connected'
          : 'deck.status.connecting'
  const bufferFraction = state.bufferedSeconds / BUFFER_TARGET_SECONDS
  const bufferTone =
    !state.playing || bufferFraction >= 0.5 ? 'ok' : bufferFraction >= 0.25 ? 'warn' : 'danger'

  // The overview envelope is static per track — recompute only when a
  // different load lands (the monotonic id; titles can repeat), not on
  // every playhead tick.
  const trackKey = track ? track.loadId : null
  const trackPeaksData = useMemo(
    () => (trackKey === null ? null : getTrackPeaks(TRACK_OVERVIEW_BUCKETS)),
    [trackKey, getTrackPeaks],
  )

  // An open edit whose target vanished (preset load, removal, a worker
  // restart stripping its sampled chip store-side) must not linger — its
  // input unmounts without a blur, and a later same-named target would
  // render pre-opened with the stale draft.
  if (editing && !targets.some((target) => target.text === editing.text)) {
    setEditing(null)
  }

  function addTarget() {
    const text = targetDraft.trim()
    if (
      !text ||
      targets.some((target) => target.text === text) ||
      targets.length >= MAX_TARGETS
    ) {
      return
    }
    styleAddTarget(deckIndex, text)
    setTargetDraft('')
  }

  function savePreset() {
    const name = presetDraft.trim().slice(0, MAX_PRESET_NAME_LENGTH)
    const textTargets = targets
      .filter((target) => !target.sample)
      .map(({ text, x, y }) => ({ text, x, y }))
    if (!name || textTargets.length === 0) return
    onSavePreset({ name, targets: textTargets, cursor, fx })
    setPresetDraft('')
  }

  // One action (M15): capture the other deck, register the embedding,
  // land it on the pad as a blendable chip. The capture resolves once the
  // embed frame is queued on the deck's control socket, and the add-intent
  // fires after that — the socket's FIFO keeps any blend send behind the
  // embedding it references. The store enforces the cap and dup rules (the
  // pad may have filled during the await); a rejected add just never
  // appears in the projection.
  async function sampleOtherDeck() {
    if (sampling || targets.length >= MAX_TARGETS) return
    setSampling(true)
    setSampleError(null)
    try {
      const result = await onSampleOtherDeck()
      if (!result) {
        // The other deck is playing but hasn't produced the minimum
        // capture yet — say so instead of silently doing nothing.
        setSampleError(t('deck.style.sampleTooSoon'))
        return
      }
      styleAddSampleTarget(deckIndex, result.label, result.sample)
    } catch (error) {
      setSampleError(error instanceof Error ? error.message : String(error))
    } finally {
      setSampling(false)
    }
  }

  /** Commit an in-place prompt edit: the target keeps its position
   * and weight, only the prompt changes — re-embedded like typing it.
   * A rename that collides with another chip (or empties) cancels,
   * the same quiet rule the Add button applies to duplicates.
   * `restoreFocus` is set for keyboard outcomes (Enter/Escape) so
   * focus returns to the row's ✎ instead of falling to <body>; a
   * blur-commit means the user already clicked elsewhere, and yanking
   * focus back would fight them. */
  function commitEdit(restoreFocus = false) {
    if (!editing) return
    const text = editing.draft.trim()
    const original = editing.text
    setEditing(null)
    // The deck may have become untouchable mid-edit (disconnect, model
    // switch); every other mutation path is gated by a disabled
    // control, so the open input cancels rather than committing.
    if (!operable) return
    const renamed = text && text !== original && !targets.some((target) => target.text === text)
    const finalText = renamed ? text : original
    if (restoreFocus) focusAfterEditRef.current = finalText
    if (!renamed) return
    styleRenameTarget(deckIndex, original, text)
  }

  function cancelEdit() {
    if (!editing) return
    focusAfterEditRef.current = editing.text
    setEditing(null)
  }

  function removeTarget(text: string) {
    styleRemoveTarget(deckIndex, text)
  }

  function handleCursor(x: number, y: number) {
    pendingCursorRef.current = { x, y, at: Date.now() }
    styleSetCursor(deckIndex, x, y)
  }

  // Double-clicking the pad tidies the arrangement in one move: the store's
  // fan-out intent parks the blue dot at the canvas centre and fans the dots
  // evenly onto the spawn circle — every prompt equidistant from the centred
  // cursor, a neutral blend.
  function handleCursorActivate() {
    if (!operable || targets.length === 0) return
    styleFanOut(deckIndex)
  }

  // Loading a preset (M16) replaces the pad wholesale — targets, cursor, and
  // a cleared net selection (a preset is a fresh arrangement). The shell
  // sender pushes the new blend like typing the prompts (cached embeddings
  // make repeats cheap). Sampled chips are gone by construction: presets
  // never contain them.
  function applyPreset(preset: StylePreset) {
    styleApplyPreset(deckIndex, preset.targets, preset.cursor)
  }

  // Hardware style intents (ADR-0005) drive the net on a realtime deck: a
  // HOT CUE pad toggles its prompt's selection, the jog reels the selected
  // dots in/out about the cursor, and the CFX knob still sweeps the cursor
  // with the same throttle as a drag. Resubscribes per render to read fresh
  // state.
  const bus = useControlBus()
  useEffect(() =>
    bus.subscribe((intent) => {
      if (intent.kind === 'preset_load' && intent.deck === deckId) {
        applyPreset(intent.preset)
        return
      }
      // Pads mean position on a playback deck (M21, ADR-0015): the
      // hot-cue meaning lives in applyAppIntent; without this gate a
      // pad press would also drive the parked worker's style cursor.
      if (mode === 'playback') return
      if (!operable || targets.length === 0) return
      if (intent.kind === 'hot_cue_pad' && intent.deck === deckId) {
        // Toggle this prompt's net selection (replaces the old cursor-snap);
        // the jog then moves whatever is selected.
        const target = targets[intent.index]
        if (!target) return
        styleToggleSelection(deckIndex, target.text)
      } else if (intent.kind === 'track_seek') {
        if (shiftedDeck === deckId) {
          // SHIFT held on this deck: the two jogs steer this cursor in 2D —
          // jog A the x axis (CW → right), jog B the y (CW → down). The other
          // deck's jog reaches us because shiftedDeck routes by held SHIFT,
          // not by the jog's own deck.
          const delta = intent.steps * CURSOR_JOG_STEP
          const base = pendingCursorRef.current ?? cursor
          if (intent.deck === 'a') {
            handleCursor(clamp01(base.x + delta), base.y)
          } else {
            handleCursor(base.x, clamp01(base.y + delta))
          }
        } else if (
          shiftedDeck == null &&
          intent.deck === deckId &&
          selected.size > 0
        ) {
          // No SHIFT: this deck's own jog reels its selected dots radially
          // about the cursor — CW pulls inward (more weight), CCW pushes out.
          const centre = pendingCursorRef.current ?? cursor
          for (const target of targets) {
            if (!selected.has(target.text)) continue
            const base = pendingMovesRef.current.get(target.text) ?? target
            const moved = moveRadial({ ...target, x: base.x, y: base.y }, centre, intent.steps)
            handleTargetMove(target.text, moved.x, moved.y)
          }
        }
        // Otherwise another deck is being steered — leave this one alone.
      } else if (intent.kind === 'style_sweep' && intent.deck === deckId) {
        const next = sweepPosition(intent.value)
        handleCursor(next.x, next.y)
      }
    }),
  )

  function handleTargetMove(id: string, x: number, y: number) {
    // Record the emit so the next jog tick in the echo window builds on it
    // (mouse drags too — a drag mid-reel must not leave a stale base).
    pendingMovesRef.current.set(id, { x, y, at: Date.now() })
    styleMoveTarget(deckIndex, id, x, y)
  }

  const activeSummary = state.activeStyle
    ? t('deck.style.active', {
        summary: state.activeStyle.prompts
          .filter((prompt) => prompt.weight >= 0.005)
          .sort((a, b) => b.weight - a.weight)
          .map((prompt) =>
            t('deck.style.blendItem', {
              percent: Math.round(prompt.weight * 100),
              text: prompt.text,
            }),
          )
          .join(t('deck.style.blendSeparator')),
      })
    : ''

  const padTargets = targets.map((target) => ({
    id: target.text,
    label: target.text,
    x: target.x,
    y: target.y,
  }))

  return (
    <section
      className={`deck deck--${deckId}`}
      aria-label={t('deck.title', { id: deckId })}
    >
      <header className="deck__header">
        <h2 className="deck__title">{t('deck.title', { id: deckId })}</h2>
        <span
          className={`deck__status${connected ? '' : ' deck__status--disconnected'}`}
        >
          <span
            className={`deck__status-led${connected && !state.workerDied ? ' deck__status-led--on' : ''}`}
          />
          {t(statusKey)}
        </span>
      </header>

      {/* Playback mode (M19, ADR-0013): the style pane gives way to the
          loaded track — its envelope is the deck's seekable surface. */}
      {mode === 'playback' && track ? (
        <Panel className="deck__style">
          <TrackOverview
            label={t('deck.track.overview', { id: deckId })}
            peaks={trackPeaksData}
            position={track.position}
            duration={track.duration}
            loop={track.loop}
            accent={deckId}
            onSeek={onSeekTrack}
          />
          <div className="deck__track-row">
            <p className="deck__active-prompt">{track.title}</p>
            <Button onClick={onLeavePlayback}>
              {t('deck.track.backToLive')}
            </Button>
          </div>
          {/* Beat-matching controls (M20, ADR-0014): varispeed and
              tempo SYNC; phase stays the performer's via the jog. */}
          <div className="deck__track-row">
            <Knob
              label={t('deck.track.tempo')}
              accent={deckId}
              value={track.rate}
              min={1 - TRACK_RATE_RANGE}
              max={1 + TRACK_RATE_RANGE}
              step={0.001}
              resetValue={1}
              onChange={onSetTrackRate}
            />
            <Button
              disabled={track.bpm === null}
              onClick={() => {
                const result = onSyncTrack()
                setSyncRefusal(
                  result === 'synced'
                    ? null
                    : { loadId: track.loadId, reason: result },
                )
              }}
            >
              {t('deck.track.sync')}
            </Button>
          </div>
          {syncRefusal && syncRefusal.loadId === track.loadId && (
            <p className="deck__error" role="alert">
              {t(
                syncRefusal.reason === 'no_tempo'
                  ? 'deck.track.syncNoTempo'
                  : 'deck.track.syncOutOfRange',
              )}
            </p>
          )}
          {/* Hot cues (M21, ADR-0015): pads mean position. SHIFT+click
              clears — the on-screen twin of the shift pad layer. */}
          <div
            className="deck__cue-pads"
            role="group"
            aria-label={t('deck.track.cues')}
          >
            {track.cues.map((cue, index) => (
              <Button
                key={index}
                lit={cue !== null}
                aria-label={t('deck.track.cue', { n: index + 1 })}
                title={cue !== null ? formatTrackTime(cue) : undefined}
                onClick={(event) =>
                  event.shiftKey ? onClearHotCue(index) : onHotCuePad(index)
                }
              >
                {index + 1}
              </Button>
            ))}
          </div>
          {/* Track loop (M21): IN arms a start, OUT closes the region
              on the beat where the grid is confident, EXIT releases. */}
          <div className="deck__track-row">
            <Button lit={track.pendingLoopIn !== null} onClick={onLoopIn}>
              {t('deck.track.loopIn')}
            </Button>
            <Button disabled={track.pendingLoopIn === null} onClick={onLoopOut}>
              {t('deck.track.loopOut')}
            </Button>
            <Button
              variant={track.loop ? 'primary' : 'default'}
              disabled={!track.loop}
              onClick={onLoopExit}
            >
              {t('deck.track.loopExit')}
            </Button>
            {track.loop && loopBeatLabel(track.loop, track.grid) !== null && (
              <span className="deck__loop-length">
                {t('deck.track.loopBeats', {
                  beats: loopBeatLabel(track.loop, track.grid),
                })}
              </span>
            )}
          </div>
          {/* Beat loops (M23, ADR-0016): a one-press 4-beat loop
              (grid-required, so inert without one), then halve/double
              the active region. */}
          <div className="deck__track-row">
            <Button disabled={!track.grid} onClick={() => onBeatLoop(4)}>
              {t('deck.track.beatLoop')}
            </Button>
            <Button disabled={!track.loop} onClick={onHalveLoop}>
              {t('deck.track.loopHalve')}
            </Button>
            <Button disabled={!track.loop} onClick={onDoubleLoop}>
              {t('deck.track.loopDouble')}
            </Button>
          </div>
        </Panel>
      ) : (
      <Panel className="deck__style">
        <div className="deck__prompt-row">
          <TextField
            label={t('deck.style.target')}
            placeholder={t('deck.style.targetPlaceholder')}
            data-shortcut={`deck-${deckId}-prompt`}
            value={targetDraft}
            onChange={(event) => setTargetDraft(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Enter') addTarget()
            }}
          />
          <Button
            onClick={addTarget}
            disabled={
              !operable ||
              !targetDraft.trim() ||
              targets.length >= MAX_TARGETS ||
              targets.some((target) => target.text === targetDraft.trim())
            }
          >
            {t('deck.style.addTarget')}
          </Button>
          <Button
            onClick={() => void sampleOtherDeck()}
            lit={sampling}
            disabled={
              !operable || !canSample || sampling || targets.length >= MAX_TARGETS
            }
          >
            {t('deck.style.sampleOther', {
              deck: (deckId === 'a' ? 'b' : 'a').toUpperCase(),
            })}
          </Button>
        </div>

        {/* The 2D prompt pad, carrying the performance door in its overlay
            slot (issue #48): the door slides across the pad surface only —
            deck A from the left, deck B from the right — leaving the pad
            label clear. */}
        <XYPad
          label={t('deck.style.pad')}
          targets={padTargets}
          cursor={cursor}
          disabled={!operable || targets.length === 0}
          onChange={handleCursor}
          onTargetMove={handleTargetMove}
          selectedIds={selected}
          onCursorActivate={handleCursorActivate}
        >
          <PerformanceDrawer deckId={deckId} deckIndex={deckId === 'a' ? 0 : 1} />
        </XYPad>

        {targets.length > 0 && (
          <ul className="deck__targets">
            {targets.map((target) => (
              <li key={target.text} className="deck__target-row">
                {editing?.text === target.text ? (
                  <input
                    className="deck__target-edit"
                    value={editing.draft}
                    autoFocus
                    aria-label={t('deck.style.editTarget', { prompt: target.text })}
                    onChange={(event) =>
                      setEditing({ text: target.text, draft: event.target.value })
                    }
                    onKeyDown={(event) => {
                      if (event.key === 'Enter') commitEdit(true)
                      if (event.key === 'Escape') cancelEdit()
                    }}
                    onBlur={() => commitEdit()}
                  />
                ) : (
                  <>
                    <span className="deck__target-text">{target.text}</span>
                    <button
                      ref={(element) => {
                        if (element) editButtons.current.set(target.text, element)
                        else editButtons.current.delete(target.text)
                      }}
                      className="deck__target-action"
                      onClick={() => {
                        // Sampled chips (M15) have no text to edit —
                        // their label names a captured moment, not a
                        // prompt. aria-disabled (not disabled) keeps
                        // the button focusable so that reasoning is
                        // announced rather than skipped.
                        if (target.sample) return
                        setEditing({ text: target.text, draft: target.text })
                      }}
                      disabled={!operable}
                      aria-disabled={!operable || Boolean(target.sample)}
                      aria-label={t('deck.style.editTarget', {
                        prompt: target.text,
                      })}
                    >
                      ✎
                    </button>
                    <button
                      className="deck__target-action"
                      onClick={() => removeTarget(target.text)}
                      disabled={!operable}
                      aria-label={t('deck.style.removeTarget', {
                        prompt: target.text,
                      })}
                    >
                      ✕
                    </button>
                  </>
                )}
              </li>
            ))}
          </ul>
        )}
        <p className="deck__active-prompt">{activeSummary}</p>
        {sampleError && (
          <p className="deck__error" role="alert">
            {t('deck.style.sampleFailed', { message: sampleError })}
          </p>
        )}

        {/* Crates (M16): the pad's text targets + FX become a named
            preset; sampled chips are excluded (session-only, M15). */}
        <div className="deck__preset-row">
          <TextField
            label={t('deck.style.presetName')}
            value={presetDraft}
            onChange={(event) => setPresetDraft(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Enter') savePreset()
            }}
          />
          <Button
            onClick={savePreset}
            disabled={
              !presetDraft.trim() || targets.every((target) => target.sample)
            }
          >
            {t('deck.style.savePreset')}
          </Button>
        </div>

      </Panel>
      )}

      <div className="deck__fx" role="group" aria-label={t('deck.fx.title')}>
        <div className="deck__fx-select">
          <Select
            label={t('deck.fx.effect')}
            value={fx.kind ?? ''}
            options={[
              { value: '', label: t('deck.fx.off') },
              ...FX_KINDS.map((kind) => ({
                value: kind,
                label: t(`deck.fx.names.${kind}`),
              })),
            ]}
            onChange={(value) =>
              onSetFx(FX_KINDS.find((kind) => kind === value) ?? null)
            }
          />
        </div>
        <Knob
          label={t('deck.fx.amount')}
          accent={deckId}
          value={fx.amount}
          disabled={!fx.kind}
          resetValue={fx.kind ? fxRestPosition(fx.kind) : 0}
          onChange={onSetFxAmount}
        />
      </div>

      {/* Freeze pads (M13): lit = slot filled, accented = looping on
          air, ellipsis = a generation in flight (M18). Shift+click
          clears a slot — the same chord as SHIFT+pad on the hardware
          bank. */}
      <div className="deck__loop" role="group" aria-label={t('deck.loop.title')}>
        <div className="deck__loop-slots">
          {Array.from({ length: LOOP_SLOT_COUNT }, (_, slot) => {
            const slotState = loop.slots[slot]
            // Sounding = the active (replacing) freeze OR a stacked layer (ADR-0022).
            const playing = loop.active === slot || loop.layering.includes(slot)
            return (
              <Button
                key={slot}
                lit={slotState.state === 'filled'}
                variant={playing ? 'primary' : 'default'}
                aria-label={
                  slotState.state === 'pending'
                    ? t('deck.loop.slotPending', { n: slot + 1 })
                    : t('deck.loop.slot', { n: slot + 1 })
                }
                aria-pressed={playing}
                disabled={!operable || slotState.state === 'pending'}
                title={
                  slotState.state === 'empty'
                    ? undefined
                    : (slotState.label ?? undefined)
                }
                onClick={(event) =>
                  event.shiftKey ? onClearLoopPad(slot) : onLoopPad(slot)
                }
              >
                {slotState.state === 'pending' ? '…' : slot + 1}
              </Button>
            )
          })}
        </div>
        <Select
          label={t('deck.loop.length')}
          value={String(loop.seconds)}
          options={LOOP_LENGTH_OPTIONS.map((seconds) => ({
            value: String(seconds),
            label: t('deck.loop.lengthOption', { seconds }),
          }))}
          onChange={(value) => onSetLoopSeconds(Number(value))}
        />
      </div>

      {/* Generated pads (M18, ADR-0012): a prompt fills the first empty
          slot — one-shots overlay the deck, loops replace it like a
          capture and share the length picker above. The engine picks
          the sound world: Stable Audio's models, or the booth's own
          third Magenta engine (its first use pays the model load
          inside the pending state). */}
      <div
        className="deck__generate"
        role="group"
        aria-label={t('deck.generate.title')}
      >
        <div className="deck__generate-options">
          <Select
            label={t('deck.generate.engine')}
            value={generateEngine}
            options={[
              { value: 'sfx', label: t('deck.generate.engineSfx') },
              { value: 'music', label: t('deck.generate.engineMusic') },
              { value: 'magenta', label: t('deck.generate.engineMagenta') },
            ]}
            onChange={(value) => setGenerateEngine(value as GenerateEngine)}
          />
          <Select
            label={t('deck.generate.kind')}
            value={generateOneShot ? 'oneshot' : 'loop'}
            options={[
              { value: 'oneshot', label: t('deck.generate.kindOneShot') },
              { value: 'loop', label: t('deck.generate.kindLoop') },
            ]}
            onChange={(value) => setGenerateOneShot(value === 'oneshot')}
          />
        </div>
        <LoraControl
          accent={deckId}
          adapters={loras}
          kind={generateEngine}
          value={padStack.stack}
          onToggle={padStack.toggle}
          onStrength={padStack.setStrength}
          onToggleBypass={padStack.toggleBypass}
        />
        <div className="deck__generate-row">
          <div className="deck__generate-prompt">
            <TextField
              label={t('deck.generate.prompt')}
              value={generateDraft}
              maxLength={GENERATE_PROMPT_MAX_LENGTH}
              disabled={!connected}
              onChange={(event) => setGenerateDraft(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === 'Enter') fireGenerate()
              }}
            />
          </div>
          <Button disabled={!canGenerate} onClick={fireGenerate}>
            {t('deck.generate.action')}
          </Button>
        </div>
      </div>
      {generateError && (
        <p className="deck__error" role="alert">
          {t('deck.generate.failed', { message: generateError })}
        </p>
      )}

      <div className="deck__transport">
        {(mode === 'playback' ? (track?.playing ?? false) : state.playing) ? (
          <TransportButton
            kind="stop"
            accent={deckId}
            lit
            label={t('deck.stop')}
            disabled={mode === 'playback' ? track === null : !operable}
            onClick={onStop}
          />
        ) : (
          <TransportButton
            kind="play"
            accent={deckId}
            label={t('deck.play')}
            disabled={mode === 'playback' ? track === null : !operable}
            onClick={onPlay}
          />
        )}
        {mode === 'playback' && track ? (
          /* A track's health is its clock, not the stream's plumbing. */
          <div className="deck__health">
            <Stat
              label={t('deck.health.position')}
              value={t('deck.track.time', {
                position: formatTrackTime(track.position),
                duration: formatTrackTime(track.duration),
              })}
            />
            <Stat
              label={t('deck.health.bpm')}
              value={
                track.bpm === null
                  ? t('deck.health.noData')
                  : (track.bpm * track.rate).toFixed(1)
              }
            />
          </div>
        ) : (
        <div className="deck__health">
          <Meter
            label={t('deck.health.buffer')}
            valueLabel={t('deck.health.bufferSeconds', {
              seconds: state.bufferedSeconds.toFixed(1),
            })}
            fraction={bufferFraction}
            tone={bufferTone}
          />
          <Stat
            label={t('deck.health.bpm')}
            value={bpm === null ? t('deck.health.noData') : bpm.toFixed(1)}
          />
          <Stat
            label={t('deck.health.underruns')}
            value={String(state.underruns)}
            tone={state.underruns > 0 ? 'danger' : 'default'}
          />
          <Stat
            label={t('deck.health.generationSpeed')}
            value={
              state.generationSpeed === null
                ? t('deck.health.noData')
                : t('deck.health.generationSpeedValue', {
                    rtf: state.generationSpeed.toFixed(2),
                  })
            }
            tone={
              state.generationSpeed !== null && state.generationSpeed < 1
                ? 'danger'
                : 'default'
            }
          />
        </div>
        )}
      </div>

      {state.workerDied && (
        <div className="deck__recovery" role="alert">
          <p className="deck__error">{t('deck.worker.died')}</p>
          {/* The model picker normally lives in settings, but switching to a
              model that fits is the recovery path when the chosen one cannot
              load — so it rides along here, where the crash is surfaced. */}
          <Select
            label={t('deck.model.label')}
            value={state.model ?? ''}
            options={state.availableModels.length ? state.availableModels : [state.model ?? '']}
            disabled={!connected || state.switchingModel}
            onChange={onSetModel}
          />
          <Button onClick={onRestart} disabled={!connected}>
            {t('deck.worker.restart')}
          </Button>
        </div>
      )}

      {state.error && !state.workerDied && (
        <p className="deck__error" role="alert">
          {t('deck.error.message', { message: state.error })}
        </p>
      )}
    </section>
  )
}
