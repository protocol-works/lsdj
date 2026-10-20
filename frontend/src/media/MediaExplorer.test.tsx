import { act, fireEvent, render, screen, within } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { togglePianoWindow } from '../audio/nativeEngine'
import { useInterfaceStore } from '../audio/interfaceStore'
import type { DeckId, TrackSource } from '../audio/types'
import { createControlBus, type ControlBus } from '../control/bus'
import { ControlBusProvider } from '../control/ControlBusProvider'
import type { StylePreset } from '../presets'
import { MediaExplorer } from './MediaExplorer'
import { MEDIA_DEFAULT_HEIGHT } from './mediaTray'

let scrollIntoView: ReturnType<typeof vi.fn>
let scrolledRows: HTMLElement[]

// Real nativeEngine except the piano-window toggle, which we spy on; and a
// controllable interface-store hook for the window's open (lit) state.
vi.mock('../audio/nativeEngine', async (importOriginal) => {
  const original = await importOriginal<typeof import('../audio/nativeEngine')>()
  return { ...original, togglePianoWindow: vi.fn() }
})
vi.mock('../audio/interfaceStore', () => ({ useInterfaceStore: vi.fn(() => null) }))

// The installed-adapter list (issue #66) is stubbed per test; the default is
// none, which hides the LoRA pickers entirely.
const useLorasMock = vi.fn<() => import('../audio/nativeEngine').LoraAdapter[]>(() => [])
vi.mock('../models/useLoras', async (importOriginal) => {
  const original = await importOriginal<typeof import('../models/useLoras')>()
  return { ...original, useLoras: () => useLorasMock() }
})

type Handlers = {
  onLoadPreset?: (deck: DeckId, preset: StylePreset) => void
  onLoadTrack?: (deck: DeckId, source: TrackSource, title: string) => Promise<boolean>
  onLoadSample?: (
    deck: DeckId,
    wav: ArrayBuffer,
    oneShot: boolean,
    label: string,
  ) => Promise<boolean>
  onPreview?: (wav: ArrayBuffer) => Promise<void>
  onStopPreview?: () => void
  onToggle?: () => void
  onResize?: (height: number, commit: boolean) => void
}

function renderExplorer(
  handlers: Handlers = {},
  presets: StylePreset[] = [],
  bus: ControlBus = createControlBus(),
  open = true,
) {
  render(
    <ControlBusProvider bus={bus}>
      <MediaExplorer
        presets={presets}
        onLoadPreset={handlers.onLoadPreset ?? vi.fn()}
        onDeletePreset={vi.fn()}
        onImportPresets={vi.fn()}
        onLoadTrack={handlers.onLoadTrack ?? vi.fn(async () => true)}
        onLoadSample={handlers.onLoadSample ?? vi.fn(async () => true)}
        onPreview={handlers.onPreview ?? vi.fn(async () => {})}
        onStopPreview={handlers.onStopPreview ?? vi.fn()}
        open={open}
        onToggle={handlers.onToggle ?? vi.fn()}
        height={MEDIA_DEFAULT_HEIGHT}
        onResize={handlers.onResize ?? vi.fn()}
      />
    </ControlBusProvider>,
  )
}

function stubFetch(response: Partial<Response> = {}) {
  const fetchMock = vi.fn(async () => ({
    ok: true,
    arrayBuffer: async () => new ArrayBuffer(4),
    json: async () => ({}),
    ...response,
  }))
  vi.stubGlobal('fetch', fetchMock)
  return fetchMock
}

beforeEach(() => {
  scrolledRows = []
  scrollIntoView = vi.fn(function (this: HTMLElement) {
    scrolledRows.push(this)
  })
  Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', {
    configurable: true,
    value: scrollIntoView,
  })
  vi.stubGlobal('crypto', {
    getRandomValues: (target: Uint32Array) => {
      target[0] = 17
      return target
    },
  })
})

// Sets the Title field too (to the same string) so the take's name and #id label are
// deterministic rather than a random title — most assertions key off the label.
async function composeTrack(name: string) {
  fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
  fireEvent.change(screen.getByLabelText('Title'), { target: { value: name } })
  fireEvent.change(screen.getByLabelText('Track prompt'), { target: { value: name } })
  await act(async () => {
    fireEvent.click(screen.getByRole('button', { name: 'Compose' }))
  })
}

/** Compose a clip in the Samples tab. One-shot by default so the requested length is
 * exact (a loop adds the seam surplus the engine folds on reload). */
async function composeSampleClip(name: string, oneShot = true) {
  fireEvent.click(screen.getByRole('tab', { name: 'Samples' }))
  if (oneShot) {
    fireEvent.click(screen.getByRole('button', { name: 'Toggle loop or one-shot' }))
  }
  fireEvent.change(screen.getByLabelText('Title'), { target: { value: name } })
  fireEvent.change(screen.getByLabelText('Loop prompt'), { target: { value: name } })
  await act(async () => {
    fireEvent.click(screen.getByRole('button', { name: 'Compose' }))
  })
}

/** Open a row's ⋯ menu and pick an item. */
async function chooseMenuItem(name: string, item: string) {
  fireEvent.click(await screen.findByRole('button', { name: `More actions for ${name}` }))
  fireEvent.click(screen.getByRole('menuitem', { name: item }))
}

afterEach(() => {
  Reflect.deleteProperty(HTMLElement.prototype, 'scrollIntoView')
  vi.unstubAllGlobals()
  localStorage.clear()
  vi.mocked(useInterfaceStore).mockReturnValue(null)
  vi.mocked(togglePianoWindow).mockClear()
  useLorasMock.mockReturnValue([])
})

