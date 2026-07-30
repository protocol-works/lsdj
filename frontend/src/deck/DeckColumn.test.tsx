import { act, fireEvent, render, screen, within } from '@testing-library/react'
import { StrictMode } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { DeckId } from '../audio/types'
import type { FxKind } from '../audio/fx'
import { createControlBus, type ControlBus } from '../control/bus'
import { ControlBusProvider } from '../control/ControlBusProvider'
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
  type DeckSnap,
  type InterfaceState,
} from '../audio/nativeEngine'
import * as interfaceStore from '../audio/interfaceStore'
import type { LoraAdapter } from '../audio/nativeEngine'
import { DeckColumn } from './DeckColumn'

// The installed-adapter list (issue #66) is stubbed per test; the default is
// none, which hides the pickers entirely.
const useLorasMock = vi.fn<() => LoraAdapter[]>(() => [])
vi.mock('../models/useLoras', async (importOriginal) => {
  const original = await importOriginal<typeof import('../models/useLoras')>()
  return { ...original, useLoras: () => useLorasMock() }
})

// The style intents are mocked flat (no rAF coalescing — that machinery has
// its own tests in nativeEngine.test.ts) and wired per test to a fake pad
// store below, so the projection tests exercise the full loop a real session
// runs: gesture → intent → store mutation → snapshot broadcast → render.
vi.mock('../audio/nativeEngine', async (importOriginal) => {
  const original = await importOriginal<typeof import('../audio/nativeEngine')>()
  return {
    ...original,
    styleAddTarget: vi.fn(),
    styleAddSampleTarget: vi.fn(),
    styleMoveTarget: vi.fn(),
    styleRemoveTarget: vi.fn(),
    styleRenameTarget: vi.fn(),
    styleToggleSelection: vi.fn(),
    styleFanOut: vi.fn(),
    styleSetCursor: vi.fn(),
    styleApplyPreset: vi.fn(),
  }
})
// The store projection, driveable per test (null = pre-hydration, the app's
// no-Tauri default): __setInterfaceStore pushes a snapshot into every
// mounted consumer, like a store://changed event would.
vi.mock('../audio/interfaceStore', async (importOriginal) => {
  const { useSyncExternalStore } = await import('react')
  const original = await importOriginal<typeof import('../audio/interfaceStore')>()
  let current: unknown = null
  const listeners = new Set<() => void>()
  return {
    ...original,
    useInterfaceStore: () =>
      useSyncExternalStore(
        (listener) => {
          listeners.add(listener)
          return () => listeners.delete(listener)
        },
        () => current,
      ),
    __setInterfaceStore: (next: unknown) => {
      current = next
      for (const listener of listeners) listener()
    },
  }
})
import { initialDeckState, type DeckState } from './deckState'
import { GENERATE_PROMPT_MAX_LENGTH } from './useDeck'
import type { DeckMode, LoopState, TrackState } from './useDeck'

const noop = () => {}

const setStore = (
  interfaceStore as unknown as {
    __setInterfaceStore: (next: InterfaceState | null) => void
  }
).__setInterfaceStore

function deckSnap(over: Partial<DeckSnap> = {}): DeckSnap {
  return {
    volume: 1,
    eq: { low: 0.5, mid: 0.5, high: 0.5 },
    trimDb: 0,
    cue: false,
    onAir: true,
    fx: { kind: null, amount: 0 },
    model: null,
    playing: false,
    mode: 'realtime',
    cues: [],
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
    ...over,
  }
}

function storeWith(over: Partial<DeckSnap>): InterfaceState {
  return {
    decks: [deckSnap(over), deckSnap()],
    crossfade: 0.5,
    cueMix: 0.5,
    recording: { active: false, path: null },
    mainDevice: '',
    cueDevice: '',
    recordingsFolder: '',
  }
}

/** A webview-side twin of the Rust store's style semantics (the real rules
 * are unit-tested in store.rs / style.rs): the mocked intents mutate it and
 * it broadcasts a fresh snapshot, exactly like the shell store would. The
 * spawn geometry is simplified (12 o'clock, then 6) — position-sensitive
 * assertions seed explicit layouts instead of adding through it. */
class FakePadStore {
  targets: { x: number; y: number; text: string; sample?: string }[] = []
  selected: boolean[] = []
  cursor = { x: 0.5, y: 0.5 }

  broadcast() {
    act(() =>
      setStore(
        storeWith({
          styleTargets: [...this.targets],
          styleSelected: [...this.selected],
          cursor: { ...this.cursor },
        }),
      ),
    )
  }

  seed(
    targets: { x: number; y: number; text: string; sample?: string }[],
    cursor = { x: 0.5, y: 0.5 },
  ) {
    this.targets = targets
    this.selected = targets.map(() => false)
    this.cursor = cursor
    this.broadcast()
  }
}

/** Wire every mocked style intent to a fresh fake store for this test. */
function wirePadStore(): FakePadStore {
  const pad = new FakePadStore()
  const spawn = () => (pad.targets.length === 0 ? { x: 0.5, y: 0.12 } : { x: 0.5, y: 0.88 })
  vi.mocked(styleAddTarget).mockImplementation((_deck, text) => {
    const trimmed = text.trim()
    if (!trimmed || pad.targets.length >= 8 || pad.targets.some((t) => t.text === trimmed)) return
    pad.targets = [...pad.targets, { ...spawn(), text: trimmed }]
    pad.selected = [...pad.selected, false]
    pad.broadcast()
  })
  vi.mocked(styleAddSampleTarget).mockImplementation((_deck, label, sample) => {
    if (pad.targets.length >= 8 || pad.targets.some((t) => t.text === label)) return
    pad.targets = [...pad.targets, { ...spawn(), text: label, sample }]
    pad.selected = [...pad.selected, false]
    pad.broadcast()
  })
  vi.mocked(styleMoveTarget).mockImplementation((_deck, text, x, y) => {
    pad.targets = pad.targets.map((t) => (t.text === text ? { ...t, x, y } : t))
    pad.broadcast()
  })
  vi.mocked(styleRemoveTarget).mockImplementation((_deck, text) => {
    const index = pad.targets.findIndex((t) => t.text === text)
    if (index < 0) return
    pad.targets = pad.targets.filter((_, i) => i !== index)
    pad.selected = pad.selected.filter((_, i) => i !== index)
    pad.broadcast()
  })
  vi.mocked(styleRenameTarget).mockImplementation((_deck, from, to) => {
    const target = pad.targets.find((t) => t.text === from)
    if (!target || target.sample || pad.targets.some((t) => t.text === to)) return
    pad.targets = pad.targets.map((t) => (t.text === from ? { ...t, text: to } : t))
    pad.broadcast()
  })
  vi.mocked(styleToggleSelection).mockImplementation((_deck, text) => {
    const index = pad.targets.findIndex((t) => t.text === text)
    if (index < 0) return
    pad.selected = pad.selected.map((on, i) => (i === index ? !on : on))
    pad.broadcast()
  })
  vi.mocked(styleFanOut).mockImplementation(() => {
    // The real circle geometry lives in style.rs; two slots are enough here.
    pad.targets = pad.targets.map((t, i) => ({
      ...t,
      x: 0.5,
      y: i === 0 ? 0.12 : 0.88,
    }))
    pad.cursor = { x: 0.5, y: 0.5 }
    pad.broadcast()
  })
  vi.mocked(styleSetCursor).mockImplementation((_deck, x, y) => {
    pad.cursor = { x, y }
    pad.broadcast()
  })
  vi.mocked(styleApplyPreset).mockImplementation((_deck, targets, cursor) => {
    pad.targets = targets.map((t) => ({ ...t }))
    pad.selected = targets.map(() => false)
    pad.cursor = { ...cursor }
    pad.broadcast()
  })
  return pad
}

let pad: FakePadStore

beforeEach(() => {
  vi.clearAllMocks()
  pad = wirePadStore()
  setStore(null)
})

const emptyLoop = (): LoopState => ({
  slots: Array.from({ length: 4 }, () => ({ state: 'empty' })),
  active: null,
  layering: [],
  seconds: 4,
})

