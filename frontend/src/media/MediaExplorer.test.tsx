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
  fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
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
    // The row names the model that produced the take (the same label
    // also lives in the engine dropdown, hence the class filter).
    expect(
      screen
        .getAllByText('Track (SA3 medium)')
        .some((element) => element.classList.contains('media__meta')),
    ).toBe(true)
  })

  it('persists Basic/Advanced mode and restores the hidden Advanced draft', () => {
    renderExplorer()
    fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
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
    fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
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
    fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
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
    fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
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
    fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
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
    fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
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
    fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
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
    fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
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
          { file: 'Freeze A.wav', title: 'Freeze A', prompt: null, model: 'freeze', oneShot: false },
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
    expect(metas.some((text) => text.includes('Freeze'))).toBe(true)
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
    fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
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
    fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
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
    fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
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
    fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
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
    fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
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
    expect(screen.queryByRole('button', { name: /Reuse generation settings/ })).toBeNull()
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
    fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
    fireEvent.click(
      await screen.findByRole('button', {
        name: 'Reuse generation settings from Basic keeper #1',
      }),
    )

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
    fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
    fireEvent.change(screen.getByLabelText('Title'), { target: { value: 'Keep title' } })
    fireEvent.click(
      await screen.findByRole('button', {
        name: 'Reuse generation settings from Recipe take #1',
      }),
    )

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
    fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
    fireEvent.change(screen.getByLabelText('Track prompt'), {
      target: { value: 'current draft' },
    })
    fireEvent.click(
      await screen.findByRole('button', {
        name: 'Reuse generation settings from Future #1',
      }),
    )
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
    fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
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

  it('deletes a take via ✕, moving the file to the Trash and pruning the registry', async () => {
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
    fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
    const removeButton = await screen.findByRole('button', { name: 'Remove keeper #1' })
    await act(async () => {
      fireEvent.click(removeButton)
    })
    expect(screen.queryByRole('button', { name: 'Remove keeper #1' })).toBeNull()
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
    fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
    const removeButton = await screen.findByRole('button', { name: 'Remove keeper #1' })
    await act(async () => {
      fireEvent.click(removeButton)
    })
    // The disk delete failed, so the row stays (matching disk) and the error shows —
    // it must not vanish and then reappear on the next launch's scan.
    expect(screen.getByRole('button', { name: 'Remove keeper #1' })).toBeInTheDocument()
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
    fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
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
    fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
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
    fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
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
    fireEvent.click(screen.getByRole('tab', { name: 'Generate' }))
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