describe('MediaExplorer', () => {
  it('opens on the folded-in crates tab', () => {
    renderExplorer()
    expect(
      screen.getByText("No presets yet — save a deck's style below its pad"),
    ).toBeInTheDocument()
  })

  it('toggles the tray via the header chevron', () => {
    const onToggle = vi.fn()
    renderExplorer({ onToggle })
    // Open: the chevron offers to collapse (with a shortcut hint), and the
    // resize grip is present.
    const toggle = screen.getByRole('button', { name: 'Collapse media explorer' })
    expect(toggle).toHaveAttribute('aria-expanded', 'true')
    expect(toggle.getAttribute('title')).toContain('Collapse media explorer')
    expect(screen.getByRole('separator', { name: 'Resize media explorer' })).toBeInTheDocument()
    fireEvent.click(toggle)
    expect(onToggle).toHaveBeenCalledTimes(1)
  })

  it('collapses to a slim bar that toggles when clicked anywhere', () => {
    const onToggle = vi.fn()
    renderExplorer({ onToggle }, [], createControlBus(), false)
    // Closed: the whole bar is the expand button (no separate chevron control),
    // it carries a shortcut-hint tooltip, the tabs are gone, the content is
    // hidden from a11y, and there is no resize grip to drag.
    const bar = screen.getByRole('button', { name: 'Expand media explorer' })
    expect(bar).toHaveAttribute('aria-expanded', 'false')
    expect(bar.getAttribute('title')).toContain('Expand media explorer')
    expect(screen.queryByRole('tab')).toBeNull()
    expect(screen.queryByRole('separator')).toBeNull()
    expect(
      screen.getByText("No presets yet — save a deck's style below its pad").closest(
        '.media__content',
      ),
    ).toHaveAttribute('aria-hidden', 'true')
    // Clicking the bar (here, its title label) expands it — not just a chevron.
    fireEvent.click(screen.getByText('Media explorer'))
    expect(onToggle).toHaveBeenCalledTimes(1)
  })

  it('toggles the MIDI keyboard window from the tray button', () => {
    renderExplorer()
    const piano = screen.getByRole('button', { name: 'Open MIDI keyboard' })
    expect(piano).toHaveAttribute('aria-pressed', 'false')
    fireEvent.click(piano)
    expect(togglePianoWindow).toHaveBeenCalledTimes(1)
  })

  it('lights the tray button and flips its label while the window is open', () => {
    vi.mocked(useInterfaceStore).mockReturnValue({
      pianoWindowOpen: true,
    } as unknown as ReturnType<typeof useInterfaceStore>)
    renderExplorer()
    const lit = screen.getByRole('button', { name: 'Close MIDI keyboard' })
    expect(lit).toHaveAttribute('aria-pressed', 'true')
    expect(lit.className).toContain('ui-button--lit')
  })

  it('hides the piano toggle while the tray is collapsed', () => {
    renderExplorer({}, [], createControlBus(), false)
    expect(screen.queryByRole('button', { name: 'Open MIDI keyboard' })).toBeNull()
  })

  it('composes an SA3 track and loads it onto a deck', async () => {
    const fetchMock = stubFetch()
    const onLoadTrack = vi.fn(async () => true)
    renderExplorer({ onLoadTrack })

    await composeTrack('late night dub techno')
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/generate',
      expect.objectContaining({
        body: JSON.stringify({
          prompt: 'late night dub techno',
          seconds: 120,
          kind: 'track',
          seed: 17,
        }),
      }),
    )
    fireEvent.click(
      screen.getByRole('button', {
        name: 'Load late night dub techno #1 to deck B',
      }),
    )
    await act(async () => {})
    // The short id rides along to the deck, so two takes of the same
    // prompt stay tellable apart.
    // An in-memory take (not yet on disk) ships its WAV container bytes once
    // (ADR-0030); a persisted take would load by library reference instead.
    expect(onLoadTrack).toHaveBeenCalledWith(
      'b',
      { kind: 'bytes', wav: expect.any(ArrayBuffer) },
      'late night dub techno #1',
    )
    // The row names the model that produced the take, then its recipe length (the
    // engine label also lives in the engine dropdown, hence the class filter).
    expect(
      screen
        .getAllByText('Track (SA3 medium) · 2:00')
        .some((element) => element.classList.contains('media__meta')),
    ).toBe(true)
  })

  it('persists Basic/Advanced mode and restores the hidden Advanced draft', () => {
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    const mode = screen.getByRole('radiogroup', { name: 'Generation mode' })
    expect(mode.closest('.media__generation-options')).toContainElement(
      document.querySelector('.ui-lora-control'),
    )
    fireEvent.click(screen.getByRole('radio', { name: 'Advanced' }))
    fireEvent.click(screen.getByRole('switch', { name: 'Guidance: Off' }))
    fireEvent.change(screen.getByLabelText('Avoid concepts'), {
      target: { value: 'vocals' },
    })
    expect(JSON.parse(localStorage.getItem('lsdj:v1') ?? '{}').app.generationMode).toBe(
      'advanced',
    )

    fireEvent.click(screen.getByRole('radio', { name: 'Basic' }))
    expect(screen.queryByLabelText('Avoid concepts')).toBeNull()
    expect(screen.queryByText(/Advanced steering is paused/)).toBeNull()
    fireEvent.click(screen.getByRole('radio', { name: 'Advanced' }))
    expect(screen.getByLabelText('Avoid concepts')).toHaveValue('vocals')
  })

  it('sends complete Advanced SA3 text steering with a fixed seed', async () => {
    const fetchMock = stubFetch()
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    fireEvent.click(screen.getByRole('radio', { name: 'Advanced' }))
    fireEvent.click(screen.getByRole('switch', { name: 'Guidance: Off' }))
    fireEvent.click(screen.getByRole('button', { name: 'No drums' }))
    fireEvent.change(screen.getByLabelText(/Classifier-Free Guidance.*3.0/), {
      target: { value: '4.0' },
    })
    fireEvent.change(screen.getByLabelText(/Adaptive Projected Guidance.*1.0/), {
      target: { value: '0.6' },
    })
    fireEvent.change(screen.getByLabelText('Seed behavior'), {
      target: { value: 'fixed' },
    })
    fireEvent.change(screen.getByLabelText('Seed (0–2147483647)'), {
      target: { value: '42' },
    })

    await composeTrack('dry dub')

    expect(fetchMock).toHaveBeenCalledWith(
      '/api/generate',
      expect.objectContaining({
        body: JSON.stringify({
          prompt: 'dry dub',
          seconds: 120,
          kind: 'track',
          negative_prompt: 'drums',
          cfg: 4,
          apg: 0.6,
          seed: 42,
        }),
      }),
    )
  })

  it('mints and sends a fresh random seed for every Advanced take', async () => {
    let seed = 10
    vi.stubGlobal('crypto', {
      getRandomValues: (target: Uint32Array) => {
        target[0] = seed
        seed += 1
        return target
      },
    })
    const fetchMock = stubFetch()
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    fireEvent.click(screen.getByRole('radio', { name: 'Advanced' }))

    await composeTrack('take one')
    await composeTrack('take two')

    const calls = fetchMock.mock.calls as unknown as [string, RequestInit][]
    const bodies = calls.map(([, init]) =>
      JSON.parse(init.body as string),
    )
    expect(bodies.map((body) => body.seed)).toEqual([10, 11])
  })

  it('keeps generation mode out of the Samples tab', () => {
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Samples' }))
    expect(screen.queryByRole('radiogroup', { name: 'Generation mode' })).toBeNull()
    expect(screen.queryByLabelText('Avoid concepts')).toBeNull()
  })

  it('rides a stacked pair of LoRA adapters with their trims on a track compose (issue #66)', async () => {
    useLorasMock.mockReturnValue([
      {
        name: 'medium/maqam',
        base: 'medium',
        slug: 'maqam',
        sizeBytes: 200_000_000,
        source: null,
        adapterType: 'lora',
        rank: 64,
      },
      {
        name: 'medium/breaks',
        base: 'medium',
        slug: 'breaks',
        sizeBytes: 150_000_000,
        source: null,
        adapterType: 'lora',
        rank: 32,
      },
      // Small adapters never reach the track rack (tracks ride medium).
      {
        name: 'small/crackle',
        base: 'small',
        slug: 'crackle',
        sizeBytes: 50_000_000,
        source: null,
        adapterType: 'lora',
        rank: 8,
      },
    ])
    const fetchMock = stubFetch()
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    fireEvent.click(screen.getByRole('button', { name: 'LoRA: Off' }))
    expect(screen.getByText('Incompatible adapters (1)')).toBeInTheDocument()
    // Apply both medium adapters from the contextual panel; trim only maqam.
    const maqamRow = screen.getByText('maqam').closest('.ui-lora-panel__adapter')
    expect(maqamRow).not.toBeNull()
    fireEvent.click(within(maqamRow as HTMLElement).getByRole('button', { name: 'Apply' }))
    const breaksRow = screen.getByText('breaks').closest('.ui-lora-panel__adapter')
    expect(breaksRow).not.toBeNull()
    fireEvent.click(within(breaksRow as HTMLElement).getByRole('button', { name: 'Apply' }))
    fireEvent.change(screen.getByLabelText('maqam strength'), {
      target: { value: '1.5' },
    })
    await composeTrack('maqam study')
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/generate',
      expect.objectContaining({
        body: JSON.stringify({
          prompt: 'maqam study',
          seconds: 120,
          kind: 'track',
          loras: [
            { name: 'medium/maqam', strength: 1.5 },
            { name: 'medium/breaks', strength: 1 },
          ],
          seed: 17,
        }),
      }),
    )
  })

  it('drops a chip toggled back out of the stack from the request', async () => {
    useLorasMock.mockReturnValue([
      {
        name: 'medium/maqam',
        base: 'medium',
        slug: 'maqam',
        sizeBytes: 200_000_000,
        source: null,
        adapterType: 'lora',
        rank: 64,
      },
    ])
    const fetchMock = stubFetch()
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    fireEvent.click(screen.getByRole('button', { name: 'LoRA: Off' }))
    const availableRow = screen.getByText('maqam').closest('.ui-lora-panel__adapter')
    expect(availableRow).not.toBeNull()
    fireEvent.click(within(availableRow as HTMLElement).getByRole('button', { name: 'Apply' }))
    const appliedRow = screen.getByText('maqam').closest('.ui-lora-panel__adapter')
    expect(appliedRow).not.toBeNull()
    fireEvent.click(within(appliedRow as HTMLElement).getByRole('button', { name: 'Remove' }))
    await composeTrack('clean take')
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/generate',
      expect.objectContaining({
        body: JSON.stringify({
          prompt: 'clean take',
          seconds: 120,
          kind: 'track',
          seed: 17,
        }),
      }),
    )
  })

  it('previews a take in the headphones and toggles it off', async () => {
    stubFetch()
    const onPreview = vi.fn(async () => {})
    const onStopPreview = vi.fn()
    renderExplorer({ onPreview, onStopPreview })
    await composeTrack('dub')

    fireEvent.click(
      screen.getByRole('button', { name: 'Preview dub #1 in headphones' }),
    )
    await act(async () => {})
    expect(onPreview).toHaveBeenCalledWith(expect.any(ArrayBuffer))
    // The button flips to a stop affordance; a second press stops the preview.
    fireEvent.click(screen.getByRole('button', { name: 'Stop preview' }))
    expect(onStopPreview).toHaveBeenCalled()
  })

  it('routes Magenta tracks to the render engine within its cap', async () => {
    const fetchMock = stubFetch()
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    // A length past Magenta's cap must snap back into range when the
    // engine switches (the render worker caps at 3 minutes).
    fireEvent.change(screen.getByLabelText('Length'), {
      target: { value: '380' },
    })
    fireEvent.change(screen.getByLabelText('Engine'), {
      target: { value: 'magenta' },
    })
    fireEvent.change(screen.getByLabelText('Track prompt'), {
      target: { value: 'air horn symphony' },
    })
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Compose' }))
    })
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/render',
      expect.objectContaining({
        body: JSON.stringify({ prompt: 'air horn symphony', seconds: 60 }),
      }),
    )
  })

  it('hides SA3 mode and steering for Magenta while preserving the draft', async () => {
    const fetchMock = stubFetch()
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    fireEvent.click(screen.getByRole('radio', { name: 'Advanced' }))
    fireEvent.click(screen.getByRole('switch', { name: 'Guidance: Off' }))
    fireEvent.click(screen.getByRole('button', { name: 'No vocals' }))
    fireEvent.change(screen.getByLabelText('Engine'), { target: { value: 'magenta' } })
    expect(screen.queryByRole('radiogroup', { name: 'Generation mode' })).toBeNull()
    expect(screen.queryByLabelText('Avoid concepts')).toBeNull()
    fireEvent.change(screen.getByLabelText('Track prompt'), {
      target: { value: 'glass piano' },
    })
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Compose' }))
    })

    expect(fetchMock).toHaveBeenCalledWith(
      '/api/render',
      expect.objectContaining({
        body: JSON.stringify({ prompt: 'glass piano', seconds: 120 }),
      }),
    )

    fireEvent.change(screen.getByLabelText('Engine'), { target: { value: 'track' } })
    expect(screen.getByRole('radio', { name: 'Advanced' })).toBeChecked()
    expect(screen.getByLabelText('Avoid concepts')).toHaveValue('vocals')
  })

  it('surfaces the backend detail and drops the pending row on failure', async () => {
    stubFetch({
      ok: false,
      status: 502,
      json: async () => ({ detail: 'render timed out' }),
    } as Partial<Response>)
    renderExplorer()
    await composeTrack('doomed')
    expect(
      screen.getByText('Track generation failed: render timed out'),
    ).toBeInTheDocument()
    expect(screen.queryByText('doomed — composing…')).toBeNull()
  })

  it('loads the rotary-highlighted track on a hardware LOAD', async () => {
    stubFetch()
    const onLoadTrack = vi.fn(async () => true)
    const bus = createControlBus()
    renderExplorer({ onLoadTrack }, [], bus)
    await composeTrack('first')
    await composeTrack('second')

    // Newest sits at the top, so the rotary starts on 'second #2'; one step
    // down lands on the older 'first #1'.
    act(() => bus.publish({ kind: 'browse_scroll', steps: 1 }))
    await act(async () => {
      bus.publish({ kind: 'browse_load', deck: 'a' })
    })
    expect(onLoadTrack).toHaveBeenCalledWith(
      'a',
      { kind: 'bytes', wav: expect.any(ArrayBuffer) },
      'first #1',
    )
  })

  it('keeps the rotary-highlighted track inside the visible viewport', async () => {
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === 'list_generated_songs') {
        return Array.from({ length: 12 }, (_, index) => ({
          file: `track-${index + 1}.wav`,
          title: `Track ${index + 1}`,
          prompt: null,
          model: null,
        }))
      }
      return []
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    const bus = createControlBus()
    renderExplorer({}, [], bus)
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    await screen.findByText('Track 12')
    scrollIntoView.mockClear()
    scrolledRows = []

    act(() => bus.publish({ kind: 'browse_scroll', steps: 9 }))

    expect(scrollIntoView).toHaveBeenCalledWith({ block: 'nearest' })
    expect(scrolledRows.at(-1)).toHaveTextContent('Track 3')
  })

  it('composes a short loop in the Samples tab with the small SFX model', async () => {
    // The small SFX/Music models compose into the samples library now (ADR-0022),
    // not the Generate tab; their shorter length menu lives on the Samples tab.
    const fetchMock = stubFetch()
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Samples' }))
    // One-shot so the requested length is exact (a loop adds the seam surplus).
    fireEvent.click(screen.getByRole('button', { name: 'Toggle loop or one-shot' }))
    fireEvent.change(screen.getByLabelText('Loop prompt'), {
      target: { value: 'vinyl spinback' },
    })
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Compose' }))
    })
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/generate',
      expect.objectContaining({
        body: JSON.stringify({ prompt: 'vinyl spinback', seconds: 10, kind: 'sfx' }),
      }),
    )
    // The row's meta shows the small-model engine (combined with the play mode).
    expect(
      screen
        .getAllByText('SFX (SA3 small)', { exact: false })
        .some((element) => element.classList.contains('media__meta')),
    ).toBe(true)
  })

  it('auto-saves a composed sample to the samples folder, carrying oneShot', async () => {
    stubFetch()
    const calls: { cmd: string; args: unknown }[] = []
    const invoke = vi.fn(async (cmd: string, args?: unknown) => {
      calls.push({ cmd, args })
      if (cmd === 'list_generated_samples') return []
      if (cmd === 'save_generated_sample') {
        return { file: 'riff.wav', title: 'riff', prompt: 'riff', model: 'sfx', oneShot: true }
      }
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    renderExplorer()
    await composeSampleClip('riff')
    const saveCall = calls.find((c) => c.cmd === 'save_generated_sample')
    expect(saveCall).toBeDefined()
    // The same binary frame as a song save: [u32 LE meta-JSON length][meta JSON][WAV].
    const payload = saveCall!.args as Uint8Array
    const metaLen = new DataView(
      payload.buffer,
      payload.byteOffset,
      payload.byteLength,
    ).getUint32(0, true)
    const meta = JSON.parse(new TextDecoder().decode(payload.subarray(4, 4 + metaLen)))
    expect(meta).toEqual({ title: 'riff', prompt: 'riff', model: 'sfx', oneShot: true })
  })

  it('loads a restored sample into a deck loop slot via onLoadSample', async () => {
    const wav = new ArrayBuffer(8)
    const calls: { cmd: string; args: unknown }[] = []
    const invoke = vi.fn(async (cmd: string, args?: unknown) => {
      calls.push({ cmd, args })
      if (cmd === 'list_generated_samples') {
        return [
          { file: 'break.wav', title: 'break', prompt: 'break', model: 'music', oneShot: false },
        ]
      }
      if (cmd === 'read_generated_sample') return wav
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    const onLoadSample = vi.fn(async () => true)
    renderExplorer({ onLoadSample })
    fireEvent.click(screen.getByRole('tab', { name: 'Samples' }))
    const loadButton = await screen.findByRole('button', {
      name: 'Load break #1 to deck A',
    })
    await act(async () => {
      fireEvent.click(loadButton)
    })
    // A restored sample carries no in-memory bytes, so the scoped read fetches them,
    // and the sample's oneShot flag rides along to the slot loader.
    const readCall = calls.find((c) => c.cmd === 'read_generated_sample')
    expect(readCall?.args).toEqual({ name: 'break.wav' })
    expect(onLoadSample).toHaveBeenCalledWith('a', expect.any(ArrayBuffer), false, 'break #1')
  })

  it('live-reloads the Samples tab on the folder-watcher event, keeping ids stable', async () => {
    // The folder watcher fires `library://changed` when a deck saves out-of-band or a
    // file is dropped in; the tab re-lists, reusing existing rows by filename.
    let rows = [
      { file: 'one.wav', title: 'one', prompt: 'one', model: 'sfx', oneShot: false },
    ]
    let onChange: ((e: { payload: unknown }) => void) | null = null
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === 'list_generated_samples') return rows
      return undefined
    })
    const listen = vi.fn(
      async (event: string, handler: (e: { payload: unknown }) => void) => {
        if (event === 'library://changed') onChange = handler
        return () => {}
      },
    )
    vi.stubGlobal('__TAURI__', { core: { invoke }, event: { listen } })
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Samples' }))
    // Scope to the name cell: a take whose title equals its prompt now shows the
    // text twice (the name and the always-visible prompt line).
    expect(
      await screen.findByText('one', { selector: '.media__name-text' }),
    ).toBeInTheDocument()
    expect(screen.getByText('#1')).toBeInTheDocument()

    // A deck saves a second sample → the watcher fires → the tab re-lists.
    rows = [
      { file: 'one.wav', title: 'one', prompt: 'one', model: 'sfx', oneShot: false },
      { file: 'two.wav', title: 'two', prompt: 'two', model: 'music', oneShot: false },
    ]
    await act(async () => {
      onChange?.({ payload: { library: 'samples' } })
    })
    expect(
      await screen.findByText('two', { selector: '.media__name-text' }),
    ).toBeInTheDocument()
    // The pre-existing row kept its identity across the reload (no id churn).
    expect(screen.getByText('one', { selector: '.media__name-text' })).toBeInTheDocument()
    expect(screen.getByText('#1')).toBeInTheDocument()
    expect(screen.getByText('#2')).toBeInTheDocument()
  })

  it('restores samples, tagging a freeze and a hand-added file', async () => {
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === 'list_generated_samples') {
        return [
          {
            file: 'Freeze A.wav',
            title: 'Freeze A',
            prompt: null,
            model: 'freeze',
            oneShot: false,
            bpm: 123.6,
            key: '8A',
          },
          { file: 'break.wav', title: 'break', prompt: null, model: null, oneShot: false },
        ]
      }
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Samples' }))
    // A deck capture reads as "Freeze" in its meta; a hand-added file as "Imported".
    await screen.findByText('break')
    const metas = [...document.querySelectorAll('.media__meta')].map(
      (el) => el.textContent ?? '',
    )
    expect(metas).toContain('Freeze · Loop · 124 BPM · 8A')
    expect(metas.some((text) => text.includes('Imported'))).toBe(true)
  })

  it('filters samples across prompt, model, and playback mode metadata', async () => {
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === 'list_generated_samples') {
        return [
          {
            file: 'bass-loop.wav',
            title: 'Low end',
            prompt: 'sub bass pressure',
            model: 'music',
            oneShot: false,
          },
          {
            file: 'laser.wav',
            title: 'Laser hit',
            prompt: 'bright zap',
            model: 'sfx',
            oneShot: true,
          },
        ]
      }
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Samples' }))
    await screen.findByText('Low end')

    fireEvent.change(screen.getByRole('searchbox', { name: 'Search samples' }), {
      target: { value: 'PRESSURE music loop' },
    })

    expect(screen.getByText('Low end')).toBeInTheDocument()
    expect(screen.queryByText('Laser hit')).toBeNull()
  })

  it('auto-saves a composed take to the songs folder via the Rust shell', async () => {
    stubFetch()
    const calls: { cmd: string; args: unknown }[] = []
    const invoke = vi.fn(async (cmd: string, args?: unknown) => {
      calls.push({ cmd, args })
      if (cmd === 'list_generated_songs') return []
      if (cmd === 'save_generated_song') {
        return { file: 'keeper #1.wav', title: 'keeper #1', prompt: 'keeper', model: 'track' }
      }
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    renderExplorer()
    await composeTrack('keeper')
    // The composed take is persisted without a second click — no download button.
    expect(screen.queryByRole('button', { name: 'Save keeper #1' })).toBeNull()
    const saveCall = calls.find((c) => c.cmd === 'save_generated_song')
    expect(saveCall).toBeDefined()
    // The payload frames [u32 LE meta-JSON length][meta JSON][WAV bytes].
    const payload = saveCall!.args as Uint8Array
    const metaLen = new DataView(
      payload.buffer,
      payload.byteOffset,
      payload.byteLength,
    ).getUint32(0, true)
    const meta = JSON.parse(new TextDecoder().decode(payload.subarray(4, 4 + metaLen)))
    expect(meta).toEqual({
      title: 'keeper',
      prompt: 'keeper',
      model: 'track',
      recipe: {
        version: 1,
        prompt: 'keeper',
        engine: 'track',
        seconds: 120,
        loras: [],
        sa3: { negativePrompt: '', seed: 17 },
      },
    })
  })

  it('auto-saves the immutable effective Advanced recipe', async () => {
    stubFetch()
    const calls: { cmd: string; args: unknown }[] = []
    const invoke = vi.fn(async (cmd: string, args?: unknown) => {
      calls.push({ cmd, args })
      if (cmd === 'list_generated_songs') return []
      if (cmd === 'save_generated_song') {
        return { file: 'guided.wav', title: 'guided', prompt: 'guided', model: 'track' }
      }
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    fireEvent.click(screen.getByRole('radio', { name: 'Advanced' }))
    fireEvent.click(screen.getByRole('switch', { name: 'Guidance: Off' }))
    fireEvent.click(screen.getByRole('button', { name: 'No vocals' }))
    fireEvent.change(screen.getByLabelText('Seed behavior'), {
      target: { value: 'fixed' },
    })
    fireEvent.change(screen.getByLabelText('Seed (0–2147483647)'), {
      target: { value: '99' },
    })
    await composeTrack('guided')

    const payload = calls.find((call) => call.cmd === 'save_generated_song')!.args as Uint8Array
    const metaLength = new DataView(
      payload.buffer,
      payload.byteOffset,
      payload.byteLength,
    ).getUint32(0, true)
    const meta = JSON.parse(
      new TextDecoder().decode(payload.subarray(4, 4 + metaLength)),
    )
    expect(meta.recipe).toEqual({
      version: 1,
      prompt: 'guided',
      engine: 'track',
      seconds: 120,
      loras: [],
      sa3: {
        negativePrompt: 'vocals',
        cfg: 3,
        apg: 1,
        seed: 99,
      },
    })
  })

  it('does not attempt a save outside the native shell', async () => {
    stubFetch()
    // No __TAURI__: a plain browser has no disk to write through, so auto-save is
    // skipped silently rather than surfacing an avoidable error.
    renderExplorer()
    await composeTrack('keeper')
    expect(screen.queryByRole('alert')).toBeNull()
  })

  it('opens the songs folder through the Rust shell', async () => {
    const calls: string[] = []
    const invoke = vi.fn(async (cmd: string) => {
      calls.push(cmd)
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Open songs folder' }))
    })
    expect(calls).toContain('open_songs_folder')
  })

  it('filters songs across title, prompt, model, and filename metadata', async () => {
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === 'list_generated_songs') {
        return [
          {
            file: 'midnight-take.wav',
            title: 'Midnight drive',
            prompt: 'rolling breakbeat',
            model: 'magenta',
          },
          {
            file: 'sunrise.wav',
            title: 'Sunrise house',
            prompt: 'bright chords',
            model: 'track',
          },
        ]
      }
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    await screen.findByText('Midnight drive')

    fireEvent.change(screen.getByRole('searchbox', { name: 'Search songs' }), {
      target: { value: 'TAKE rolling magenta' },
    })

    expect(screen.getByText('Midnight drive')).toBeInTheDocument()
    expect(screen.queryByText('Sunrise house')).toBeNull()
  })

  it('clears a song filter and restores all results', async () => {
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === 'list_generated_songs') {
        return [
          { file: 'dub.wav', title: 'Dub', prompt: null, model: null },
          { file: 'house.wav', title: 'House', prompt: null, model: null },
        ]
      }
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    await screen.findByText('Dub')
    fireEvent.change(screen.getByRole('searchbox', { name: 'Search songs' }), {
      target: { value: 'none' },
    })
    expect(screen.getByRole('status')).toHaveTextContent('No results for “none”.')

    fireEvent.click(screen.getByRole('button', { name: 'Clear search' }))

    expect(screen.getByText('Dub')).toBeInTheDocument()
    expect(screen.getByText('House')).toBeInTheDocument()
  })

  it('restores takes from the registry on startup, tagging hand-added files as imported', async () => {
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === 'list_generated_songs') {
        return [
          {
            file: 'late night dub.wav',
            title: 'late night dub',
            prompt: 'late night dub',
            model: 'track',
          },
          { file: 'mixtape.wav', title: 'mixtape', prompt: null, model: null },
        ]
      }
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    // The composed take comes back as its title + a kept-visible #id tag…
    // (scoped to the name: this take's title equals its prompt, which the prompt
    // line also renders.)
    expect(
      await screen.findByText('late night dub', { selector: '.media__name-text' }),
    ).toBeInTheDocument()
    expect(screen.getByText('#1')).toBeInTheDocument()
    // …and the hand-added one is marked Imported (no model).
    expect(screen.getByText('mixtape')).toBeInTheDocument()
    expect(
      screen.getAllByText('Imported').some((el) => el.classList.contains('media__meta')),
    ).toBe(true)
    fireEvent.click(screen.getByRole('button', { name: 'More actions for late night dub #1' }))
    expect(screen.getByRole('menuitem', { name: 'Rename' })).toBeInTheDocument()
    expect(screen.queryByRole('menuitem', { name: 'Reuse settings' })).toBeNull()
  })

  it('promotes saved Basic settings to Advanced with the used seed fixed', async () => {
    const fetchMock = stubFetch()
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === 'list_generated_songs') {
        return [
          {
            file: 'basic.wav',
            title: 'Basic keeper',
            prompt: 'warm house',
            model: 'track',
            recipe: {
              version: 1,
              prompt: 'warm house',
              engine: 'track',
              seconds: 120,
              loras: [],
              // Older Rust registries serialized absent guidance options as null.
              sa3: { negativePrompt: '', cfg: null, apg: null, seed: 55 },
            },
          },
        ]
      }
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    await chooseMenuItem('Basic keeper #1', 'Reuse settings')

    expect(screen.getByRole('radio', { name: 'Advanced' })).toBeChecked()
    const guidance = screen.getByRole('switch', { name: 'Guidance: Off' })
    expect(guidance).not.toBeChecked()
    expect(guidance).toBeEnabled()
    expect(screen.getByLabelText('Avoid concepts')).toBeDisabled()
    expect(screen.getByLabelText('Seed behavior')).toHaveValue('fixed')
    expect(screen.getByLabelText('Seed (0–2147483647)')).toHaveValue('55')
    expect(screen.getByText('Loaded settings from Basic keeper.')).toBeInTheDocument()

    fireEvent.click(guidance)
    expect(screen.getByRole('switch', { name: 'Guidance: On' })).toBeEnabled()
    expect(screen.getByLabelText('Avoid concepts')).toBeEnabled()
    expect(
      screen.getByRole('slider', { name: /Classifier-Free Guidance/ }),
    ).toBeEnabled()
    expect(
      screen.getByRole('slider', { name: /Adaptive Projected Guidance/ }),
    ).toBeEnabled()
    expect(fetchMock).not.toHaveBeenCalled()
  })

  it('recalls a complete saved recipe without changing Title or starting generation', async () => {
    useLorasMock.mockReturnValue([
      {
        name: 'medium/dub',
        base: 'medium',
        slug: 'dub',
        sizeBytes: 100,
        source: null,
        adapterType: 'lora',
        rank: 8,
      },
    ])
    const fetchMock = stubFetch()
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === 'list_generated_songs') {
        return [
          {
            file: 'recipe.wav',
            title: 'Recipe take',
            prompt: 'old display prompt',
            model: 'track',
            recipe: {
              version: 1,
              prompt: 'warm dub',
              engine: 'track',
              seconds: 240,
              loras: [
                { name: 'medium/dub', strength: 1.25 },
                { name: 'medium/missing', strength: 1 },
              ],
              sa3: {
                negativePrompt: 'vocals',
                cfg: 3.5,
                apg: 0.8,
                seed: 77,
              },
            },
          },
        ]
      }
      if (cmd === 'save_generated_song') {
        return {
          file: 'recalled.wav',
          title: 'Keep title',
          prompt: 'warm dub',
          model: 'track',
        }
      }
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    fireEvent.change(screen.getByLabelText('Title'), { target: { value: 'Keep title' } })
    await chooseMenuItem('Recipe take #1', 'Reuse settings')

    expect(screen.getByLabelText('Title')).toHaveValue('Keep title')
    expect(screen.getByLabelText('Track prompt')).toHaveValue('warm dub')
    expect(screen.getByLabelText('Length')).toHaveValue('240')
    expect(screen.getByRole('radio', { name: 'Advanced' })).toHaveAttribute(
      'aria-checked',
      'true',
    )
    expect(screen.getByLabelText('Avoid concepts')).toHaveValue('vocals')
    const guidance = screen.getByRole('switch', { name: 'Guidance: On' })
    expect(guidance).toBeEnabled()
    fireEvent.click(guidance)
    expect(screen.getByRole('switch', { name: 'Guidance: Off' })).toBeEnabled()
    expect(screen.getByLabelText('Avoid concepts')).toHaveValue('vocals')
    expect(screen.getByLabelText('Avoid concepts')).toBeDisabled()
    expect(
      screen.getByRole('slider', { name: /Classifier-Free Guidance/ }),
    ).toBeDisabled()
    fireEvent.click(screen.getByRole('switch', { name: 'Guidance: Off' }))
    expect(screen.getByLabelText('Avoid concepts')).toBeEnabled()
    expect(screen.getByLabelText('Seed behavior')).toHaveValue('fixed')
    expect(screen.getByLabelText('Seed (0–2147483647)')).toHaveValue('77')
    expect(screen.getByText(/without unavailable adapters: medium\/missing/)).toBeInTheDocument()
    expect(fetchMock).not.toHaveBeenCalled()

    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Compose' }))
    })
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/generate',
      expect.objectContaining({
        body: JSON.stringify({
          prompt: 'warm dub',
          seconds: 240,
          kind: 'track',
          loras: [{ name: 'medium/dub', strength: 1.25 }],
          negative_prompt: 'vocals',
          cfg: 3.5,
          apg: 0.8,
          seed: 77,
        }),
      }),
    )
  })

  it('recalls an MCP take whose length is not a preset (bug: Reuse settings refused it)', async () => {
    useLorasMock.mockReturnValue([
      {
        name: 'medium/dub',
        base: 'medium',
        slug: 'dub',
        sizeBytes: 100,
        source: null,
        adapterType: 'lora',
        rank: 8,
      },
    ])
    const fetchMock = stubFetch()
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === 'list_generated_songs') {
        // Exactly what mcp.rs `track_recipe` writes: any length, the 0-4 LoRA
        // range, an empty Avoid, no guidance, the minted seed.
        return [
          {
            file: 'agent.wav',
            title: 'Agent take',
            prompt: 'robot dogs',
            model: 'track',
            recipe: {
              version: 1,
              prompt: 'robot dogs',
              engine: 'track',
              seconds: 213.0,
              loras: [{ name: 'medium/dub', strength: 2.5 }],
              sa3: { negativePrompt: '', seed: 1994801567 },
            },
          },
        ]
      }
      if (cmd === 'save_generated_song') {
        return { file: 'again.wav', title: 'Again', prompt: 'robot dogs', model: 'track' }
      }
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    await chooseMenuItem('Agent take #1', 'Reuse settings')

    expect(screen.getByRole('status')).toHaveTextContent('Loaded settings from Agent take')
    expect(screen.getByLabelText('Track prompt')).toHaveValue('robot dogs')
    expect(screen.getByLabelText('Length')).toHaveValue('213')
    expect(screen.getByRole('option', { name: '3:33' })).toBeInTheDocument()
    expect(screen.getByRole('switch', { name: 'Guidance: Off' })).toBeInTheDocument()
    expect(screen.getByLabelText('Seed (0–2147483647)')).toHaveValue('1994801567')

    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Compose' }))
    })
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/generate',
      expect.objectContaining({
        body: JSON.stringify({
          prompt: 'robot dogs',
          seconds: 213,
          kind: 'track',
          // The rack's own ceiling is 2.
          loras: [{ name: 'medium/dub', strength: 2 }],
          seed: 1994801567,
        }),
      }),
    )
  })

  it('snaps a recalled length the engine cannot render to the nearest preset', async () => {
    const invoke = vi.fn(async (cmd: string) =>
      cmd === 'list_generated_songs'
        ? [
            {
              file: 'long.wav',
              title: 'Long take',
              prompt: 'long',
              model: 'magenta',
              recipe: { version: 1, prompt: 'long', engine: 'magenta', seconds: 400, loras: [] },
            },
          ]
        : undefined,
    )
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    await chooseMenuItem('Long take #1', 'Reuse settings')
    expect(screen.getByLabelText('Length')).toHaveValue('180')
  })

  it('reports a future recipe version without disturbing the form', async () => {
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === 'list_generated_songs') {
        return [
          {
            file: 'future.wav',
            title: 'Future',
            prompt: 'future',
            model: 'track',
            recipe: { version: 2 },
          },
        ]
      }
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    fireEvent.change(screen.getByLabelText('Track prompt'), {
      target: { value: 'current draft' },
    })
    await chooseMenuItem('Future #1', 'Reuse settings')
    expect(screen.getByLabelText('Track prompt')).toHaveValue('current draft')
    expect(screen.getByRole('status')).toHaveTextContent('newer recipe version')
  })

  it('loads a restored take by library reference — no bytes round-trip', async () => {
    const calls: { cmd: string; args: unknown }[] = []
    const invoke = vi.fn(async (cmd: string, args?: unknown) => {
      calls.push({ cmd, args })
      if (cmd === 'list_generated_songs') {
        return [{ file: 'keeper #1.wav', title: 'keeper', prompt: 'keeper', model: 'track' }]
      }
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    const onLoadTrack = vi.fn(async () => true)
    renderExplorer({ onLoadTrack })
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    const loadButton = await screen.findByRole('button', {
      name: 'Load keeper #1 to deck A',
    })
    await act(async () => {
      fireEvent.click(loadButton)
    })
    // A persisted take names its library file; the shell reads and decodes it
    // (ADR-0030) — the explorer never fetches the bytes.
    expect(calls.find((c) => c.cmd === 'read_generated_song')).toBeUndefined()
    expect(onLoadTrack).toHaveBeenCalledWith(
      'a',
      { kind: 'song', name: 'keeper #1.wav' },
      'keeper #1',
    )
  })

  it('deletes a take from the ⋯ menu, moving the file to the Trash and pruning the registry', async () => {
    const calls: { cmd: string; args: unknown }[] = []
    const invoke = vi.fn(async (cmd: string, args?: unknown) => {
      calls.push({ cmd, args })
      if (cmd === 'list_generated_songs') {
        return [{ file: 'keeper #1.wav', title: 'keeper', prompt: 'keeper', model: 'track' }]
      }
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    fireEvent.click(await screen.findByRole('button', { name: 'More actions for keeper #1' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('menuitem', { name: 'Delete from library' }))
    })
    expect(screen.queryByRole('button', { name: 'More actions for keeper #1' })).toBeNull()
    const deleteCall = calls.find((c) => c.cmd === 'delete_generated_song')
    expect(deleteCall?.args).toEqual({ name: 'keeper #1.wav' })
  })

  it('keeps the row and surfaces an error when a delete fails', async () => {
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === 'list_generated_songs') {
        return [{ file: 'keeper #1.wav', title: 'keeper', prompt: 'keeper', model: 'track' }]
      }
      if (cmd === 'delete_generated_song') throw new Error('Trash is unavailable')
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    fireEvent.click(await screen.findByRole('button', { name: 'More actions for keeper #1' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('menuitem', { name: 'Delete from library' }))
    })
    // The disk delete failed, so the row stays (matching disk) and the error shows —
    // it must not vanish and then reappear on the next launch's scan.
    expect(screen.getByRole('button', { name: 'More actions for keeper #1' })).toBeInTheDocument()
    expect(screen.getByRole('alert')).toHaveTextContent('delete keeper')
    expect(screen.getByRole('alert')).toHaveTextContent('Trash is unavailable')
  })

  it('shows the prompt inline on the row and expands it on click', async () => {
    const prompt = 'deep rolling dub techno with tape hiss and a long modular intro'
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === 'list_generated_songs') {
        return [{ file: 'dub.wav', title: 'Dub Reverie', prompt, model: 'magenta' }]
      }
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    // Inline on the title's line (CSS truncates it to one row), with the full
    // text on the hover tooltip; collapsed at first.
    const promptLine = await screen.findByRole('button', {
      name: 'Show the full prompt for Dub Reverie #1',
    })
    expect(promptLine).toHaveTextContent(prompt)
    expect(promptLine).toHaveAttribute('title', prompt)
    expect(promptLine).toHaveAttribute('aria-expanded', 'false')
    // Clicking the truncated prompt expands it to the full text; clicking again
    // collapses it back to one line.
    fireEvent.click(promptLine)
    expect(promptLine).toHaveAttribute('aria-expanded', 'true')
    expect(promptLine.className).toContain('media__prompt--expanded')
    fireEvent.click(promptLine)
    expect(promptLine).toHaveAttribute('aria-expanded', 'false')
  })

  it('pretty-prints a JSON prompt in the inline prompt line', async () => {
    const minified = '{"title":"X","bpm":120}'
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === 'list_generated_songs') {
        return [{ file: 'x.wav', title: 'My Take', prompt: minified, model: 'magenta' }]
      }
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    await screen.findByText('My Take', { selector: '.media__name-text' })
    // The prompt is re-indented, not the minified original.
    const expected = JSON.stringify(JSON.parse(minified), null, 2)
    expect(document.querySelector('.media__prompt')?.textContent).toBe(expected)
  })

  it('uses the Title field for the name and filename, independent of the prompt', async () => {
    stubFetch()
    const calls: { cmd: string; args: unknown }[] = []
    const invoke = vi.fn(async (cmd: string, args?: unknown) => {
      calls.push({ cmd, args })
      if (cmd === 'list_generated_songs') return []
      if (cmd === 'save_generated_song') {
        return { file: 'Porcelain Halo.wav', title: 'Porcelain Halo', prompt: '{"a":1}', model: 'track' }
      }
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    fireEvent.change(screen.getByLabelText('Title'), { target: { value: 'Porcelain Halo' } })
    fireEvent.change(screen.getByLabelText('Track prompt'), { target: { value: '{"a":1}' } })
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Compose' }))
    })
    // The row shows the title, not the (JSON) prompt.
    expect(screen.getByText('Porcelain Halo')).toBeInTheDocument()
    // Saved metadata keeps the title and the prompt separate.
    const saveCall = calls.find((c) => c.cmd === 'save_generated_song')
    const payload = saveCall!.args as Uint8Array
    const metaLen = new DataView(
      payload.buffer,
      payload.byteOffset,
      payload.byteLength,
    ).getUint32(0, true)
    const meta = JSON.parse(new TextDecoder().decode(payload.subarray(4, 4 + metaLen)))
    expect(meta).toEqual({
      title: 'Porcelain Halo',
      prompt: '{"a":1}',
      model: 'track',
      recipe: {
        version: 1,
        prompt: '{"a":1}',
        engine: 'track',
        seconds: 120,
        loras: [],
        sa3: { negativePrompt: '', seed: 17 },
      },
    })
  })

  it('falls back to a random title when the Title field is blank', async () => {
    stubFetch()
    const calls: { cmd: string; args: unknown }[] = []
    const invoke = vi.fn(async (cmd: string, args?: unknown) => {
      calls.push({ cmd, args })
      if (cmd === 'list_generated_songs') return []
      if (cmd === 'save_generated_song') {
        return { file: 'x.wav', title: 'x', prompt: 'x', model: 'track' }
      }
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    // Title left blank — only a prompt is given.
    fireEvent.change(screen.getByLabelText('Track prompt'), {
      target: { value: 'rolling sub bass' },
    })
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Compose' }))
    })
    const saveCall = calls.find((c) => c.cmd === 'save_generated_song')
    const payload = saveCall!.args as Uint8Array
    const metaLen = new DataView(
      payload.buffer,
      payload.byteOffset,
      payload.byteLength,
    ).getUint32(0, true)
    const meta = JSON.parse(new TextDecoder().decode(payload.subarray(4, 4 + metaLen)))
    // A non-empty title was generated, distinct from the prompt that was sent.
    expect(meta.title).toBeTruthy()
    expect(meta.title).not.toBe('rolling sub bass')
    expect(meta.prompt).toBe('rolling sub bass')
  })

  it('cycles the visible tab on a hardware rotary press', () => {
    const bus = createControlBus()
    renderExplorer({}, [], bus)
    act(() => bus.publish({ kind: 'browse_tab' }))
    expect(screen.getByLabelText('Track prompt')).toBeInTheDocument()
    act(() => bus.publish({ kind: 'browse_tab' }))
    // Samples sits between Generate and Folder in the rotation.
    expect(screen.getByLabelText('Loop prompt')).toBeInTheDocument()
    act(() => bus.publish({ kind: 'browse_tab' }))
    expect(
      screen.getByRole('button', { name: 'Choose folder' }),
    ).toBeInTheDocument()
    act(() => bus.publish({ kind: 'browse_tab' }))
    // Full circle: back on the crates tab.
    expect(
      screen.getByText("No presets yet — save a deck's style below its pad"),
    ).toBeInTheDocument()
  })

  it('uses the native picker + Rust commands under Tauri', async () => {
    const calls: { cmd: string; args: unknown }[] = []
    const invoke = vi.fn(async (cmd: string, args?: unknown) => {
      calls.push({ cmd, args })
      if (cmd === 'plugin:dialog|open') return '/Users/dj/DJ Sets'
      if (cmd === 'list_audio_files') return ['a-side.mp3', 'b-side.wav']
      return undefined
    })
    // Presence of `__TAURI__` is what isTauri() keys on; its core.invoke is the bridge.
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    const onLoadTrack = vi.fn(async () => true)
    renderExplorer({ onLoadTrack })
    fireEvent.click(screen.getByRole('tab', { name: 'Folder' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Choose folder' }))
    })
    // The picked path's basename shows, and the Rust listing populates.
    expect(screen.getByText('DJ Sets')).toBeInTheDocument()
    expect(screen.getByText('a-side.mp3')).toBeInTheDocument()
    await act(async () => {
      fireEvent.click(
        screen.getByRole('button', { name: 'Load a-side.mp3 to deck A' }),
      )
    })
    // The load is BY REFERENCE (ADR-0030): the chosen dir + the plain name —
    // the shell re-derives the scoped path, reads, and decodes; the explorer
    // fetches no bytes.
    expect(calls.find((c) => c.cmd === 'read_audio_file')).toBeUndefined()
    expect(onLoadTrack).toHaveBeenCalledWith(
      'a',
      { kind: 'folder', dir: '/Users/dj/DJ Sets', name: 'a-side.mp3' },
      'a-side.mp3',
    )
  })

  it('filters folder files and scopes hardware loading to the results', async () => {
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === 'plugin:dialog|open') return '/Users/dj/DJ Sets'
      if (cmd === 'list_audio_files') return ['a-side.mp3', 'b-side.wav']
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    const bus = createControlBus()
    const onLoadTrack = vi.fn(async () => true)
    renderExplorer({ onLoadTrack }, [], bus)
    fireEvent.click(screen.getByRole('tab', { name: 'Folder' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Choose folder' }))
    })
    fireEvent.change(screen.getByRole('searchbox', { name: 'Search folder files' }), {
      target: { value: 'B-SIDE' },
    })

    expect(screen.queryByText('a-side.mp3')).toBeNull()
    expect(screen.getByText('b-side.wav')).toBeInTheDocument()
    act(() => bus.publish({ kind: 'browse_load', deck: 'a' }))
    expect(onLoadTrack).toHaveBeenCalledWith(
      'a',
      { kind: 'folder', dir: '/Users/dj/DJ Sets', name: 'b-side.wav' },
      'b-side.wav',
    )
  })

  it('a refused folder load shows the shell reason, not the generic message', async () => {
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === 'plugin:dialog|open') return '/Users/dj/DJ Sets'
      if (cmd === 'list_audio_files') return ['chained.ogg']
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    // A shell refusal rejects with its reason — a string, the Tauri command
    // error shape (ADR-0030: an explicit load error, never a silent one).
    const onLoadTrack = vi.fn(async () => true)
    onLoadTrack.mockRejectedValue('unsupported audio codec: opus')
    renderExplorer({ onLoadTrack })
    fireEvent.click(screen.getByRole('tab', { name: 'Folder' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Choose folder' }))
    })
    await act(async () => {
      fireEvent.click(
        screen.getByRole('button', { name: 'Load chained.ogg to deck A' }),
      )
    })
    expect(screen.getByText('unsupported audio codec: opus')).toBeInTheDocument()
    expect(screen.queryByText('chained.ogg could not be decoded')).toBeNull()
  })

  it('dismissing the native picker lists nothing and shows no error', async () => {
    const invoke = vi.fn(async (cmd: string) =>
      cmd === 'plugin:dialog|open' ? null : undefined,
    )
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Folder' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Choose folder' }))
    })
    expect(invoke.mock.calls.some((c) => c[0] === 'list_audio_files')).toBe(false)
    expect(screen.queryByRole('alert')).toBeNull()
  })

  it('trims a trailing slash from the native folder name', async () => {
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === 'plugin:dialog|open') return '/Users/dj/My Sets/'
      if (cmd === 'list_audio_files') return []
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Folder' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Choose folder' }))
    })
    expect(screen.getByText('My Sets')).toBeInTheDocument()
  })

  it('surfaces a native listing error', async () => {
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === 'plugin:dialog|open') return '/Users/dj/Locked'
      if (cmd === 'list_audio_files') throw new Error('cannot read folder: denied')
      return undefined
    })
    vi.stubGlobal('__TAURI__', { core: { invoke } })
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Folder' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Choose folder' }))
    })
    expect(screen.getByRole('alert')).toHaveTextContent('cannot read folder: denied')
  })
})