function renderPanel(
  state: Partial<DeckState>,
  handlers: Record<string, () => void> = {},
  bus: ControlBus = createControlBus(),
  fx: { kind: FxKind | null; amount: number } = { kind: null, amount: 0 },
  loop: LoopState = emptyLoop(),
  bpm: number | null = null,
  canSample = true,
  generateError: string | null = null,
  playback: { mode: DeckMode; track: TrackState | null } = {
    mode: 'realtime',
    track: null,
  },
  shiftedDeck: DeckId | null = null,
) {
  return render(
    <ControlBusProvider bus={bus}>
      <DeckColumn
        deckId="a"
        state={{ ...initialDeckState, ...state }}
        onPlay={handlers.onPlay ?? noop}
        onStop={handlers.onStop ?? noop}
        onSetModel={(handlers.onSetModel as (m: string) => void) ?? noop}
        onRestart={handlers.onRestart ?? noop}
        shiftedDeck={shiftedDeck}
        fx={fx}
        onSetFx={(handlers.onSetFx as (k: unknown) => void) ?? noop}
        onSetFxAmount={(handlers.onSetFxAmount as (v: number) => void) ?? noop}
        loop={loop}
        onLoopPad={(handlers.onLoopPad as (slot: number) => void) ?? noop}
        onClearLoopPad={
          (handlers.onClearLoopPad as (slot: number) => void) ?? noop
        }
        onSetLoopSeconds={
          (handlers.onSetLoopSeconds as (seconds: number) => void) ?? noop
        }
        onGenerateToPad={
          (handlers.onGenerateToPad as (prompt: string, kind: string) => void) ??
          noop
        }
        generateError={generateError}
        bpm={bpm}
        onSampleOtherDeck={
          (handlers.onSampleOtherDeck as () => Promise<{
            label: string
            sample: string
          } | null>) ?? (async () => null)
        }
        canSample={canSample}
        onSavePreset={
          (handlers.onSavePreset as (preset: object) => void) ?? noop
        }
        mode={playback.mode}
        track={playback.track}
        onLeavePlayback={handlers.onLeavePlayback ?? noop}
        onSeekTrack={(handlers.onSeekTrack as (s: number) => void) ?? noop}
        onSetTrackRate={(handlers.onSetTrackRate as (r: number) => void) ?? noop}
        onSyncTrack={
          (handlers.onSyncTrack as () => 'synced') ?? (() => 'synced' as const)
        }
        onHotCuePad={(handlers.onHotCuePad as (i: number) => void) ?? noop}
        onClearHotCue={(handlers.onClearHotCue as (i: number) => void) ?? noop}
        onLoopIn={handlers.onLoopIn ?? noop}
        onLoopOut={handlers.onLoopOut ?? noop}
        onLoopExit={handlers.onLoopExit ?? noop}
        onBeatLoop={(handlers.onBeatLoop as (b: number) => void) ?? noop}
        onHalveLoop={handlers.onHalveLoop ?? noop}
        onDoubleLoop={handlers.onDoubleLoop ?? noop}
        getTrackPeaks={() => null}
      />
    </ControlBusProvider>,
  )
}