describe('library curation', () => {
  type Song = {
    file: string
    title: string
    prompt: string | null
    model: string | null
    recipe?: unknown
    rating?: number
    tags?: string[]
  }
  const dub: Song = { file: 'dub.wav', title: 'Dub', prompt: 'dub', model: 'track', rating: 3 }

  /** Stub the shell with a mutable songs registry. `respond` answers any other
   * command (return undefined to fall through); `calls` records every invoke. */
  function stubShell(
    songs: () => Song[],
    respond: (cmd: string, args: Record<string, unknown>) => unknown = () => undefined,
    samples: () => unknown[] = () => [],
  ) {
    const calls: { cmd: string; args: Record<string, unknown> }[] = []
    let onLibraryChanged: ((e: { payload: unknown }) => void) | null = null
    const invoke = vi.fn(async (cmd: string, args: Record<string, unknown> = {}) => {
      calls.push({ cmd, args })
      const answer = respond(cmd, args)
      if (answer !== undefined) return answer
      if (cmd === 'list_generated_songs') return songs()
      if (cmd === 'list_generated_samples') return samples()
      return undefined
    })
    const listen = vi.fn(async (event: string, handler: (e: { payload: unknown }) => void) => {
      if (event === 'library://changed') onLibraryChanged = handler
      return () => {}
    })
    vi.stubGlobal('__TAURI__', { core: { invoke }, event: { listen } })
    return {
      calls,
      commands: (name: string) => calls.filter((call) => call.cmd === name),
      changed: (library: string) => onLibraryChanged?.({ payload: { library } }),
    }
  }

  async function openGenerate(firstTitle: string) {
    fireEvent.click(screen.getByRole('tab', { name: 'Songs' }))
    await screen.findByText(firstTitle, { selector: '.media__name-text' })
  }

  const rowTitles = () =>
    [...document.querySelectorAll('.media__list .media__name-text')].map(
      (element) => element.textContent,
    )
  const ratingOf = (name: string) =>
    within(screen.getByRole('radiogroup', { name: `Rating for ${name}` }))

  describe('stars', () => {
    it('shows the registry rating on each row', async () => {
      stubShell(() => [dub, { ...dub, file: 'raw.wav', title: 'Raw', rating: undefined }])
      renderExplorer()
      await openGenerate('Dub')
      expect(ratingOf('Dub #1').getByRole('radio', { name: '3 stars' })).toBeChecked()
      expect(
        ratingOf('Raw #2').getAllByRole('radio').some((radio) => (radio as HTMLInputElement).checked),
      ).toBe(false)
    })

    it('rates through the shell and adopts the returned entry', async () => {
      const shell = stubShell(
        () => [dub],
        (cmd, args) =>
          cmd === 'rate_generated_song'
            ? { ...dub, title: 'Dub (shell)', rating: args.rating as number }
            : undefined,
      )
      renderExplorer()
      await openGenerate('Dub')
      await act(async () => {
        fireEvent.click(ratingOf('Dub #1').getByRole('radio', { name: '4 stars' }))
      })
      expect(shell.commands('rate_generated_song').map((call) => call.args)).toEqual([
        { name: 'dub.wav', rating: 4 },
      ])
      expect(ratingOf('Dub (shell) #1').getByRole('radio', { name: '4 stars' })).toBeChecked()
    })

    it('clears with a click on the current star', async () => {
      const shell = stubShell(
        () => [dub],
        (cmd) => (cmd === 'rate_generated_song' ? { ...dub, rating: undefined } : undefined),
      )
      renderExplorer()
      await openGenerate('Dub')
      await act(async () => {
        fireEvent.click(ratingOf('Dub #1').getByRole('radio', { name: '3 stars' }))
      })
      expect(shell.commands('rate_generated_song')[0].args).toEqual({
        name: 'dub.wav',
        rating: null,
      })
      expect(ratingOf('Dub #1').getByRole('radio', { name: '3 stars' })).not.toBeChecked()
    })

    it('shows a failed rating and re-lists to resync', async () => {
      const shell = stubShell(
        () => [dub],
        (cmd) => (cmd === 'rate_generated_song' ? Promise.reject(new Error('locked')) : undefined),
      )
      renderExplorer()
      await openGenerate('Dub')
      await act(async () => {
        fireEvent.click(ratingOf('Dub #1').getByRole('radio', { name: '5 stars' }))
      })
      expect(screen.getByRole('alert')).toHaveTextContent("Couldn't rate Dub: locked")
      expect(shell.commands('list_generated_songs')).toHaveLength(2)
      expect(ratingOf('Dub #1').getByRole('radio', { name: '3 stars' })).toBeChecked()
    })

    it('rates a sample through the samples command', async () => {
      const sample = { file: 'riser.wav', title: 'Riser', prompt: 'riser', model: 'sfx', oneShot: true }
      const shell = stubShell(
        () => [],
        (cmd) => (cmd === 'rate_generated_sample' ? { ...sample, rating: 2 } : undefined),
        () => [sample],
      )
      renderExplorer()
      fireEvent.click(screen.getByRole('tab', { name: 'Samples' }))
      await screen.findByText('Riser', { selector: '.media__name-text' })
      await act(async () => {
        fireEvent.click(ratingOf('Riser #1').getByRole('radio', { name: '2 stars' }))
      })
      expect(shell.commands('rate_generated_sample')[0].args).toEqual({
        name: 'riser.wav',
        rating: 2,
      })
    })
  })

  describe('rename', () => {
    function renameShell(respond?: (title: string) => unknown) {
      return stubShell(
        () => [dub],
        (cmd, args) =>
          cmd === 'rename_generated_song'
            ? (respond?.(args.title as string) ?? { ...dub, title: args.title })
            : undefined,
      )
    }
    const editor = () => screen.getByRole('textbox', { name: 'New title for Dub #1' })
    const openEditor = () => chooseMenuItem('Dub #1', 'Rename')

    it('opens an editor prefilled with the title and commits on Enter', async () => {
      const shell = renameShell()
      renderExplorer()
      await openGenerate('Dub')
      await openEditor()
      expect(editor()).toHaveValue('Dub')
      expect(editor()).toHaveFocus()
      fireEvent.change(editor(), { target: { value: '  Dub Plate  ' } })
      await act(async () => {
        fireEvent.keyDown(editor(), { key: 'Enter' })
      })
      expect(shell.commands('rename_generated_song').map((call) => call.args)).toEqual([
        { name: 'dub.wav', title: 'Dub Plate' },
      ])
      expect(screen.queryByRole('textbox', { name: /New title/ })).toBeNull()
      expect(rowTitles()).toEqual(['Dub Plate'])
      // The session #id stays with the row through a rename.
      expect(screen.getByText('#1')).toBeInTheDocument()
    })

    it('opens the editor on a double-click of the name', async () => {
      renameShell()
      renderExplorer()
      await openGenerate('Dub')
      fireEvent.doubleClick(screen.getByRole('button', { name: 'Select Dub #1' }))
      expect(editor()).toHaveValue('Dub')
    })

    it('cancels on Escape without calling the shell', async () => {
      const shell = renameShell()
      renderExplorer()
      await openGenerate('Dub')
      await openEditor()
      fireEvent.change(editor(), { target: { value: 'Something else' } })
      await act(async () => {
        fireEvent.keyDown(editor(), { key: 'Escape' })
      })
      expect(shell.commands('rename_generated_song')).toHaveLength(0)
      expect(rowTitles()).toEqual(['Dub'])
    })

    it('sends nothing for an unchanged or empty title', async () => {
      const shell = renameShell()
      renderExplorer()
      await openGenerate('Dub')
      await openEditor()
      fireEvent.change(editor(), { target: { value: ' Dub ' } })
      await act(async () => {
        fireEvent.keyDown(editor(), { key: 'Enter' })
      })
      await openEditor()
      fireEvent.change(editor(), { target: { value: '   ' } })
      await act(async () => {
        fireEvent.keyDown(editor(), { key: 'Enter' })
      })
      expect(shell.commands('rename_generated_song')).toHaveLength(0)
      expect(rowTitles()).toEqual(['Dub'])
    })

    it('commits on blur', async () => {
      const shell = renameShell()
      renderExplorer()
      await openGenerate('Dub')
      await openEditor()
      fireEvent.change(editor(), { target: { value: 'Dub Siren' } })
      await act(async () => {
        fireEvent.blur(editor())
      })
      expect(shell.commands('rename_generated_song')[0].args).toEqual({
        name: 'dub.wav',
        title: 'Dub Siren',
      })
      expect(rowTitles()).toEqual(['Dub Siren'])
    })

    it('renames a sample through the samples command', async () => {
      const sample = { file: 'fz.wav', title: 'A freeze 3', prompt: null, model: 'freeze', oneShot: false }
      const shell = stubShell(
        () => [],
        (cmd, args) =>
          cmd === 'rename_generated_sample' ? { ...sample, title: args.title } : undefined,
        () => [sample],
      )
      renderExplorer()
      fireEvent.click(screen.getByRole('tab', { name: 'Samples' }))
      await screen.findByText('A freeze 3', { selector: '.media__name-text' })
      await chooseMenuItem('A freeze 3', 'Rename')
      const field = screen.getByRole('textbox', { name: 'New title for A freeze 3' })
      fireEvent.change(field, { target: { value: 'vocal chop' } })
      await act(async () => {
        fireEvent.keyDown(field, { key: 'Enter' })
      })
      expect(shell.commands('rename_generated_sample')[0].args).toEqual({
        name: 'fz.wav',
        title: 'vocal chop',
      })
      expect(rowTitles()).toEqual(['vocal chop'])
    })

    it('shows a failed rename and re-lists to resync', async () => {
      const shell = renameShell(() => Promise.reject(new Error('too long')))
      renderExplorer()
      await openGenerate('Dub')
      await openEditor()
      fireEvent.change(editor(), { target: { value: 'Dub Siren' } })
      await act(async () => {
        fireEvent.keyDown(editor(), { key: 'Enter' })
      })
      expect(screen.getByRole('alert')).toHaveTextContent("Couldn't rename Dub: too long")
      expect(shell.commands('list_generated_songs')).toHaveLength(2)
      expect(rowTitles()).toEqual(['Dub'])
    })
  })

  describe('sort', () => {
    // Registry order is oldest-first; the explorer shows newest-first.
    const library: Song[] = [
      { file: 'b.wav', title: 'beta', prompt: null, model: null, rating: 2 },
      { file: 'g.wav', title: 'Gamma', prompt: null, model: null },
      { file: 'a.wav', title: 'alpha', prompt: null, model: null, rating: 5 },
    ]

    it('reorders the list from the header select and persists the choice', async () => {
      stubShell(() => library)
      renderExplorer()
      await openGenerate('alpha')
      expect(rowTitles()).toEqual(['alpha', 'Gamma', 'beta'])
      fireEvent.change(screen.getByLabelText('Sort'), { target: { value: 'rating' } })
      expect(rowTitles()).toEqual(['alpha', 'beta', 'Gamma'])
      fireEvent.change(screen.getByLabelText('Sort'), { target: { value: 'title' } })
      expect(rowTitles()).toEqual(['alpha', 'beta', 'Gamma'])
      expect(JSON.parse(localStorage.getItem('lsdj:v1')!).app.mediaSort).toBe('title')
    })

    it('restores the persisted sort and walks it with the rotary', async () => {
      localStorage.setItem('lsdj:v1', JSON.stringify({ app: { mediaSort: 'rating' } }))
      stubShell(() => library)
      const bus = createControlBus()
      const onLoadTrack = vi.fn(async () => true)
      renderExplorer({ onLoadTrack }, [], bus)
      await openGenerate('alpha')
      expect(screen.getByLabelText('Sort')).toHaveValue('rating')
      act(() => bus.publish({ kind: 'browse_scroll', steps: 1 }))
      await act(async () => {
        bus.publish({ kind: 'browse_load', deck: 'a' })
      })
      expect(onLoadTrack).toHaveBeenCalledWith('a', { kind: 'song', name: 'b.wav' }, 'beta')
    })

    it('hides the sort outside the song and sample libraries', () => {
      renderExplorer()
      expect(screen.queryByLabelText('Sort')).toBeNull()
      fireEvent.click(screen.getByRole('tab', { name: 'Folder' }))
      expect(screen.queryByLabelText('Sort')).toBeNull()
    })
  })

  describe('highlight and number keys', () => {
    const pair: Song[] = [dub, { file: 'raw.wav', title: 'Raw', prompt: 'raw', model: 'track' }]

    it('highlights a row when its name is clicked', async () => {
      stubShell(() => pair)
      renderExplorer()
      await openGenerate('Dub')
      const name = screen.getByRole('button', { name: 'Select Dub #1' })
      fireEvent.click(name)
      expect(name).toHaveAttribute('aria-current', 'true')
      expect(name.closest('li')).toHaveClass('media__item--highlighted')
      expect(screen.getByRole('button', { name: 'Select Raw #2' })).toHaveAttribute(
        'aria-current',
        'false',
      )
    })

    it('rates the highlighted row with 1–5 and clears it with 0', async () => {
      const shell = stubShell(
        () => pair,
        (cmd, args) =>
          cmd === 'rate_generated_song' ? { ...dub, rating: args.rating ?? undefined } : undefined,
      )
      renderExplorer()
      await openGenerate('Dub')
      fireEvent.click(screen.getByRole('button', { name: 'Select Dub #1' }))
      await act(async () => {
        fireEvent.keyDown(window, { key: '3' })
      })
      await act(async () => {
        fireEvent.keyDown(window, { key: '0' })
      })
      // A modified key is someone else's shortcut.
      await act(async () => {
        fireEvent.keyDown(window, { key: '5', metaKey: true })
      })
      expect(shell.commands('rate_generated_song').map((call) => call.args)).toEqual([
        { name: 'dub.wav', rating: 3 },
        { name: 'dub.wav', rating: null },
      ])
    })

    it('ignores number keys while typing in the search or the rename editor', async () => {
      const shell = stubShell(() => pair)
      renderExplorer()
      await openGenerate('Dub')
      fireEvent.keyDown(screen.getByRole('searchbox'), { key: '3' })
      await chooseMenuItem('Dub #1', 'Rename')
      fireEvent.keyDown(screen.getByRole('textbox', { name: 'New title for Dub #1' }), {
        key: '4',
      })
      expect(shell.commands('rate_generated_song')).toHaveLength(0)
    })
  })

  describe('row context', () => {
    const deckSnap = (track: { title: string } | null) => ({
      track: track && { ...track, bpm: null, durationSeconds: 120 },
      loopLabels: [],
    })
    function storeReports(trackA: { title: string } | null) {
      vi.mocked(useInterfaceStore).mockReturnValue({
        pianoWindowOpen: false,
        decks: [deckSnap(trackA), deckSnap(null)],
      } as unknown as ReturnType<typeof useInterfaceStore>)
    }

    it('badges the row a deck holds, then mutes it once played', async () => {
      stubShell(() => [dub])
      storeReports(null)
      renderExplorer()
      await openGenerate('Dub')
      // The store already reports the load by the time the deck call resolves.
      storeReports({ title: 'Dub #1' })
      await act(async () => {
        fireEvent.click(screen.getByRole('button', { name: 'Load Dub #1 to deck A' }))
      })
      const row = screen.getByText('Dub', { selector: '.media__name-text' }).closest('li')!
      expect(within(row).getByText('On A')).toBeInTheDocument()
      expect(row).not.toHaveClass('media__item--played')

      // Ejected: the badge goes, the played mark stays. (Any re-render picks up the
      // store; expanding the prompt is a convenient one.)
      storeReports(null)
      fireEvent.click(screen.getByRole('button', { name: 'Show the full prompt for Dub #1' }))
      expect(within(row).queryByText('On A')).toBeNull()
      expect(row).toHaveClass('media__item--played')
    })

    it('drops the badge when the deck holds a different track', async () => {
      stubShell(() => [dub])
      storeReports({ title: 'Dub #1' })
      renderExplorer()
      await openGenerate('Dub')
      await act(async () => {
        fireEvent.click(screen.getByRole('button', { name: 'Load Dub #1 to deck A' }))
      })
      expect(screen.getByText('On A')).toBeInTheDocument()
      storeReports({ title: 'Something else' })
      fireEvent.click(screen.getByRole('button', { name: 'Show the full prompt for Dub #1' }))
      expect(screen.queryByText('On A')).toBeNull()
    })

    it('shows the recipe length in the meta column, blank without one', async () => {
      stubShell(() => [
        { ...dub, recipe: { version: 1, engine: 'track', prompt: 'dub', seconds: 120, loras: [] } },
        { file: 'raw.wav', title: 'Raw', prompt: null, model: 'track' },
      ])
      renderExplorer()
      await openGenerate('Dub')
      const metas = [...document.querySelectorAll('.media__meta')].map((el) => el.textContent)
      expect(metas).toEqual(['Track (SA3 medium)', 'Track (SA3 medium) · 2:00'])
    })
  })

  describe('row menu and tags', () => {
    const recipe = { version: 1, engine: 'track', prompt: 'dub', seconds: 120, loras: [] }
    const tagged: Song = { ...dub, tags: ['dub', 'drone'] }
    // Its prompt merely contains the word "drone"; it isn't tagged with it.
    const raw: Song = { file: 'raw.wav', title: 'Raw', prompt: 'droney raw', model: 'track', tags: ['ambient'] }
    const riser = { file: 'riser.wav', title: 'Riser', prompt: 'riser', model: 'sfx', oneShot: true, tags: ['riser'] }
    const rowOf = (title: string) =>
      screen.getByText(title, { selector: '.media__name-text' }).closest('li')!
    const chipsOf = (title: string) =>
      [...rowOf(title).querySelectorAll('.media__tag')].map((chip) => chip.textContent)
    const tagShell = (respond?: (tags: string[]) => unknown) =>
      stubShell(
        () => [tagged, raw],
        (cmd, args) =>
          cmd === 'tag_generated_song'
            ? (respond?.(args.tags as string[]) ?? { ...tagged, tags: args.tags })
            : undefined,
        () => [riser],
      )

    it('lists Rename, Tags…, Crop…, Add to playlist…, Use as source, Reuse settings and Delete from library in order', async () => {
      stubShell(() => [{ ...dub, recipe }])
      renderExplorer()
      await openGenerate('Dub')
      fireEvent.click(screen.getByRole('button', { name: 'More actions for Dub #1' }))
      expect(screen.getAllByRole('menuitem').map((item) => item.textContent)).toEqual([
        'Rename',
        'Tags…',
        'Crop…',
        'Add to playlist…',
        'Use as source',
        'Reuse settings',
        'Delete from library',
      ])
    })

    describe('crop', () => {
      // 10 buckets over 4 s: silence, sound in buckets 2..7, silence.
      const levels = [0, 0, 0.5, 0.4, 0.3, 0.3, 0.2, 0.1, 0, 0]
      const peaks = {
        min: levels.map((level) => -level),
        max: levels,
        duration: 4,
      }
      const cropShell = () =>
        stubShell(
          () => [dub],
          (cmd) => {
            if (cmd === 'library_clip_peaks') return peaks
            if (cmd === 'library_crop_preview') return new ArrayBuffer(8)
            if (cmd === 'crop_generated_song') {
              return { ...dub, file: 'Dub -crop-.wav', title: 'Dub (crop)', rating: undefined }
            }
            return undefined
          },
          () => [riser],
        )

      it('opens the waveform from the menu, auto-trims, previews the selection and saves a new clip', async () => {
        const shell = cropShell()
        const onPreview = vi.fn(async () => {})
        renderExplorer({ onPreview })
        await openGenerate('Dub')
        await chooseMenuItem('Dub #1', 'Crop…')
        const panel = within(await screen.findByRole('dialog', { name: 'Crop Dub #1' }))
        await panel.findByRole('slider', { name: 'Crop start' })
        expect(panel.getByLabelText('End (s)')).toHaveValue(4)

        fireEvent.click(panel.getByRole('button', { name: 'Auto-trim silence' }))
        expect(panel.getByLabelText('Start (s)')).toHaveValue(0.8)
        expect(panel.getByLabelText('End (s)')).toHaveValue(3.2)
        fireEvent.keyDown(panel.getByRole('slider', { name: 'Crop end' }), {
          key: 'ArrowLeft',
          shiftKey: true,
        })
        expect(panel.getByLabelText('End (s)')).toHaveValue(3.1)

        await act(async () => {
          fireEvent.click(panel.getByRole('button', { name: 'Preview' }))
        })
        expect(shell.commands('library_crop_preview').map((call) => call.args)).toEqual([
          { library: 'songs', name: 'dub.wav', start: 0.8, end: 3.1 },
        ])
        expect(onPreview).toHaveBeenCalledTimes(1)

        await act(async () => {
          fireEvent.click(panel.getByRole('button', { name: 'Save as new clip' }))
        })
        expect(shell.commands('crop_generated_song').map((call) => call.args)).toEqual([
          { name: 'dub.wav', start: 0.8, end: 3.1, replace: false },
        ])
        expect(screen.queryByRole('dialog', { name: 'Crop Dub #1' })).toBeNull()
        // The saved crop re-lists the library.
        expect(shell.commands('list_generated_songs').length).toBeGreaterThan(1)
      })

      it('crops a sample through the samples command', async () => {
        const shell = cropShell()
        renderExplorer()
        fireEvent.click(screen.getByRole('tab', { name: 'Samples' }))
        await screen.findByText('Riser', { selector: '.media__name-text' })
        await chooseMenuItem('Riser #2', 'Crop…')
        const panel = within(await screen.findByRole('dialog', { name: 'Crop Riser #2' }))
        await panel.findByRole('slider', { name: 'Crop start' })
        fireEvent.change(panel.getByLabelText('Start (s)'), { target: { value: '1.5' } })
        await act(async () => {
          fireEvent.click(panel.getByRole('button', { name: 'Save as new clip' }))
        })
        expect(shell.commands('crop_generated_sample').map((call) => call.args)).toEqual([
          { name: 'riser.wav', start: 1.5, end: 4, replace: false },
        ])
      })

      it('replaces the original in place when asked', async () => {
        const shell = cropShell()
        renderExplorer()
        fireEvent.click(screen.getByRole('tab', { name: 'Samples' }))
        await screen.findByText('Riser', { selector: '.media__name-text' })
        await chooseMenuItem('Riser #2', 'Crop…')
        const panel = within(await screen.findByRole('dialog', { name: 'Crop Riser #2' }))
        await panel.findByRole('slider', { name: 'Crop start' })
        fireEvent.change(panel.getByLabelText('Start (s)'), { target: { value: '1.5' } })
        await act(async () => {
          fireEvent.click(panel.getByRole('button', { name: 'Replace original' }))
        })
        expect(shell.commands('crop_generated_sample').map((call) => call.args)).toEqual([
          { name: 'riser.wav', start: 1.5, end: 4, replace: true },
        ])
      })

      it('shows the shell’s refusal and keeps the panel open', async () => {
        stubShell(
          () => [dub],
          (cmd) => {
            if (cmd === 'library_clip_peaks') return peaks
            if (cmd === 'crop_generated_song') throw new Error('the crop is empty')
            return undefined
          },
        )
        renderExplorer()
        await openGenerate('Dub')
        await chooseMenuItem('Dub #1', 'Crop…')
        const panel = within(await screen.findByRole('dialog', { name: 'Crop Dub #1' }))
        await panel.findByRole('slider', { name: 'Crop start' })
        await act(async () => {
          fireEvent.click(panel.getByRole('button', { name: 'Save as new clip' }))
        })
        expect(panel.getByRole('alert')).toHaveTextContent("Couldn't crop Dub #1: the crop is empty")
      })
    })

    it('continues a saved song: Use as source cuts the window and posts it as init audio', async () => {
      const oneSecondClip = new ArrayBuffer(44 + 44_100 * 4)
      const shell = stubShell(
        () => [dub],
        (cmd) =>
          cmd === 'song_source_clip'
            ? oneSecondClip
            : cmd === 'save_generated_song'
              ? { ...dub, file: 'more.wav', title: 'More' }
              : undefined,
      )
      const fetchMock = stubFetch()
      renderExplorer()
      await openGenerate('Dub')
      await chooseMenuItem('Dub #1', 'Use as source')
      expect(screen.getByRole('radio', { name: 'Continue' })).toHaveAttribute(
        'aria-checked',
        'true',
      )
      fireEvent.change(screen.getByLabelText('To (s)'), { target: { value: '1' } })
      fireEvent.change(screen.getByLabelText('Track prompt'), { target: { value: 'more dub' } })
      await act(async () => {
        fireEvent.click(screen.getByRole('button', { name: 'Compose' }))
      })

      expect(shell.commands('song_source_clip')[0].args).toEqual({
        name: 'dub.wav',
        start: 0,
        end: 1,
      })
      const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit]
      expect(url).toBe('/api/generate')
      const form = init.body as FormData
      expect(JSON.parse(form.get('request') as string)).toMatchObject({
        prompt: 'more dub',
        seconds: 120,
        kind: 'track',
        inpaint_range: [1, 120],
      })
      expect(form.get('init_audio')).toBeInstanceOf(Blob)
    })

    it('refuses a source window whose end is not after its start', async () => {
      stubShell(() => [dub])
      const fetchMock = stubFetch()
      renderExplorer()
      await openGenerate('Dub')
      await chooseMenuItem('Dub #1', 'Use as source')
      fireEvent.change(screen.getByLabelText('From (s)'), { target: { value: '30' } })
      fireEvent.change(screen.getByLabelText('To (s)'), { target: { value: '10' } })
      fireEvent.change(screen.getByLabelText('Track prompt'), { target: { value: 'more dub' } })
      await act(async () => {
        fireEvent.click(screen.getByRole('button', { name: 'Compose' }))
      })
      expect(screen.getByRole('alert')).toHaveTextContent(
        'The source window needs From before To, in seconds.',
      )
      expect(fetchMock).not.toHaveBeenCalled()
    })

    it('gives a pending row no menu', async () => {
      vi.stubGlobal('fetch', vi.fn(() => new Promise(() => {})))
      renderExplorer()
      await composeTrack('wip')
      expect(screen.getByText('wip — composing…')).toBeInTheDocument()
      expect(screen.queryByRole('button', { name: /More actions/ })).toBeNull()
    })

    // A row's chip used to fill the search box; it now toggles the tag filter.
    it('renders tag chips and filters by a tag when one is clicked', async () => {
      tagShell()
      renderExplorer()
      await openGenerate('Dub')
      expect(chipsOf('Dub')).toEqual(['#dub', '#drone'])
      expect(chipsOf('Raw')).toEqual(['#ambient'])
      fireEvent.click(within(rowOf('Dub')).getByRole('button', { name: 'Filter by #drone' }))
      expect(screen.getByRole('searchbox')).toHaveValue('')
      // The prompt "droney raw" contains the word but not the tag.
      expect(rowTitles()).toEqual(['Dub'])
      const chip = within(rowOf('Dub')).getByRole('button', { name: 'Filter by #drone' })
      expect(chip).toHaveAttribute('aria-pressed', 'true')
      fireEvent.click(chip)
      expect(rowTitles()).toEqual(['Raw', 'Dub'])
    })

    const filterBar = () => within(screen.getByRole('group', { name: 'Filter by tag' }))
    const barChips = () =>
      filterBar()
        .getAllByRole('button', { name: /^Filter by/ })
        .map((chip) => chip.textContent)

    it('offers the tab’s tags above the list; several selected must all match; Clear resets', async () => {
      tagShell()
      renderExplorer()
      await openGenerate('Dub')
      // Songs' tags only — the samples' #riser is not offered here.
      expect(barChips()).toEqual(['#ambient', '#drone', '#dub'])
      fireEvent.click(filterBar().getByRole('button', { name: 'Filter by #dub' }))
      expect(rowTitles()).toEqual(['Dub'])
      fireEvent.click(filterBar().getByRole('button', { name: 'Filter by #ambient' }))
      expect(rowTitles()).toEqual([])
      expect(screen.getByRole('status')).toHaveTextContent('No results for “#dub #ambient”.')
      fireEvent.click(filterBar().getByRole('button', { name: 'Clear tags' }))
      expect(rowTitles()).toEqual(['Raw', 'Dub'])
      expect(filterBar().queryByRole('button', { name: 'Clear tags' })).toBeNull()
    })

    it('filters the samples tab by its own tags', async () => {
      tagShell()
      renderExplorer()
      fireEvent.click(screen.getByRole('tab', { name: 'Samples' }))
      await screen.findByText('Riser', { selector: '.media__name-text' })
      expect(barChips()).toEqual(['#riser'])
      fireEvent.click(filterBar().getByRole('button', { name: 'Filter by #riser' }))
      expect(rowTitles()).toEqual(['Riser'])
    })

    it('walks only the filtered rows with the controller', async () => {
      tagShell()
      const bus = createControlBus()
      const onLoadTrack = vi.fn(async () => true)
      renderExplorer({ onLoadTrack }, [], bus)
      await openGenerate('Dub')
      fireEvent.click(filterBar().getByRole('button', { name: 'Filter by #drone' }))
      await act(async () => {
        bus.publish({ kind: 'browse_load', deck: 'a' })
      })
      expect(onLoadTrack).toHaveBeenCalledWith('a', { kind: 'song', name: 'dub.wav' }, 'Dub #1')
    })

    it('matches a plain word against tags and prompt text alike', async () => {
      tagShell()
      renderExplorer()
      await openGenerate('Dub')
      fireEvent.change(screen.getByRole('searchbox'), { target: { value: 'drone' } })
      expect(rowTitles()).toEqual(['Raw', 'Dub'])
    })

    it('offers the vocabulary of both libraries alphabetically, with the row’s tags checked', async () => {
      tagShell()
      renderExplorer()
      await openGenerate('Dub')
      await chooseMenuItem('Dub #1', 'Tags…')
      const panel = within(screen.getByRole('dialog', { name: 'Tags for Dub #1' }))
      expect(panel.getAllByRole('checkbox').map((box) => box.closest('label')!.textContent)).toEqual([
        '#ambient',
        '#drone',
        '#dub',
        '#riser',
      ])
      expect(panel.getByRole('checkbox', { name: '#drone' })).toBeChecked()
      expect(panel.getByRole('checkbox', { name: '#dub' })).toBeChecked()
      expect(panel.getByRole('checkbox', { name: '#ambient' })).not.toBeChecked()
    })

    it('sends the whole list on a toggle and adopts the shell’s tags', async () => {
      const shell = tagShell((tags) => ({ ...tagged, tags: [...tags].sort() }))
      renderExplorer()
      await openGenerate('Dub')
      await chooseMenuItem('Dub #1', 'Tags…')
      await act(async () => {
        fireEvent.click(screen.getByRole('checkbox', { name: '#ambient' }))
      })
      await act(async () => {
        fireEvent.click(screen.getByRole('checkbox', { name: '#dub' }))
      })
      expect(shell.commands('tag_generated_song').map((call) => call.args)).toEqual([
        { name: 'dub.wav', tags: ['dub', 'drone', 'ambient'] },
        { name: 'dub.wav', tags: ['ambient', 'drone'] },
      ])
      expect(chipsOf('Dub')).toEqual(['#ambient', '#drone'])
      // No row carries #dub any more, so it leaves the vocabulary.
      expect(screen.queryByRole('checkbox', { name: '#dub' })).toBeNull()
    })

    it('adds a typed tag on Enter and shows the normalised result', async () => {
      const shell = tagShell(() => ({ ...tagged, tags: ['dub', 'drone', 'deep-house'] }))
      renderExplorer()
      await openGenerate('Dub')
      await chooseMenuItem('Dub #1', 'Tags…')
      const field = screen.getByRole('textbox', { name: 'Add a tag' })
      fireEvent.change(field, { target: { value: '  Deep House ' } })
      await act(async () => {
        fireEvent.keyDown(field, { key: 'Enter' })
      })
      expect(shell.commands('tag_generated_song')[0].args).toEqual({
        name: 'dub.wav',
        tags: ['dub', 'drone', 'Deep House'],
      })
      expect(chipsOf('Dub')).toEqual(['#dub', '#drone', '#deep-house'])
    })

    it('shows a failed tag and re-lists to resync', async () => {
      const shell = tagShell(() => Promise.reject(new Error('too many tags')))
      renderExplorer()
      await openGenerate('Dub')
      await chooseMenuItem('Dub #1', 'Tags…')
      await act(async () => {
        fireEvent.click(screen.getByRole('checkbox', { name: '#riser' }))
      })
      expect(screen.getByRole('alert')).toHaveTextContent("Couldn't tag Dub: too many tags")
      expect(shell.commands('list_generated_songs')).toHaveLength(2)
      expect(chipsOf('Dub')).toEqual(['#dub', '#drone'])
    })

    it('tags a sample through the samples command', async () => {
      const shell = stubShell(
        () => [],
        (cmd, args) => (cmd === 'tag_generated_sample' ? { ...riser, tags: args.tags } : undefined),
        () => [riser],
      )
      renderExplorer()
      fireEvent.click(screen.getByRole('tab', { name: 'Samples' }))
      await chooseMenuItem('Riser #1', 'Tags…')
      await act(async () => {
        fireEvent.click(screen.getByRole('checkbox', { name: '#riser' }))
      })
      expect(shell.commands('tag_generated_sample')[0].args).toEqual({ name: 'riser.wav', tags: [] })
    })

    it('opens the highlighted row’s panel on the add field with t, not while typing', async () => {
      tagShell()
      renderExplorer()
      await openGenerate('Dub')
      fireEvent.keyDown(screen.getByRole('searchbox'), { key: 't' })
      expect(screen.queryByRole('dialog')).toBeNull()
      fireEvent.click(screen.getByRole('button', { name: 'Select Dub #1' }))
      fireEvent.keyDown(window, { key: 't' })
      expect(screen.getByRole('dialog', { name: 'Tags for Dub #1' })).toBeInTheDocument()
      expect(screen.getByRole('textbox', { name: 'Add a tag' })).toHaveFocus()
    })
  })

  describe('playlists', () => {
    type Playlist = { name: string; files: string[] }
    // Registry order is oldest-first, so All songs reads Gamma, Beta, Alpha.
    const library: Song[] = [
      { file: 'a.wav', title: 'Alpha', prompt: null, model: null },
      { file: 'b.wav', title: 'Beta', prompt: null, model: null },
      { file: 'c.wav', title: 'Gamma', prompt: null, model: null },
    ]
    const set: Playlist = { name: 'Set', files: ['a.wav', 'c.wav', 'b.wav'] }

    /** The curation shell plus a mutable playlist store that answers like the Rust
     * one: every write returns the stored playlist, a new name comes back sanitised. */
    function playlistShell(
      initial: Playlist[],
      respond: (cmd: string, args: Record<string, unknown>) => unknown = () => undefined,
    ) {
      let lists = initial
      const shell = stubShell(
        () => library,
        (cmd, args) => {
          const answer = respond(cmd, args)
          if (answer !== undefined) return answer
          if (cmd === 'list_playlists') return lists
          if (cmd === 'save_playlist') {
            const stored = {
              name: (args.name as string).replaceAll('/', '-'),
              files: args.files as string[],
            }
            lists = [...lists.filter((entry) => entry.name !== stored.name), stored]
            return stored
          }
          if (cmd === 'rename_playlist') {
            const from = lists.find((entry) => entry.name === args.from)!
            const stored = { ...from, name: args.to as string }
            lists = [...lists.filter((entry) => entry !== from), stored]
            return stored
          }
          if (cmd === 'delete_playlist') {
            lists = lists.filter((entry) => entry.name !== args.name)
            return null
          }
          return undefined
        },
      )
      return { ...shell, setLists: (next: Playlist[]) => (lists = next) }
    }

    const sources = () => within(screen.getByRole('navigation', { name: 'Sources' }))
    const sourceButton = (name: string) =>
      sources().getByRole('button', { name: new RegExp(`^${name}( \\d+ songs?)?$`) })
    async function openPlaylist(name: string) {
      await openGenerate('Gamma')
      await act(async () => {
        fireEvent.click(await sources().findByRole('button', { name: new RegExp(`^${name} `) }))
      })
    }
    const saves = (shell: ReturnType<typeof playlistShell>) =>
      shell.commands('save_playlist').map((call) => call.args)
    const dataTransfer = (file: string) => ({ getData: () => file, setData: vi.fn() })

    it('names the tab Songs and lists the playlists in the source column', async () => {
      playlistShell([set, { name: 'Warm-up', files: ['b.wav'] }])
      renderExplorer()
      expect(screen.getByRole('tab', { name: 'Songs' })).toBeInTheDocument()
      await openGenerate('Gamma')
      await sources().findByRole('button', { name: /^Warm-up / })
      expect(sources().getAllByRole('button').map((button) => button.textContent)).toEqual([
        'All songs',
        'Set 3 songs',
        'Warm-up 1 song',
        'New playlist',
      ])
      expect(sourceButton('All songs')).toHaveAttribute('aria-pressed', 'true')
      // The narrow-width stand-in offers the same choices.
      expect(
        [...(screen.getByLabelText('Source') as HTMLSelectElement).options].map(
          (option) => option.text,
        ),
      ).toEqual(['All songs', 'Set', 'Warm-up'])
    })

    it('shows a playlist’s songs in its order, without pending rows, and the rotary walks it', async () => {
      playlistShell([set])
      vi.stubGlobal('fetch', vi.fn(() => new Promise(() => {})))
      const bus = createControlBus()
      const onLoadTrack = vi.fn(async () => true)
      renderExplorer({ onLoadTrack }, [], bus)
      await composeTrack('wip')
      await openPlaylist('Set')
      expect(rowTitles()).toEqual(['Alpha', 'Gamma', 'Beta'])
      expect(screen.queryByText('wip — composing…')).toBeNull()
      expect(screen.getByText('Set', { selector: '.media__playlist-name' })).toBeInTheDocument()
      expect(JSON.parse(localStorage.getItem('lsdj:v1')!).app.mediaSource).toBe('Set')
      act(() => bus.publish({ kind: 'browse_scroll', steps: 1 }))
      await act(async () => {
        bus.publish({ kind: 'browse_load', deck: 'b' })
      })
      expect(onLoadTrack).toHaveBeenCalledWith('b', { kind: 'song', name: 'c.wav' }, 'Gamma')
    })

    it('selects a playlist from the narrow-width Select too', async () => {
      playlistShell([set])
      renderExplorer()
      await openGenerate('Gamma')
      await sources().findByRole('button', { name: /^Set / })
      fireEvent.change(screen.getByLabelText('Source'), { target: { value: 'Set' } })
      expect(rowTitles()).toEqual(['Alpha', 'Gamma', 'Beta'])
      expect(sourceButton('Set')).toHaveAttribute('aria-pressed', 'true')
    })

    it('narrows a playlist with a search', async () => {
      playlistShell([set])
      renderExplorer()
      await openPlaylist('Set')
      fireEvent.change(screen.getByRole('searchbox'), { target: { value: 'ga' } })
      expect(rowTitles()).toEqual(['Gamma'])
    })

    it('defaults to Playlist order, and a Title sort reorders the view without saving or persisting', async () => {
      const shell = playlistShell([set])
      renderExplorer()
      await openPlaylist('Set')
      expect(screen.getByLabelText('Sort')).toHaveValue('playlist')
      fireEvent.change(screen.getByLabelText('Sort'), { target: { value: 'title' } })
      expect(rowTitles()).toEqual(['Alpha', 'Beta', 'Gamma'])
      expect(saves(shell)).toEqual([])
      expect(JSON.parse(localStorage.getItem('lsdj:v1')!).app.mediaSort).toBeUndefined()
      // Back in All songs the library sort applies, without Playlist order.
      fireEvent.click(sourceButton('All songs'))
      expect(screen.getByLabelText('Sort')).toHaveValue('newest')
      expect(rowTitles()).toEqual(['Gamma', 'Beta', 'Alpha'])
    })

    it('shows the count and total length in the header, and an empty playlist’s hint', async () => {
      const recipe = (seconds: number) => ({ version: 1, engine: 'track', prompt: 'x', seconds, loras: [] })
      stubShell(
        () => [
          { ...library[0], recipe: recipe(60) },
          { ...library[1], recipe: recipe(125) },
          library[2],
        ],
        (cmd) =>
          cmd === 'list_playlists'
            ? [
                { name: 'Timed', files: ['a.wav', 'b.wav'] },
                { name: 'Empty', files: [] },
              ]
            : undefined,
      )
      renderExplorer()
      await openPlaylist('Timed')
      expect(screen.getByText('2 songs · 3:05')).toBeInTheDocument()
      fireEvent.click(sourceButton('Empty'))
      expect(
        screen.getByText('Drag songs here, or use ⋯ → Add to playlist.'),
      ).toBeInTheDocument()
    })

    it('✕ removes a song from the playlist and never deletes it', async () => {
      const shell = playlistShell([set])
      renderExplorer()
      await openPlaylist('Set')
      await act(async () => {
        fireEvent.click(screen.getByRole('button', { name: 'Remove Gamma from Set' }))
      })
      expect(saves(shell)).toEqual([{ name: 'Set', files: ['a.wav', 'b.wav'] }])
      expect(shell.commands('delete_generated_song')).toEqual([])
      expect(rowTitles()).toEqual(['Alpha', 'Beta'])
      expect(sourceButton('Set')).toHaveTextContent('Set 2 songs')
    })

    it('Remove from playlist in the menu does the same', async () => {
      const shell = playlistShell([set])
      renderExplorer()
      await openPlaylist('Set')
      await chooseMenuItem('Alpha', 'Remove from playlist')
      await act(async () => {})
      expect(saves(shell)).toEqual([{ name: 'Set', files: ['c.wav', 'b.wav'] }])
      expect(shell.commands('delete_generated_song')).toEqual([])
    })

    it('shows no ✕ outside a playlist', async () => {
      playlistShell([set])
      renderExplorer()
      await openGenerate('Gamma')
      expect(screen.queryByRole('button', { name: /^Remove .* from / })).toBeNull()
    })

    it('Delete from library still deletes, then re-lists the playlists', async () => {
      const shell = playlistShell([set])
      renderExplorer()
      await openPlaylist('Set')
      // The shell strips the file from every playlist on delete.
      shell.setLists([{ name: 'Set', files: ['a.wav', 'b.wav'] }])
      await chooseMenuItem('Gamma', 'Delete from library')
      await act(async () => {})
      expect(shell.commands('delete_generated_song').map((call) => call.args)).toEqual([
        { name: 'c.wav' },
      ])
      expect(shell.commands('list_playlists')).toHaveLength(2)
      expect(sourceButton('Set')).toHaveTextContent('Set 2 songs')
    })

    it('Move up / Move down save the reordered list and are disabled at the ends', async () => {
      const shell = playlistShell([set])
      renderExplorer()
      await openPlaylist('Set')
      fireEvent.click(screen.getByRole('button', { name: 'More actions for Alpha' }))
      expect(screen.getByRole('menuitem', { name: 'Move up' })).toBeDisabled()
      await act(async () => {
        fireEvent.click(screen.getByRole('menuitem', { name: 'Move down' }))
      })
      expect(rowTitles()).toEqual(['Gamma', 'Alpha', 'Beta'])
      fireEvent.click(screen.getByRole('button', { name: 'More actions for Beta' }))
      expect(screen.getByRole('menuitem', { name: 'Move down' })).toBeDisabled()
      await act(async () => {
        fireEvent.click(screen.getByRole('menuitem', { name: 'Move up' }))
      })
      expect(saves(shell)).toEqual([
        { name: 'Set', files: ['c.wav', 'a.wav', 'b.wav'] },
        { name: 'Set', files: ['c.wav', 'b.wav', 'a.wav'] },
      ])
    })

    it('Add to playlist… checks the playlists holding the song and toggles with the whole list', async () => {
      const shell = playlistShell([set, { name: 'Warm-up', files: ['c.wav'] }])
      renderExplorer()
      await openGenerate('Gamma')
      await sources().findByRole('button', { name: /^Warm-up / })
      await chooseMenuItem('Beta', 'Add to playlist…')
      const panel = within(screen.getByRole('dialog', { name: 'Playlists for Beta' }))
      expect(panel.getByRole('checkbox', { name: 'Set' })).toBeChecked()
      expect(panel.getByRole('checkbox', { name: 'Warm-up' })).not.toBeChecked()
      await act(async () => {
        fireEvent.click(panel.getByRole('checkbox', { name: 'Warm-up' }))
      })
      await act(async () => {
        fireEvent.click(panel.getByRole('checkbox', { name: 'Set' }))
      })
      expect(saves(shell)).toEqual([
        { name: 'Warm-up', files: ['c.wav', 'b.wav'] },
        { name: 'Set', files: ['a.wav', 'c.wav'] },
      ])
      expect(panel.getByRole('checkbox', { name: 'Warm-up' })).toBeChecked()
      expect(panel.getByRole('checkbox', { name: 'Set' })).not.toBeChecked()
    })

    it('creates a playlist holding just the song from the panel, adopting the sanitised name', async () => {
      const shell = playlistShell([])
      renderExplorer()
      await openGenerate('Gamma')
      await chooseMenuItem('Beta', 'Add to playlist…')
      expect(screen.getByText('No playlists yet — name one below.')).toBeInTheDocument()
      const field = screen.getByRole('textbox', { name: 'New playlist' })
      fireEvent.change(field, { target: { value: 'Warm/up' } })
      await act(async () => {
        fireEvent.keyDown(field, { key: 'Enter' })
      })
      expect(saves(shell)).toEqual([{ name: 'Warm/up', files: ['b.wav'] }])
      expect(sourceButton('Warm-up')).toHaveTextContent('Warm-up 1 song')
      expect(sources().queryByRole('button', { name: /Warm\/up/ })).toBeNull()
    })

    it('adds to an existing playlist rather than replacing it when the panel names one', async () => {
      const shell = playlistShell([set])
      renderExplorer()
      await openGenerate('Gamma')
      await sources().findByRole('button', { name: /^Set / })
      fireEvent.click(sourceButton('All songs'))
      shell.setLists([{ name: 'Set', files: ['a.wav'] }])
      await act(async () => {
        shell.changed('playlists')
      })
      await chooseMenuItem('Beta', 'Add to playlist…')
      const field = screen.getByRole('textbox', { name: 'New playlist' })
      fireEvent.change(field, { target: { value: 'set' } })
      await act(async () => {
        fireEvent.keyDown(field, { key: 'Enter' })
      })
      expect(saves(shell)).toEqual([{ name: 'Set', files: ['a.wav', 'b.wav'] }])
    })

    it('offers no Add to playlist… on a song not saved to the library', async () => {
      stubFetch()
      renderExplorer()
      await composeTrack('fresh')
      fireEvent.click(await screen.findByRole('button', { name: 'More actions for fresh #1' }))
      expect(screen.getByRole('menuitem', { name: 'Rename' })).toBeInTheDocument()
      expect(screen.queryByRole('menuitem', { name: 'Add to playlist…' })).toBeNull()
    })

    it('offers no Add to playlist… on a sample', async () => {
      const riser = { file: 'riser.wav', title: 'Riser', prompt: null, model: 'sfx', oneShot: true }
      stubShell(() => [], undefined, () => [riser])
      renderExplorer()
      fireEvent.click(screen.getByRole('tab', { name: 'Samples' }))
      fireEvent.click(await screen.findByRole('button', { name: 'More actions for Riser' }))
      expect(screen.getByRole('menuitem', { name: 'Rename' })).toBeInTheDocument()
      expect(screen.queryByRole('menuitem', { name: 'Add to playlist…' })).toBeNull()
    })

    it('drags a saved song as a whole row: dragstart carries the file', async () => {
      playlistShell([set])
      renderExplorer()
      await openGenerate('Gamma')
      const transfer = dataTransfer('')
      const row = screen.getByRole('button', { name: 'Select Beta' }).closest('li')!
      fireEvent.dragStart(row, { dataTransfer: transfer })
      // The row is the drag source — WebKit ignores `draggable` on a button.
      expect(row).toHaveAttribute('draggable', 'true')
      expect(screen.getByRole('button', { name: 'Select Beta' })).not.toHaveAttribute('draggable')
      expect(transfer.setData).toHaveBeenCalledWith('text/plain', 'b.wav')
    })

    it('shows a reorder grip only in a playlist shown in its stored order', async () => {
      playlistShell([set])
      renderExplorer()
      await openGenerate('Gamma')
      expect(document.querySelectorAll('.media__grip')).toHaveLength(0)
      await openPlaylist('Set')
      expect(document.querySelectorAll('.media__grip')).toHaveLength(3)
      expect(document.querySelector('.media__grip')).toHaveAttribute(
        'title',
        'Drag to reorder, or use ⋯ → Move up / Move down',
      )
      // A rating/title sort is a view of the playlist, not its order: no grip.
      fireEvent.change(screen.getByLabelText('Sort'), { target: { value: 'title' } })
      expect(document.querySelectorAll('.media__grip')).toHaveLength(0)
    })

    it('a drop on a playlist in the column appends the song once', async () => {
      const shell = playlistShell([{ name: 'Warm-up', files: ['c.wav'] }])
      renderExplorer()
      await openGenerate('Gamma')
      const target = await sources().findByRole('button', { name: /^Warm-up / })
      fireEvent.dragOver(target, { dataTransfer: dataTransfer('a.wav') })
      expect(target.closest('.media__source')).toHaveClass('media__source--drop')
      await act(async () => {
        fireEvent.drop(target, { dataTransfer: dataTransfer('a.wav') })
      })
      expect(target.closest('.media__source')).not.toHaveClass('media__source--drop')
      await act(async () => {
        fireEvent.drop(sourceButton('Warm-up'), { dataTransfer: dataTransfer('a.wav') })
      })
      // Text that isn't a library song is ignored too.
      await act(async () => {
        fireEvent.drop(sourceButton('Warm-up'), { dataTransfer: dataTransfer('elsewhere') })
      })
      expect(saves(shell)).toEqual([{ name: 'Warm-up', files: ['c.wav', 'a.wav'] }])
    })

    it('a drop on a row inside a playlist reorders and saves', async () => {
      const shell = playlistShell([set])
      renderExplorer()
      await openPlaylist('Set')
      const alphaRow = screen.getByRole('button', { name: 'Select Alpha' }).closest('li')!
      fireEvent.dragOver(alphaRow, { dataTransfer: dataTransfer('b.wav') })
      expect(alphaRow).toHaveClass('media__item--drop-before')
      await act(async () => {
        fireEvent.drop(alphaRow, { dataTransfer: dataTransfer('b.wav') })
      })
      expect(saves(shell)).toEqual([{ name: 'Set', files: ['b.wav', 'a.wav', 'c.wav'] }])
      expect(rowTitles()).toEqual(['Beta', 'Alpha', 'Gamma'])
      expect(alphaRow).not.toHaveClass('media__item--drop-before')
    })

    it('creates a playlist from the column and selects it', async () => {
      const shell = playlistShell([])
      renderExplorer()
      await openGenerate('Gamma')
      fireEvent.click(sourceButton('New playlist'))
      const field = screen.getByRole('textbox', { name: 'Playlist name' })
      fireEvent.change(field, { target: { value: 'Peak' } })
      await act(async () => {
        fireEvent.keyDown(field, { key: 'Enter' })
      })
      expect(saves(shell)).toEqual([{ name: 'Peak', files: [] }])
      expect(sourceButton('Peak')).toHaveAttribute('aria-pressed', 'true')
      expect(screen.getByText('Drag songs here, or use ⋯ → Add to playlist.')).toBeInTheDocument()
    })

    it('Escape or a blank name cancels a new playlist', async () => {
      const shell = playlistShell([])
      renderExplorer()
      await openGenerate('Gamma')
      fireEvent.click(sourceButton('New playlist'))
      const field = screen.getByRole('textbox', { name: 'Playlist name' })
      fireEvent.change(field, { target: { value: 'Peak' } })
      fireEvent.keyDown(field, { key: 'Escape' })
      fireEvent.click(sourceButton('New playlist'))
      fireEvent.keyDown(screen.getByRole('textbox', { name: 'Playlist name' }), { key: 'Enter' })
      expect(saves(shell)).toEqual([])
      expect(sourceButton('New playlist')).toBeInTheDocument()
    })

    it('renames the open playlist from its menu, following it in the column and the selection', async () => {
      const shell = playlistShell([set])
      renderExplorer()
      await openPlaylist('Set')
      fireEvent.click(screen.getByRole('button', { name: 'Playlist actions' }))
      fireEvent.click(screen.getByRole('menuitem', { name: 'Rename playlist' }))
      const field = screen.getByRole('textbox', { name: 'Playlist name' })
      fireEvent.change(field, { target: { value: 'Closing set' } })
      await act(async () => {
        fireEvent.keyDown(field, { key: 'Enter' })
      })
      expect(shell.commands('rename_playlist').map((call) => call.args)).toEqual([
        { from: 'Set', to: 'Closing set' },
      ])
      expect(sourceButton('Closing set')).toHaveAttribute('aria-pressed', 'true')
      expect(sources().queryByRole('button', { name: /^Set / })).toBeNull()
      expect(rowTitles()).toEqual(['Alpha', 'Gamma', 'Beta'])
      expect(JSON.parse(localStorage.getItem('lsdj:v1')!).app.mediaSource).toBe('Closing set')
    })

    it('a double-click on the header name renames too; unchanged is a cancel', async () => {
      const shell = playlistShell([set])
      renderExplorer()
      await openPlaylist('Set')
      fireEvent.doubleClick(screen.getByText('Set', { selector: '.media__playlist-name' }))
      fireEvent.keyDown(screen.getByRole('textbox', { name: 'Playlist name' }), { key: 'Enter' })
      expect(shell.commands('rename_playlist')).toEqual([])
    })

    it('deletes the open playlist and returns to All songs', async () => {
      const shell = playlistShell([set])
      renderExplorer()
      await openPlaylist('Set')
      fireEvent.click(screen.getByRole('button', { name: 'Playlist actions' }))
      await act(async () => {
        fireEvent.click(screen.getByRole('menuitem', { name: 'Delete playlist' }))
      })
      expect(shell.commands('delete_playlist').map((call) => call.args)).toEqual([{ name: 'Set' }])
      expect(sourceButton('All songs')).toHaveAttribute('aria-pressed', 'true')
      expect(sources().queryByRole('button', { name: /^Set / })).toBeNull()
      expect(rowTitles()).toEqual(['Gamma', 'Beta', 'Alpha'])
    })

    it('falls back to All songs when the persisted playlist is gone', async () => {
      localStorage.setItem('lsdj:v1', JSON.stringify({ app: { mediaSource: 'Lost' } }))
      playlistShell([set])
      renderExplorer()
      await openGenerate('Gamma')
      await sources().findByRole('button', { name: /^Set / })
      expect(sourceButton('All songs')).toHaveAttribute('aria-pressed', 'true')
      expect(rowTitles()).toEqual(['Gamma', 'Beta', 'Alpha'])
    })

    it('re-lists on a library://changed playlists event', async () => {
      const shell = playlistShell([])
      renderExplorer()
      await openGenerate('Gamma')
      shell.setLists([{ name: 'Agent set', files: ['a.wav'] }])
      await act(async () => {
        shell.changed('playlists')
      })
      expect(sourceButton('Agent set')).toHaveTextContent('Agent set 1 song')
    })

    it('shows a failed save and re-lists', async () => {
      const shell = playlistShell([set], (cmd) =>
        cmd === 'save_playlist' ? Promise.reject(new Error('disk full')) : undefined,
      )
      renderExplorer()
      await openPlaylist('Set')
      await act(async () => {
        fireEvent.click(screen.getByRole('button', { name: 'Remove Gamma from Set' }))
      })
      expect(screen.getByRole('alert')).toHaveTextContent(
        "Couldn't save playlist Set: disk full",
      )
      expect(shell.commands('list_playlists')).toHaveLength(2)
      expect(rowTitles()).toEqual(['Alpha', 'Gamma', 'Beta'])
    })
  })

  it('rebuilds a re-listed row from the registry, keeping its id', async () => {
    let songs: Song[] = [{ ...dub, rating: undefined }]
    const shell = stubShell(() => songs)
    renderExplorer()
    await openGenerate('Dub')
    expect(screen.getByText('#1')).toBeInTheDocument()

    // An out-of-band write (an MCP rating, a hand edit) lands; the watcher fires.
    songs = [{ ...dub, title: 'Dub v2', rating: 4 }]
    await act(async () => {
      shell.changed('songs')
    })
    expect(rowTitles()).toEqual(['Dub v2'])
    expect(screen.getByText('#1')).toBeInTheDocument()
    expect(ratingOf('Dub v2 #1').getByRole('radio', { name: '4 stars' })).toBeChecked()
  })
})

describe('rotary inside the folded-in crates tab', () => {
  const preset = (name: string): StylePreset => ({
    name,
    targets: [{ x: 0.5, y: 0.5, text: 'funk' }],
    cursor: { x: 0.5, y: 0.5 },
    fx: { kind: null, amount: 0 },
  })

  it('scrolls the crate highlight and quick-loads it', () => {
    const bus = createControlBus()
    const onLoadPreset = vi.fn()
    renderExplorer({ onLoadPreset }, [preset('one'), preset('two')], bus)
    act(() => bus.publish({ kind: 'browse_scroll', steps: 1 }))
    expect(
      screen.getByRole('button', { name: 'Select preset two' }),
    ).toHaveAttribute('aria-current', 'true')
    act(() => bus.publish({ kind: 'browse_load', deck: 'a' }))
    expect(onLoadPreset).toHaveBeenCalledWith('a', preset('two'))
  })
})