describe('DeckColumn', () => {
  it('makes underruns visible, highlighted when above zero', () => {
    renderPanel({ connection: 'open', playing: true, underruns: 3 })
    const stat = screen.getByText('Underruns').parentElement!
    expect(stat).toHaveTextContent('3')
    expect(stat).toHaveClass('ui-stat--danger')
  })

  it('shows the buffer level in seconds', () => {
    renderPanel({ connection: 'open', bufferedSeconds: 2.4 })
    expect(screen.getByText('2.4s')).toBeInTheDocument()
  })

  it('flags a generation speed below real time', () => {
    renderPanel({ connection: 'open', generationSpeed: 0.84 })
    const stat = screen.getByText('Gen speed').parentElement!
    expect(stat).toHaveTextContent('0.84×')
    expect(stat).toHaveClass('ui-stat--danger')
  })

  it('disables transport while the worker is dead', () => {
    renderPanel({ connection: 'open', workerDied: true })
    expect(screen.getByRole('button', { name: 'Play' })).toBeDisabled()
  })

  it('starts playback from the play button', () => {
    const onPlay = vi.fn()
    renderPanel({ connection: 'open' }, { onPlay })
    fireEvent.click(screen.getByRole('button', { name: 'Play' }))
    expect(onPlay).toHaveBeenCalled()
  })

  it('stops playback from the stop button while playing', () => {
    const onStop = vi.fn()
    renderPanel({ connection: 'open', playing: true }, { onStop })
    fireEvent.click(screen.getByRole('button', { name: 'Stop' }))
    expect(onStop).toHaveBeenCalled()
  })

  function addTarget(text: string) {
    fireEvent.change(screen.getByLabelText('Style target'), {
      target: { value: text },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Add' }))
  }

  it('adds a trimmed target through the add intent and projects the echo', () => {
    renderPanel({ connection: 'open' })
    addTarget('  warm disco funk  ')
    // The add crosses as an intent (the store owns the spawn geometry and
    // the blend — style.rs); the chip renders from the store's echo.
    expect(styleAddTarget).toHaveBeenCalledWith(0, 'warm disco funk')
    expect(
      screen.getByRole('button', { name: 'Remove warm disco funk' }),
    ).toBeInTheDocument()
  })

  it('removes a target from its chip through the remove intent', () => {
    renderPanel({ connection: 'open' })
    addTarget('funk')
    addTarget('techno')
    fireEvent.click(screen.getByRole('button', { name: 'Remove funk' }))
    expect(styleRemoveTarget).toHaveBeenCalledWith(0, 'funk')
    expect(
      screen.queryByRole('button', { name: 'Remove funk' }),
    ).not.toBeInTheDocument()
    expect(
      screen.getByRole('button', { name: 'Remove techno' }),
    ).toBeInTheDocument()
  })

  function editTarget(prompt: string, replacement: string) {
    fireEvent.click(screen.getByRole('button', { name: `Edit ${prompt}` }))
    const field = screen.getByRole('textbox', { name: `Edit ${prompt}` })
    fireEvent.change(field, { target: { value: replacement } })
    fireEvent.keyDown(field, { key: 'Enter' })
  }

  it('edits a prompt in place through the rename intent, keeping its spot', () => {
    renderPanel({ connection: 'open' })
    addTarget('fnuk')
    addTarget('techno')
    const spotBefore = pad.targets[0]

    editTarget('fnuk', '  funk  ')
    expect(styleRenameTarget).toHaveBeenCalledWith(0, 'fnuk', 'funk')
    expect(
      screen.getByRole('button', { name: 'Remove funk' }),
    ).toBeInTheDocument()
    // The renamed target keeps its slot (and therefore its weight).
    expect(pad.targets[0]).toEqual({ ...spotBefore, text: 'funk' })
  })

  it('escape cancels an edit without touching the style', () => {
    renderPanel({ connection: 'open' })
    addTarget('funk')

    fireEvent.click(screen.getByRole('button', { name: 'Edit funk' }))
    const field = screen.getByRole('textbox', { name: 'Edit funk' })
    fireEvent.change(field, { target: { value: 'techno' } })
    fireEvent.keyDown(field, { key: 'Escape' })
    expect(screen.getByRole('button', { name: 'Remove funk' })).toBeInTheDocument()
    expect(styleRenameTarget).not.toHaveBeenCalled()
  })

  it('a rename that collides with another chip cancels quietly', () => {
    renderPanel({ connection: 'open' })
    addTarget('funk')
    addTarget('techno')

    editTarget('funk', 'techno')
    expect(screen.getByRole('button', { name: 'Remove funk' })).toBeInTheDocument()
    expect(styleRenameTarget).not.toHaveBeenCalled()
  })

  it.each([
    ['an emptied draft', '   '],
    ['an unchanged draft', 'funk'],
  ])('%s cancels quietly without a rename intent', (_label, replacement) => {
    renderPanel({ connection: 'open' })
    addTarget('funk')

    editTarget('funk', replacement)
    expect(screen.getByRole('button', { name: 'Remove funk' })).toBeInTheDocument()
    expect(styleRenameTarget).not.toHaveBeenCalled()
  })

  it('returns focus to the row after a keyboard commit or cancel', () => {
    renderPanel({ connection: 'open' })
    addTarget('fnuk')

    editTarget('fnuk', 'funk') // Enter
    expect(screen.getByRole('button', { name: 'Edit funk' })).toHaveFocus()

    fireEvent.click(screen.getByRole('button', { name: 'Edit funk' }))
    const field = screen.getByRole('textbox', { name: 'Edit funk' })
    fireEvent.keyDown(field, { key: 'Escape' })
    expect(screen.getByRole('button', { name: 'Edit funk' })).toHaveFocus()
  })

  it('an edit open when the deck becomes inoperable cancels instead of committing', () => {
    const bus = createControlBus()
    const view = renderPanel({ connection: 'open' }, {}, bus)
    addTarget('funk')

    fireEvent.click(screen.getByRole('button', { name: 'Edit funk' }))
    const field = screen.getByRole('textbox', { name: 'Edit funk' })
    fireEvent.change(field, { target: { value: 'techno' } })
    // The model switch starts while the edit is open.
    view.rerender(
      <ControlBusProvider bus={bus}>
        <DeckColumn
          deckId="a"
          state={{ ...initialDeckState, connection: 'open', switchingModel: true }}
          onPlay={noop}
          onStop={noop}
          onSetModel={noop as (m: string) => void}
          onRestart={noop}
          fx={{ kind: null, amount: 0 }}
          onSetFx={noop as (k: unknown) => void}
          onSetFxAmount={noop as (v: number) => void}
          loop={emptyLoop()}
          onGenerateToPad={noop as (prompt: string, kind: string) => void}
          generateError={null}
          onLoopPad={noop as (slot: number) => void}
          onClearLoopPad={noop as (slot: number) => void}
          onSetLoopSeconds={noop as (seconds: number) => void}
          bpm={null}
          onSampleOtherDeck={async () => null}
          canSample
          onSavePreset={noop as (preset: object) => void}
          mode="realtime"
          track={null}
          onLeavePlayback={noop}
          onSeekTrack={noop as (s: number) => void}
          onSetTrackRate={noop as (r: number) => void}
          onSyncTrack={() => 'synced' as const}
          onHotCuePad={noop}
          onClearHotCue={noop}
          onLoopIn={noop}
          onLoopOut={noop}
          onLoopExit={noop}
          onBeatLoop={noop}
          onHalveLoop={noop}
          onDoubleLoop={noop}
          getTrackPeaks={() => null}
        />
      </ControlBusProvider>,
    )
    fireEvent.keyDown(
      screen.getByRole('textbox', { name: 'Edit funk' }),
      { key: 'Enter' },
    )
    expect(
      screen.getByRole('button', { name: 'Remove funk' }),
    ).toBeInTheDocument()
    expect(styleRenameTarget).not.toHaveBeenCalled()
  })

  it('a preset load closes an open edit instead of leaving a stale draft', () => {
    const bus = createControlBus()
    renderPanel({ connection: 'open' }, {}, bus)
    addTarget('funk')
    fireEvent.click(screen.getByRole('button', { name: 'Edit funk' }))
    expect(screen.getByRole('textbox', { name: 'Edit funk' })).toBeInTheDocument()

    act(() =>
      bus.publish({
        kind: 'preset_load',
        deck: 'a',
        preset: {
          name: 'Other',
          targets: [{ text: 'dub', x: 0.5, y: 0.5 }],
          cursor: { x: 0.5, y: 0.5 },
          fx: { kind: null, amount: 0 },
        },
      }),
    )
    expect(
      screen.queryByRole('textbox', { name: 'Edit funk' }),
    ).not.toBeInTheDocument()
    // Re-adding the same text must render a plain row, not a
    // pre-opened editor with the stale draft.
    addTarget('funk')
    expect(
      screen.queryByRole('textbox', { name: 'Edit funk' }),
    ).not.toBeInTheDocument()
  })

  it('blurring the edit field commits like Enter', () => {
    renderPanel({ connection: 'open' })
    addTarget('funk')

    fireEvent.click(screen.getByRole('button', { name: 'Edit funk' }))
    const field = screen.getByRole('textbox', { name: 'Edit funk' })
    fireEvent.change(field, { target: { value: 'dub' } })
    fireEvent.blur(field)
    expect(styleRenameTarget).toHaveBeenCalledWith(0, 'funk', 'dub')
    expect(screen.getByRole('button', { name: 'Remove dub' })).toBeInTheDocument()
  })

  it('sampled chips are not editable — their label names a moment', async () => {
    const onSampleOtherDeck = vi.fn(async () => ({
      label: '⏺ B·1',
      sample: 'sample:b:1',
    }))
    renderPanel(
      { connection: 'open' },
      { onSampleOtherDeck: onSampleOtherDeck as unknown as () => void },
    )
    fireEvent.click(screen.getByRole('button', { name: 'Sample deck B' }))
    await screen.findByRole('button', { name: 'Remove ⏺ B·1' })
    // aria-disabled, not disabled: the button stays focusable so a
    // screen reader hears WHY instead of skipping the control.
    const edit = screen.getByRole('button', { name: 'Edit ⏺ B·1' })
    expect(edit).toHaveAttribute('aria-disabled', 'true')
    fireEvent.click(edit)
    expect(
      screen.queryByRole('textbox', { name: 'Edit ⏺ B·1' }),
    ).not.toBeInTheDocument()
  })

  it('keeps the pad locked until there are two targets to blend', () => {
    renderPanel({ connection: 'open' })
    expect(screen.getByLabelText('Style pad')).toHaveAttribute(
      'aria-disabled',
      'true',
    )
  })

  it('moves the cursor by keyboard through the cursor intent', () => {
    renderPanel({ connection: 'open' })
    addTarget('funk')
    addTarget('techno')

    fireEvent.keyDown(screen.getByLabelText('Style pad'), { key: 'ArrowUp' })
    // The gesture crosses as an intent (the blend recomputes shell-side);
    // the projected cursor follows the echo upward.
    expect(styleSetCursor).toHaveBeenCalled()
    expect(pad.cursor.x).toBeCloseTo(0.5)
    expect(pad.cursor.y).toBeLessThan(0.5)
  })

  it('drags a target dot and ships the move as a per-target intent', () => {
    renderPanel({ connection: 'open' })
    addTarget('funk')
    addTarget('techno')

    const surface = screen.getByLabelText('Style pad')
    vi.spyOn(surface, 'getBoundingClientRect').mockReturnValue({
      left: 0,
      top: 0,
      width: 100,
      height: 100,
      right: 100,
      bottom: 100,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    } as DOMRect)

    // Grab the funk dot (12 o'clock) and drop it beside the centred cursor.
    // The chip's text button also says 'funk' — address the pad dot's label.
    fireEvent.pointerDown(
      screen.getByText('funk', { selector: '.ui-xypad__target-label' }),
      { clientX: 50, clientY: 12, pointerId: 1 },
    )
    fireEvent.pointerMove(surface, { clientX: 51, clientY: 50, pointerId: 1 })
    fireEvent.pointerUp(surface, { pointerId: 1 })

    expect(styleMoveTarget).toHaveBeenCalledWith(
      0,
      'funk',
      expect.closeTo(0.51, 2),
      expect.closeTo(0.5, 2),
    )
    // The projection renders the dot where the store put it.
    expect(pad.targets[0]).toMatchObject({ text: 'funk' })
    expect(pad.targets[0].y).toBeCloseTo(0.5, 2)
  })

  it('projects a shell-hydrated arrangement on mount', () => {
    // The shell settings file hydrates the store before the webview exists
    // (phase B); the pad renders it from the first snapshot — no local copy,
    // no boot replay (the worker re-send is the shell sender's job now).
    pad.seed(
      [
        { text: 'funk', x: 0.2, y: 0.2 },
        { text: 'techno', x: 0.8, y: 0.8 },
      ],
      { x: 0.2, y: 0.2 },
    )
    renderPanel({ connection: 'open' })

    expect(screen.getByRole('button', { name: 'Remove funk' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Remove techno' })).toBeInTheDocument()
  })

  it('shows the active blend summary', () => {
    renderPanel({
      connection: 'open',
      activeStyle: {
        prompts: [
          { text: 'funk', weight: 0.7 },
          { text: 'techno', weight: 0.3 },
        ],
      },
    })
    expect(screen.getByText('Playing: 70% funk · 30% techno')).toBeInTheDocument()
  })

  it('offers the model picker in recovery and reports a selection', () => {
    // The model picker moved to settings; its in-deck twin survives only in the
    // worker-died recovery block (the "switch to a model that fits" path).
    const onSetModel = vi.fn()
    renderPanel(
      {
        connection: 'open',
        workerDied: true,
        model: 'mrt2_small',
        availableModels: ['mrt2_small', 'mrt2_base'],
      },
      { onSetModel: onSetModel as () => void },
    )
    fireEvent.change(screen.getByLabelText('Model'), {
      target: { value: 'mrt2_base' },
    })
    expect(onSetModel).toHaveBeenCalledWith('mrt2_base')
  })

  it('keeps the model picker out of the deck column outside recovery', () => {
    // A healthy deck shows no picker — model selection lives in settings now.
    renderPanel({
      connection: 'open',
      model: 'mrt2_small',
      availableModels: ['mrt2_small', 'mrt2_base'],
    })
    expect(screen.queryByLabelText('Model')).toBeNull()
  })

  it('locks the deck while a model is loading', () => {
    renderPanel({
      connection: 'open',
      switchingModel: true,
      model: 'mrt2_base',
      availableModels: ['mrt2_small', 'mrt2_base'],
    })
    expect(screen.getByText('Loading model…')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Play' })).toBeDisabled()
  })

  it('offers recovery when the worker died', () => {
    const onRestart = vi.fn()
    renderPanel(
      {
        connection: 'open',
        workerDied: true,
        model: 'mrt2_base',
        availableModels: ['mrt2_small', 'mrt2_base'],
      },
      { onRestart },
    )
    expect(screen.getByRole('alert')).toHaveTextContent('The deck engine crashed.')
    fireEvent.click(screen.getByRole('button', { name: 'Restart deck' }))
    expect(onRestart).toHaveBeenCalled()
    expect(screen.getByRole('button', { name: 'Play' })).toBeDisabled()
    // Recovery from a model that cannot load is switching to one that can —
    // the picker must stay usable while the worker is dead.
    expect(screen.getByLabelText('Model')).toBeEnabled()
  })

  it('announces worker errors', () => {
    renderPanel({ connection: 'open', error: 'generation failed; deck stopped' })
    expect(screen.getByRole('alert')).toHaveTextContent(
      'generation failed; deck stopped',
    )
  })

  it('selects a Color FX by name and reports knob moves', () => {
    const onSetFx = vi.fn()
    const onSetFxAmount = vi.fn()
    renderPanel(
      { connection: 'open' },
      { onSetFx: onSetFx as () => void, onSetFxAmount: onSetFxAmount as () => void },
      createControlBus(),
      { kind: 'filter', amount: 0.5 },
    )

    fireEvent.change(screen.getByLabelText('Effect'), {
      target: { value: 'dub_echo' },
    })
    expect(onSetFx).toHaveBeenCalledWith('dub_echo')
    // The option shows translated copy while the value stays the kind.
    expect(screen.getByRole('option', { name: 'Dub Echo' })).toHaveValue(
      'dub_echo',
    )

    fireEvent.change(screen.getByLabelText('FX amount'), {
      target: { value: '0.8' },
    })
    expect(onSetFxAmount).toHaveBeenCalledWith(0.8)
  })

  it('switching FX off and disabling the knob', () => {
    const onSetFx = vi.fn()
    renderPanel(
      { connection: 'open' },
      { onSetFx: onSetFx as () => void },
      createControlBus(),
      { kind: null, amount: 0 },
    )
    expect(screen.getByLabelText('Effect')).toHaveValue('')
    expect(screen.getByLabelText('FX amount')).toBeDisabled()

    fireEvent.change(screen.getByLabelText('Effect'), {
      target: { value: '' },
    })
    expect(onSetFx).toHaveBeenCalledWith(null)
  })

  it('toggles a pad target into and out of the net selection', () => {
    const bus = createControlBus()
    renderPanel({ connection: 'open' }, {}, bus)
    addTarget('funk')
    addTarget('techno')

    act(() => bus.publish({ kind: 'hot_cue_pad', deck: 'a', index: 1 }))
    // The toggle crosses as an intent; the store owns the mask.
    expect(styleToggleSelection).toHaveBeenCalledWith(0, 'techno')
    expect(pad.selected).toEqual([false, true])

    // Re-tapping the same pad deselects it.
    act(() => bus.publish({ kind: 'hot_cue_pad', deck: 'a', index: 1 }))
    expect(pad.selected).toEqual([false, false])
  })

  it('highlights a selected dot in the net', () => {
    const bus = createControlBus()
    const { container } = renderPanel({ connection: 'open' }, {}, bus)
    addTarget('funk')
    expect(container.querySelector('.ui-xypad__strand--selected')).toBeNull()

    act(() => bus.publish({ kind: 'hot_cue_pad', deck: 'a', index: 0 }))
    expect(container.querySelector('.ui-xypad__strand--selected')).not.toBeNull()
    expect(
      container.querySelector('.ui-xypad__target-dot--selected'),
    ).not.toBeNull()
  })

  it('reels a selected dot toward the hub on a clockwise jog', () => {
    const bus = createControlBus()
    renderPanel({ connection: 'open' }, {}, bus)
    addTarget('funk') // 12 o'clock
    addTarget('techno') // 6 o'clock — symmetric about the centred cursor
    act(() => bus.publish({ kind: 'hot_cue_pad', deck: 'a', index: 1 }))

    // Clockwise (positive steps) reels ONLY the selected techno inward:
    // one move intent, for techno, closer to the centred cursor.
    act(() =>
      bus.publish({ kind: 'track_seek', deck: 'a', steps: 1, shifted: false }),
    )
    expect(styleMoveTarget).toHaveBeenCalledTimes(1)
    const [, text, , y] = vi.mocked(styleMoveTarget).mock.calls[0]
    expect(text).toBe('techno')
    expect(y).toBeLessThan(0.88) // reeled up from 6 o'clock toward the hub
    expect(y).toBeGreaterThan(0.5)
  })

  it('leaves the realtime jog inert when nothing is selected', () => {
    const bus = createControlBus()
    renderPanel({ connection: 'open' }, {}, bus)
    addTarget('funk')

    act(() =>
      bus.publish({ kind: 'track_seek', deck: 'a', steps: 1, shifted: false }),
    )

    expect(styleMoveTarget).not.toHaveBeenCalled()
    expect(styleSetCursor).not.toHaveBeenCalled()
  })

  it('centres the blue dot and fans the dots out on double-click', () => {
    // Two dots clustered, cursor parked off-centre.
    pad.seed(
      [
        { text: 'funk', x: 0.2, y: 0.2 },
        { text: 'techno', x: 0.7, y: 0.6 },
      ],
      { x: 0.25, y: 0.8 },
    )
    const { container } = renderPanel({ connection: 'open' })

    fireEvent.doubleClick(container.querySelector('[data-cursor]')!)
    expect(styleFanOut).toHaveBeenCalledWith(0)

    // The blue dot parks at the canvas centre…
    const cursorStyle = container
      .querySelector('.ui-xypad__cursor')!
      .getAttribute('style')
    expect(cursorStyle).toContain('left: 50%')
    expect(cursorStyle).toContain('top: 50%')
    // …and the dots fan onto the spawn circle (top and bottom of the pad).
    expect(
      container.querySelector('[data-target-id="funk"]')!.getAttribute('style'),
    ).toContain('top: 12')
    expect(
      container
        .querySelector('[data-target-id="techno"]')!
        .getAttribute('style'),
    ).toContain('top: 88')
  })

  it('drops a prompt from the selection when it is removed', () => {
    const bus = createControlBus()
    renderPanel({ connection: 'open' }, {}, bus)
    addTarget('funk')
    addTarget('techno')
    act(() => bus.publish({ kind: 'hot_cue_pad', deck: 'a', index: 0 }))
    expect(pad.selected).toEqual([true, false])

    fireEvent.click(screen.getByRole('button', { name: 'Remove funk' }))
    // funk is gone; the store realigns the mask, techno stays unselected.
    expect(pad.selected).toEqual([false])
  })

  it('projects an external (MCP) arrangement the moment the store broadcasts it', () => {
    // The store is the ONLY truth (phase B): there is no local pad copy to
    // fight, no adoption gate — an agent's write replaces the pad exactly
    // like our own echo does. (The old writer-gated mirror, and the
    // playing-deck prompt-revert class it fenced, are gone by construction.)
    renderPanel({ connection: 'open' })
    addTarget('funk')
    expect(screen.getAllByText('funk').length).toBeGreaterThan(0)

    act(() =>
      setStore(
        storeWith({
          styleTargets: [{ x: 0.5, y: 0.5, text: 'agent groove' }],
          styleSelected: [false],
        }),
      ),
    )
    expect(screen.getAllByText('agent groove').length).toBeGreaterThan(0)
    expect(screen.queryAllByText('funk')).toHaveLength(0)
  })

  // Deck A is the shifted deck, so its jogs steer its cursor in 2D.
  function steerPanel(bus: ControlBus) {
    pad.seed(
      [
        { text: 'funk', x: 0.2, y: 0.2 },
        { text: 'techno', x: 0.8, y: 0.8 },
      ],
      { x: 0.5, y: 0.5 },
    )
    return renderPanel(
      { connection: 'open' },
      {},
      bus,
      { kind: null, amount: 0 },
      emptyLoop(),
      null,
      true,
      null,
      { mode: 'realtime', track: null },
      'a',
    )
  }

  it('steers the cursor with SHIFT+jog — jog A on x, jog B on y', () => {
    const bus = createControlBus()
    const { container } = steerPanel(bus)
    const cursorStyle = () =>
      container.querySelector('.ui-xypad__cursor')!.getAttribute('style')

    // Jog A clockwise → right (x up): 0.5 + 10·0.01 = 0.6.
    act(() =>
      bus.publish({ kind: 'track_seek', deck: 'a', steps: 10, shifted: true }),
    )
    expect(cursorStyle()).toContain('left: 60%')

    // Jog A counter-clockwise → left: 0.6 − 30·0.01 = 0.3.
    act(() =>
      bus.publish({ kind: 'track_seek', deck: 'a', steps: -30, shifted: true }),
    )
    expect(cursorStyle()).toContain('left: 30%')

    // Jog B clockwise → down (y up): 0.5 + 10·0.01 = 0.6. (Its own SHIFT is not
    // held, but deck A's is — that's what routes it here.)
    act(() =>
      bus.publish({ kind: 'track_seek', deck: 'b', steps: 10, shifted: false }),
    )
    expect(cursorStyle()).toContain('top: 60%')
  })

  it('SHIFT+jog steers instead of reeling the selected dots', () => {
    const bus = createControlBus()
    const { container } = steerPanel(bus)
    // Select a dot — without SHIFT a jog would reel it.
    act(() => bus.publish({ kind: 'hot_cue_pad', deck: 'a', index: 0 }))
    const dotStyle = () =>
      container.querySelector('[data-target-id="funk"]')!.getAttribute('style')
    const before = dotStyle()

    act(() =>
      bus.publish({ kind: 'track_seek', deck: 'a', steps: 10, shifted: true }),
    )

    // The dot stayed put; the cursor moved instead.
    expect(dotStyle()).toEqual(before)
    expect(
      container.querySelector('.ui-xypad__cursor')!.getAttribute('style'),
    ).toContain('left: 60%')
  })

  it('fast jog ticks inside one echo window accumulate — no lost reel steps', () => {
    const bus = createControlBus()
    renderPanel({ connection: 'open' }, {}, bus)
    addTarget('funk') // 12 o'clock
    addTarget('techno') // 6 o'clock
    act(() => bus.publish({ kind: 'hot_cue_pad', deck: 'a', index: 1 }))
    // The real store's echo lags a fast burst (the coalescer keeps one write
    // per frame): apply each move but hold the snapshot back.
    vi.mocked(styleMoveTarget).mockImplementation((_deck, text, x, y) => {
      pad.targets = pad.targets.map((t) => (t.text === text ? { ...t, x, y } : t))
    })
    act(() =>
      bus.publish({ kind: 'track_seek', deck: 'a', steps: 1, shifted: false }),
    )
    act(() =>
      bus.publish({ kind: 'track_seek', deck: 'a', steps: 1, shifted: false }),
    )

    expect(styleMoveTarget).toHaveBeenCalledTimes(2)
    const [, , , firstY] = vi.mocked(styleMoveTarget).mock.calls[0]
    const [, , , secondY] = vi.mocked(styleMoveTarget).mock.calls[1]
    // Each tick reels techno (6 o'clock) further hubward. Recomputing from
    // the stale projection would send the same position twice — a lost step.
    expect(secondY).toBeLessThan(firstY)
  })

  it('fast SHIFT-jog steer ticks accumulate before the echo lands', () => {
    const bus = createControlBus()
    steerPanel(bus)
    vi.mocked(styleSetCursor).mockImplementation((_deck, x, y) => {
      pad.cursor = { x, y } // echo held back, as above
    })
    act(() =>
      bus.publish({ kind: 'track_seek', deck: 'a', steps: 10, shifted: true }),
    )
    act(() =>
      bus.publish({ kind: 'track_seek', deck: 'a', steps: 10, shifted: true }),
    )
    // 0.5 → 0.6 → 0.7: the second tick builds on the first EMIT, not the
    // stale projection (which would send 0.6 twice).
    expect(vi.mocked(styleSetCursor).mock.calls[1][1]).toBeCloseTo(0.7, 5)
  })

  it('sweeps the cursor around the target circle from the control bus', () => {
    const bus = createControlBus()
    renderPanel({ connection: 'open' }, {}, bus)
    addTarget('funk') // spawns at 12 o'clock — exactly where sweep 0 lands
    addTarget('techno')

    act(() => bus.publish({ kind: 'style_sweep', deck: 'a', value: 0 }))

    // Sweep 0 lands the cursor at 12 o'clock (the blend itself is Rust's).
    expect(styleSetCursor).toHaveBeenCalledWith(
      0,
      expect.closeTo(0.5, 4),
      expect.closeTo(0.12, 2),
    )
  })

  it('ignores style intents addressed to the other deck', () => {
    const bus = createControlBus()
    renderPanel({ connection: 'open' }, {}, bus)
    addTarget('funk')

    act(() => bus.publish({ kind: 'hot_cue_pad', deck: 'b', index: 0 }))
    act(() => bus.publish({ kind: 'style_sweep', deck: 'b', value: 0.5 }))

    expect(styleToggleSelection).not.toHaveBeenCalled()
    expect(styleSetCursor).not.toHaveBeenCalled()
  })

  it('ignores hardware style intents while the deck cannot take them', () => {
    pad.seed([{ text: 'funk', x: 0.5, y: 0.12 }])
    const bus = createControlBus()
    renderPanel({ connection: 'open', switchingModel: true }, {}, bus)

    act(() => bus.publish({ kind: 'hot_cue_pad', deck: 'a', index: 0 }))

    expect(styleToggleSelection).not.toHaveBeenCalled()
  })

  it('samples the other deck onto the pad through the sample-add intent', async () => {
    const onSampleOtherDeck = vi.fn(async () => ({
      label: '⏺ B·1',
      sample: 'sample:b:1',
    }))
    renderPanel(
      { connection: 'open' },
      { onSampleOtherDeck: onSampleOtherDeck as unknown as () => void },
    )
    fireEvent.click(screen.getByRole('button', { name: 'Sample deck B' }))
    expect(
      await screen.findByRole('button', { name: 'Remove ⏺ B·1' }),
    ).toBeInTheDocument()
    // The intent fires AFTER the embed resolved (the control socket's FIFO
    // keeps the shell's blend send behind the embedding it references).
    expect(styleAddSampleTarget).toHaveBeenCalledWith(0, '⏺ B·1', 'sample:b:1')
  })

  it('fires the sample-add intent exactly once under StrictMode', async () => {
    // Guards the updater-purity rule: an intent smuggled into a state
    // updater (or an effect without a gate) double-fires when StrictMode
    // replays it — a second add is a quiet Rust-side dup no-op, but the
    // discipline matters for every intent that is not idempotent.
    const onSampleOtherDeck = vi.fn(async () => ({
      label: '⏺ B·1',
      sample: 'sample:b:1',
    }))
    render(
      <StrictMode>
        <ControlBusProvider bus={createControlBus()}>
          <DeckColumn
            deckId="a"
            state={{
              ...initialDeckState,
              connection: 'open',
              activeStyle: { prompts: [{ text: 'x', weight: 1 }] },
            }}
            onPlay={noop}
            onStop={noop}
            onSetModel={noop as (m: string) => void}
            onRestart={noop}
            fx={{ kind: null, amount: 0 }}
            onSetFx={noop as (k: unknown) => void}
            onSetFxAmount={noop as (v: number) => void}
            loop={emptyLoop()}
            onGenerateToPad={noop as (prompt: string, kind: string) => void}
            generateError={null}
            onLoopPad={noop as (slot: number) => void}
            onClearLoopPad={noop as (slot: number) => void}
            onSetLoopSeconds={noop as (seconds: number) => void}
            bpm={null}
            onSampleOtherDeck={onSampleOtherDeck}
            canSample
            onSavePreset={noop as (preset: object) => void}
            mode="realtime"
            track={null}
            onLeavePlayback={noop}
            onSeekTrack={noop as (s: number) => void}
            onSetTrackRate={noop as (r: number) => void}
            onSyncTrack={() => 'synced' as const}
            onHotCuePad={noop}
            onClearHotCue={noop}
            onLoopIn={noop}
            onLoopOut={noop}
            onLoopExit={noop}
            onBeatLoop={noop}
            onHalveLoop={noop}
            onDoubleLoop={noop}
            getTrackPeaks={() => null}
          />
        </ControlBusProvider>
      </StrictMode>,
    )
    fireEvent.click(screen.getByRole('button', { name: 'Sample deck B' }))
    await screen.findByRole('button', { name: 'Remove ⏺ B·1' })
    expect(styleAddSampleTarget).toHaveBeenCalledTimes(1)
  })

  it('reports an honest reason when the other deck has not played enough', async () => {
    const onSampleOtherDeck = vi.fn(async () => null)
    renderPanel(
      { connection: 'open' },
      { onSampleOtherDeck: onSampleOtherDeck as unknown as () => void },
    )
    fireEvent.click(screen.getByRole('button', { name: 'Sample deck B' }))
    expect(
      await screen.findByText(
        "Sampling failed: the other deck hasn't played enough yet",
      ),
    ).toBeInTheDocument()
  })

  it('disables sampling while the other deck is silent', () => {
    renderPanel(
      { connection: 'open' },
      {},
      createControlBus(),
      { kind: null, amount: 0 },
      emptyLoop(),
      null,
      false,
    )
    expect(screen.getByRole('button', { name: 'Sample deck B' })).toBeDisabled()
  })

  it('shows the reason when sampling fails', async () => {
    const onSampleOtherDeck = vi.fn(async () => {
      throw new Error('deck is loading a model')
    })
    renderPanel(
      { connection: 'open' },
      { onSampleOtherDeck: onSampleOtherDeck as unknown as () => void },
    )
    fireEvent.click(screen.getByRole('button', { name: 'Sample deck B' }))
    expect(
      await screen.findByText('Sampling failed: deck is loading a model'),
    ).toBeInTheDocument()
  })

  // Persistence (text targets + cursor only) and the worker-death chip strip
  // moved into the Rust store with the arrangement itself (phase B) — both
  // are covered by store.rs / style_send.rs tests; here the projection just
  // renders whatever snapshot the store broadcasts.
  it('drops a sampled chip the moment the store broadcasts without it', async () => {
    const onSampleOtherDeck = vi.fn(async () => ({
      label: '⏺ B·1',
      sample: 'sample:b:1',
    }))
    renderPanel(
      { connection: 'open' },
      { onSampleOtherDeck: onSampleOtherDeck as unknown as () => void },
    )
    fireEvent.click(screen.getByRole('button', { name: 'Sample deck B' }))
    await screen.findByRole('button', { name: 'Remove ⏺ B·1' })

    // The shell strips the chip on worker death; the projection follows.
    pad.seed([])
    expect(
      screen.queryByRole('button', { name: 'Remove ⏺ B·1' }),
    ).not.toBeInTheDocument()
  })

  it('saves the pad and FX as a named preset, excluding sampled chips', async () => {
    const onSavePreset = vi.fn()
    const onSampleOtherDeck = vi.fn(async () => ({
      label: '⏺ B·1',
      sample: 'sample:b:1',
    }))
    renderPanel(
      { connection: 'open' },
      {
        onSavePreset: onSavePreset as () => void,
        onSampleOtherDeck: onSampleOtherDeck as unknown as () => void,
      },
      createControlBus(),
      { kind: 'dub_echo', amount: 0.4 },
    )
    addTarget('funk')
    fireEvent.click(screen.getByRole('button', { name: 'Sample deck B' }))
    await screen.findByRole('button', { name: 'Remove ⏺ B·1' })

    fireEvent.change(screen.getByLabelText('Preset name'), {
      target: { value: '  Warm funk  ' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Save preset' }))
    expect(onSavePreset).toHaveBeenCalledWith({
      name: 'Warm funk',
      targets: [{ text: 'funk', x: 0.5, y: expect.any(Number) }],
      cursor: { x: 0.5, y: 0.5 },
      fx: { kind: 'dub_echo', amount: 0.4 },
    })
    // The name clears so the next save starts fresh.
    expect(screen.getByLabelText('Preset name')).toHaveValue('')
  })

  it('refuses to save when only sampled chips are on the pad', async () => {
    const onSampleOtherDeck = vi.fn(async () => ({
      label: '⏺ B·1',
      sample: 'sample:b:1',
    }))
    renderPanel(
      { connection: 'open' },
      { onSampleOtherDeck: onSampleOtherDeck as unknown as () => void },
    )
    fireEvent.click(screen.getByRole('button', { name: 'Sample deck B' }))
    await screen.findByRole('button', { name: 'Remove ⏺ B·1' })
    fireEvent.change(screen.getByLabelText('Preset name'), {
      target: { value: 'Only samples' },
    })
    expect(screen.getByRole('button', { name: 'Save preset' })).toBeDisabled()
  })

  it('applies a loaded preset wholesale through the preset intent', () => {
    const bus = createControlBus()
    renderPanel({ connection: 'open' }, {}, bus)
    addTarget('old target')

    act(() =>
      bus.publish({
        kind: 'preset_load',
        deck: 'a',
        preset: {
          name: 'Warm funk',
          targets: [
            { text: 'warm disco funk', x: 0.2, y: 0.3 },
            { text: 'soul breaks', x: 0.8, y: 0.7 },
          ],
          cursor: { x: 0.2, y: 0.3 },
          fx: { kind: null, amount: 0 },
        },
      }),
    )
    expect(styleApplyPreset).toHaveBeenCalledWith(
      0,
      [
        { text: 'warm disco funk', x: 0.2, y: 0.3 },
        { text: 'soul breaks', x: 0.8, y: 0.7 },
      ],
      { x: 0.2, y: 0.3 },
    )
    // The pad is replaced wholesale (the blend re-send is the shell's).
    expect(
      screen.getByRole('button', { name: 'Remove warm disco funk' }),
    ).toBeInTheDocument()
    expect(
      screen.queryByRole('button', { name: 'Remove old target' }),
    ).not.toBeInTheDocument()
  })

  it('ignores preset loads addressed to the other deck', () => {
    const bus = createControlBus()
    renderPanel({ connection: 'open' }, {}, bus)
    addTarget('mine')
    act(() =>
      bus.publish({
        kind: 'preset_load',
        deck: 'b',
        preset: {
          name: 'X',
          targets: [{ text: 'theirs', x: 0.5, y: 0.5 }],
          cursor: { x: 0.5, y: 0.5 },
          fx: { kind: null, amount: 0 },
        },
      }),
    )
    expect(styleApplyPreset).not.toHaveBeenCalled()
    expect(
      screen.queryByRole('button', { name: 'Remove theirs' }),
    ).not.toBeInTheDocument()
  })

  it('fires a loop pad on click and a clear on shift-click', () => {
    const onLoopPad = vi.fn()
    const onClearLoopPad = vi.fn()
    renderPanel(
      { connection: 'open' },
      {
        onLoopPad: onLoopPad as () => void,
        onClearLoopPad: onClearLoopPad as () => void,
      },
    )
    const slot = screen.getByRole('button', { name: 'Loop slot 2' })

    fireEvent.click(slot)
    expect(onLoopPad).toHaveBeenCalledWith(1)
    fireEvent.click(slot, { shiftKey: true })
    expect(onClearLoopPad).toHaveBeenCalledWith(1)
  })

  it('shows the frozen status while a loop is on air', () => {
    renderPanel(
      { connection: 'open', playing: true },
      {},
      createControlBus(),
      { kind: null, amount: 0 },
      {
        ...emptyLoop(),
        slots: [
          { state: 'filled', label: null, oneShot: false, layer: false },
          { state: 'empty' },
          { state: 'empty' },
          { state: 'empty' },
        ],
        active: 0,
      },
    )
    expect(screen.getByText('Frozen — looping')).toBeInTheDocument()
    expect(
      screen.getByRole('button', { name: 'Loop slot 1' }),
    ).toHaveAttribute('aria-pressed', 'true')
  })

  it('disables the loop slots while the deck cannot take them', () => {
    renderPanel({ connection: 'open', switchingModel: true })
    expect(
      screen.getByRole('button', { name: 'Loop slot 1' }),
    ).toBeDisabled()
  })

  function generateRow(
    handlers: Record<string, () => void> = {},
    loop: LoopState = emptyLoop(),
    generateError: string | null = null,
  ) {
    renderPanel(
      { connection: 'open' },
      handlers,
      createControlBus(),
      { kind: null, amount: 0 },
      loop,
      null,
      true,
      generateError,
    )
  }

  it('generates with the typed prompt, chosen engine, and behaviour', () => {
    const onGenerateToPad = vi.fn()
    generateRow({ onGenerateToPad: onGenerateToPad as () => void })
    expect(screen.queryByRole('radiogroup', { name: 'Generation mode' })).toBeNull()

    fireEvent.change(screen.getByLabelText('Generate prompt'), {
      target: { value: 'vinyl spinback' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Generate' }))
    expect(onGenerateToPad).toHaveBeenCalledWith('vinyl spinback', 'sfx', true, [])

    fireEvent.change(screen.getByLabelText('Engine'), {
      target: { value: 'music' },
    })
    fireEvent.change(screen.getByLabelText('Type'), {
      target: { value: 'loop' },
    })
    fireEvent.keyDown(screen.getByLabelText('Generate prompt'), {
      key: 'Enter',
    })
    expect(onGenerateToPad).toHaveBeenLastCalledWith(
      'vinyl spinback',
      'music',
      false,
      [],
    )
  })

  it('rides the stacked LoRA adapters and their trims into a pad generation (issue #66)', () => {
    useLorasMock.mockReturnValue([
      {
        name: 'small/maqam',
        base: 'small',
        slug: 'maqam',
        sizeBytes: 200_000_000,
        source: null,
        adapterType: 'lora',
        rank: 64,
      },
      {
        name: 'small/crackle',
        base: 'small',
        slug: 'crackle',
        sizeBytes: 50_000_000,
        source: null,
        adapterType: 'lora',
        rank: 8,
      },
      // A medium adapter never reaches the pad rack (pads ride the small DiTs).
      {
        name: 'medium/tape',
        base: 'medium',
        slug: 'tape',
        sizeBytes: 100_000_000,
        source: null,
        adapterType: 'lora',
        rank: 16,
      },
    ])
    const onGenerateToPad = vi.fn()
    generateRow({ onGenerateToPad: onGenerateToPad as () => void })

    fireEvent.click(screen.getByRole('button', { name: 'LoRA: Off' }))
    expect(screen.getByText('Incompatible adapters (1)')).toBeInTheDocument()
    const maqamRow = screen.getByText('maqam').closest('.ui-lora-panel__adapter')
    expect(maqamRow).not.toBeNull()
    fireEvent.click(within(maqamRow as HTMLElement).getByRole('button', { name: 'Apply' }))
    const crackleRow = screen.getByText('crackle').closest('.ui-lora-panel__adapter')
    expect(crackleRow).not.toBeNull()
    fireEvent.click(within(crackleRow as HTMLElement).getByRole('button', { name: 'Apply' }))
    fireEvent.change(screen.getByLabelText('maqam strength'), {
      target: { value: '0.75' },
    })
    fireEvent.change(screen.getByLabelText('Generate prompt'), {
      target: { value: 'oud phrase' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Generate' }))
    expect(onGenerateToPad).toHaveBeenCalledWith('oud phrase', 'sfx', true, [
      { name: 'small/maqam', strength: 0.75 },
      { name: 'small/crackle', strength: 1 },
    ])
  })

  it('offers Magenta while the deck plays — the third engine is its own worker', () => {
    const onGenerateToPad = vi.fn()
    renderPanel(
      { connection: 'open', playing: true },
      { onGenerateToPad: onGenerateToPad as () => void },
    )
    fireEvent.change(screen.getByLabelText('Generate prompt'), {
      target: { value: 'dub chords' },
    })
    fireEvent.change(screen.getByLabelText('Engine'), {
      target: { value: 'magenta' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Generate' }))
    expect(onGenerateToPad).toHaveBeenCalledWith('dub chords', 'magenta', true, [])
  })

  it('caps the prompt input short of the backend limit, sparing the BPM stamp', () => {
    generateRow()
    // The backend ceiling minus room for ", NNN BPM" — so a prompt the input
    // accepted can never bounce off the backend once stamped.
    expect(screen.getByLabelText('Generate prompt')).toHaveAttribute(
      'maxlength',
      String(GENERATE_PROMPT_MAX_LENGTH),
    )
  })

  it('refuses to generate without a prompt or an empty slot', () => {
    const fullSlots: LoopState = {
      ...emptyLoop(),
      slots: Array.from({ length: 4 }, () => ({
        state: 'filled',
        label: null,
        oneShot: false,
        layer: false,
      })),
    }
    generateRow({}, fullSlots)
    fireEvent.change(screen.getByLabelText('Generate prompt'), {
      target: { value: 'riser' },
    })
    expect(screen.getByRole('button', { name: 'Generate' })).toBeDisabled()
  })

  it('shows a pending slot as busy and unpressable', () => {
    const pending: LoopState = {
      ...emptyLoop(),
      slots: [
        { state: 'pending', label: 'air horn', oneShot: true },
        { state: 'empty' },
        { state: 'empty' },
        { state: 'empty' },
      ],
    }
    generateRow({}, pending)
    const pad = screen.getByRole('button', {
      name: 'Loop slot 1 — generating',
    })
    expect(pad).toBeDisabled()
    expect(pad).toHaveTextContent('…')
  })

  it('labels a generated slot with its prompt', () => {
    const generated: LoopState = {
      ...emptyLoop(),
      slots: [
        { state: 'filled', label: 'air horn', oneShot: true, layer: false },
        { state: 'empty' },
        { state: 'empty' },
        { state: 'empty' },
      ],
    }
    generateRow({}, generated)
    expect(screen.getByRole('button', { name: 'Loop slot 1' })).toHaveAttribute(
      'title',
      'air horn',
    )
  })

  it('surfaces the generation failure detail', () => {
    generateRow({}, emptyLoop(), 'sa3_mlx checkout not found')
    expect(screen.getByRole('alert')).toHaveTextContent(
      'Generation failed: sa3_mlx checkout not found',
    )
  })

  it('shows the gated BPM, and an honest dash without one', () => {
    renderPanel(
      { connection: 'open', playing: true },
      {},
      createControlBus(),
      { kind: null, amount: 0 },
      emptyLoop(),
      131.9,
    )
    const stat = screen.getByText('BPM').parentElement!
    expect(stat).toHaveTextContent('131.9')

    renderPanel({ connection: 'open', playing: true })
    expect(screen.getAllByText('BPM').at(-1)!.parentElement).toHaveTextContent('—')
  })

  it('changes the loop capture length', () => {
    const onSetLoopSeconds = vi.fn()
    renderPanel(
      { connection: 'open' },
      { onSetLoopSeconds: onSetLoopSeconds as () => void },
    )
    fireEvent.change(screen.getByLabelText('Loop length'), {
      target: { value: '8' },
    })
    expect(onSetLoopSeconds).toHaveBeenCalledWith(8)
  })
})

describe('DeckColumn playback mode (M19)', () => {
  const aTrack = (overrides: Partial<TrackState> = {}): TrackState => ({
    loadId: 1,
    title: 'Warehouse Anthem',
    duration: 125,
    position: 65.4,
    playing: false,
    ended: false,
    bpm: 132.5,
    grid: null,
    rate: 1,
    cues: Array<number | null>(8).fill(null),
    loop: null,
    pendingLoopIn: null,
    ...overrides,
  })

  function renderPlayback(
    track: TrackState,
    handlers: Record<string, () => void> = {},
  ) {
    return renderPanel(
      { connection: 'open' },
      handlers,
      createControlBus(),
      { kind: null, amount: 0 },
      emptyLoop(),
      null,
      true,
      null,
      { mode: 'playback', track },
    )
  }

  it('swaps the style pane for the track overview, title, and status', () => {
    renderPlayback(aTrack())
    expect(screen.queryByLabelText('Style pad')).toBeNull()
    expect(screen.queryByLabelText('Model')).toBeNull()
    expect(
      screen.getByRole('slider', { name: 'Track overview a' }),
    ).toBeInTheDocument()
    expect(screen.getByText('Warehouse Anthem')).toBeInTheDocument()
    expect(screen.getByText('Track — paused')).toBeInTheDocument()
  })

  it('reads the transport from the track, not the worker', () => {
    const onStop = vi.fn()
    renderPlayback(aTrack({ playing: true }), { onStop })
    // state.playing is false — the lit STOP belongs to the track.
    fireEvent.click(screen.getByRole('button', { name: 'Stop' }))
    expect(onStop).toHaveBeenCalled()
    expect(screen.getByText('Track — playing')).toBeInTheDocument()
  })

  it('shows the track clock instead of the stream plumbing', () => {
    renderPlayback(aTrack())
    const position = screen.getByText('Position').parentElement!
    expect(position).toHaveTextContent('1:05 / 2:05')
    const stat = screen.getByText('BPM').parentElement!
    expect(stat).toHaveTextContent('132.5')
    expect(screen.queryByText('Buffer')).toBeNull()
    expect(screen.queryByText('Underruns')).toBeNull()
  })

  it('seeks from the overview with the keyboard', () => {
    const onSeekTrack = vi.fn()
    renderPlayback(aTrack(), {
      onSeekTrack: onSeekTrack as unknown as () => void,
    })
    const slider = screen.getByRole('slider', { name: 'Track overview a' })
    fireEvent.keyDown(slider, { key: 'ArrowRight' })
    expect(onSeekTrack).toHaveBeenCalledWith(70.4)
    fireEvent.keyDown(slider, { key: 'Home' })
    expect(onSeekTrack).toHaveBeenCalledWith(0)
    fireEvent.keyDown(slider, { key: 'End' })
    expect(onSeekTrack).toHaveBeenCalledWith(125)
  })

  it('announces the explicit end-of-track state', () => {
    renderPlayback(aTrack({ position: 125, ended: true }))
    expect(screen.getByText('Track — ended')).toBeInTheDocument()
  })

  it('rides the tempo knob and shows the effective BPM (M20)', () => {
    const onSetTrackRate = vi.fn()
    renderPlayback(aTrack({ rate: 1.05 }), {
      onSetTrackRate: onSetTrackRate as unknown as () => void,
    })
    // The readout is bpm × rate — SYNC must visibly do something.
    const stat = screen.getByText('BPM').parentElement!
    expect(stat).toHaveTextContent((132.5 * 1.05).toFixed(1))
    fireEvent.change(screen.getByLabelText('Tempo'), {
      target: { value: '1.02' },
    })
    expect(onSetTrackRate).toHaveBeenCalledWith(1.02)
  })

  it('SYNC names its refusal reason', () => {
    const onSyncTrack = vi.fn(() => 'out_of_range' as const)
    renderPlayback(aTrack(), {
      onSyncTrack: onSyncTrack as unknown as () => void,
    })
    fireEvent.click(screen.getByRole('button', { name: 'Sync' }))
    expect(onSyncTrack).toHaveBeenCalled()
    expect(
      screen.getByText('Sync refused — tempo out of range'),
    ).toBeInTheDocument()

    // A missing target is its own message — never the wrong blame.
    onSyncTrack.mockReturnValue('no_tempo' as never)
    fireEvent.click(screen.getByRole('button', { name: 'Sync' }))
    expect(
      screen.getByText('Sync refused — no tempo to sync to'),
    ).toBeInTheDocument()
  })

  it('carries its own exit back to the live stream', () => {
    // Without crates the Media Explorer's live row is the only other
    // way out — the deck must not depend on it.
    const onLeavePlayback = vi.fn()
    renderPlayback(aTrack(), { onLeavePlayback })
    fireEvent.click(screen.getByRole('button', { name: 'Back to live' }))
    expect(onLeavePlayback).toHaveBeenCalled()
  })

  it('hot cue pads set on click and clear on SHIFT+click (M21)', () => {
    const onHotCuePad = vi.fn()
    const onClearHotCue = vi.fn()
    const cues = Array<number | null>(8).fill(null)
    cues[1] = 32.25
    renderPlayback(aTrack({ cues }), {
      onHotCuePad: onHotCuePad as unknown as () => void,
      onClearHotCue: onClearHotCue as unknown as () => void,
    })
    fireEvent.click(screen.getByRole('button', { name: 'Hot cue 3' }))
    expect(onHotCuePad).toHaveBeenCalledWith(2)
    fireEvent.click(screen.getByRole('button', { name: 'Hot cue 2' }), {
      shiftKey: true,
    })
    expect(onClearHotCue).toHaveBeenCalledWith(1)
    // The filled pad is the lit one — LEDs and screen agree.
    expect(
      screen.getByRole('button', { name: 'Hot cue 2' }).className,
    ).toContain('--lit')
    expect(
      screen.getByRole('button', { name: 'Hot cue 3' }).className,
    ).not.toContain('--lit')
  })

  it('loop controls arm IN and gate OUT/EXIT on having work (M21)', () => {
    const onLoopIn = vi.fn()
    renderPlayback(aTrack(), { onLoopIn })
    // No IN armed: OUT cannot close a region; no loop: nothing to exit.
    expect(screen.getByRole('button', { name: 'Loop out' })).toBeDisabled()
    expect(screen.getByRole('button', { name: 'Exit loop' })).toBeDisabled()
    fireEvent.click(screen.getByRole('button', { name: 'Loop in' }))
    expect(onLoopIn).toHaveBeenCalled()
  })

  it('an active loop enables EXIT and names its whole-beat length (M21)', () => {
    const onLoopExit = vi.fn()
    renderPlayback(
      aTrack({
        grid: { bpm: 120, firstBeatSeconds: 0 },
        loop: { start: 64, end: 66 },
      }),
      { onLoopExit },
    )
    // 2 s at 120 BPM: four beats, said outright.
    expect(screen.getByText('4-beat loop')).toBeInTheDocument()
    const exit = screen.getByRole('button', { name: 'Exit loop' })
    expect(exit).toBeEnabled()
    fireEvent.click(exit)
    expect(onLoopExit).toHaveBeenCalled()
  })

  it('claims no beat length for a gridless loop — the honesty rule', () => {
    renderPlayback(aTrack({ loop: { start: 64, end: 66 } }))
    expect(screen.queryByText(/-beat loop/)).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Exit loop' })).toBeEnabled()
  })

  it('claims no beat length for a tail-clamped fractional loop either', () => {
    // quantisedLoop clamps the end into the track: 0.1s at 120 BPM is
    // not "0 beats" — a count the region doesn't have stays unsaid.
    renderPlayback(
      aTrack({
        grid: { bpm: 120, firstBeatSeconds: 0 },
        loop: { start: 124.8, end: 124.9 },
      }),
    )
    expect(screen.queryByText(/-beat loop/)).not.toBeInTheDocument()
  })

  it('the 4-beat button sets a beat loop (M23)', () => {
    const onBeatLoop = vi.fn()
    renderPlayback(aTrack({ grid: { bpm: 120, firstBeatSeconds: 0 } }), {
      onBeatLoop: onBeatLoop as unknown as () => void,
    })
    fireEvent.click(screen.getByRole('button', { name: '4 beats' }))
    expect(onBeatLoop).toHaveBeenCalledWith(4)
  })

  it('the 4-beat button is inert without a grid — disabled, not a guess (M23)', () => {
    renderPlayback(aTrack({ grid: null }))
    expect(screen.getByRole('button', { name: '4 beats' })).toBeDisabled()
  })

  it('halve and double are gated on an active loop (M23)', () => {
    renderPlayback(aTrack({ loop: null }))
    expect(screen.getByRole('button', { name: '½×' })).toBeDisabled()
    expect(screen.getByRole('button', { name: '2×' })).toBeDisabled()
  })

  it('halve and double drive an active loop (M23)', () => {
    const onHalveLoop = vi.fn()
    const onDoubleLoop = vi.fn()
    renderPlayback(
      aTrack({
        grid: { bpm: 120, firstBeatSeconds: 0 },
        loop: { start: 8, end: 10 },
      }),
      { onHalveLoop, onDoubleLoop },
    )
    fireEvent.click(screen.getByRole('button', { name: '½×' }))
    fireEvent.click(screen.getByRole('button', { name: '2×' }))
    expect(onHalveLoop).toHaveBeenCalled()
    expect(onDoubleLoop).toHaveBeenCalled()
  })

  it('names a sub-beat loop by its fraction (M23)', () => {
    // Half a beat at 120 BPM — a count a whole-beat label can't carry.
    renderPlayback(
      aTrack({
        grid: { bpm: 120, firstBeatSeconds: 0 },
        loop: { start: 8, end: 8.25 },
      }),
    )
    expect(screen.getByText('½-beat loop')).toBeInTheDocument()
  })
})
