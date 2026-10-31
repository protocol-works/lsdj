/** App-level wiring tests with a fake engine: the crossfade and cue-mix
 * chains (audio bus + persistence) are owned by App and must hold from
 * both the on-screen control and the hardware intent path. */

import { act, fireEvent, render, screen, within } from '@testing-library/react'
import { createElement, type ComponentProps } from 'react'
import { describe, expect, it, vi } from 'vitest'

import App from './App'
import { AudioEngineProvider } from './audio/AudioEngineProvider'
import type { AudioEngine } from './audio/types'
import { applyAppIntent } from './control/appIntents'
import { createControlBus, type ControlBus } from './control/bus'
import { ControlBusProvider } from './control/ControlBusProvider'
import type { MediaExplorer } from './media/MediaExplorer'
import { loadAppSettings, updateAppSettings } from './persistence'

// The LSDJ brand mark renders through three.js / react-three-fiber, which needs
// a real WebGL context and ResizeObserver — neither exists in jsdom. These
// tests exercise App's crossfade/cue-mix wiring, not the logo, so stub it out.
vi.mock('./ui/HypercubeMark', () => ({ HypercubeMark: () => null }))

// Pass-throughs that let a test see what App hands on: the intents it
// dispatches to the decks, and the explorer's props (its →A/→B load path).
vi.mock('./control/appIntents', async (importOriginal) => {
  const original = await importOriginal<typeof import('./control/appIntents')>()
  return { ...original, applyAppIntent: vi.fn(original.applyAppIntent) }
})
const explorer = vi.hoisted(() => ({ props: null as ComponentProps<typeof MediaExplorer> | null }))
vi.mock('./media/MediaExplorer', async (importOriginal) => {
  const original = await importOriginal<typeof import('./media/MediaExplorer')>()
  return {
    ...original,
    MediaExplorer: (props: ComponentProps<typeof MediaExplorer>) => {
      explorer.props = props
      return createElement(original.MediaExplorer, props)
    },
  }
})
// No shell in jsdom: every command fails as it does without Tauri, except the
// two a song needs to become a loom node.
vi.mock('./audio/nativeEngine', async (importOriginal) => {
  const original = await importOriginal<typeof import('./audio/nativeEngine')>()
  const songs = [{ file: 'dub.wav', title: 'Dub', prompt: 'warm dub, 124 BPM', model: 'track', bpm: 124 }]
  return {
    ...original,
    invoke: (cmd: string) =>
      cmd === 'list_generated_songs'
        ? Promise.resolve(songs)
        : cmd === 'library_clip_peaks'
          ? Promise.resolve({ min: [], max: [], duration: 40 })
          : Promise.reject(new Error('Tauri IPC unavailable')),
  }
})

function makeEngine(): AudioEngine {
  return {
    createDeckChannel: vi.fn(),
    resume: vi.fn(async () => {}),
    getContextTime: vi.fn(() => 0),
    setCrossfade: vi.fn(),
    setCueMix: vi.fn(),
    auditionPlay: vi.fn(async () => {}),
    auditionStop: vi.fn(),
    listOutputDevices: vi.fn(async () => []),
    setMainDevice: vi.fn(async () => {}),
    setCueDevice: vi.fn(async () => {}),
    startRecording: vi.fn(async () => '/Downloads/lsdj-take.wav'),
    stopRecording: vi.fn(async () => {}),
    getMasterLevel: vi.fn(() => 0),
    getMasterGainReduction: vi.fn(() => 0),
  }
}

function renderApp(engine: AudioEngine, bus: ControlBus = createControlBus()) {
  return render(
    <AudioEngineProvider engine={engine}>
      <ControlBusProvider bus={bus}>
        <App />
      </ControlBusProvider>
    </AudioEngineProvider>,
  )
}

describe('App crossfade ownership', () => {
  // Persistence assertions are gone with the localStorage slot: the engine
  // move records into the Rust store, and the SHELL persists that (ADR-0020
  // phase C — settings::watch_persistence).
  it('a slider move drives the audio bus', () => {
    const engine = makeEngine()
    renderApp(engine)
    vi.mocked(engine.setCrossfade).mockClear()

    fireEvent.change(screen.getByLabelText('Crossfade'), {
      target: { value: '0.2' },
    })

    expect(engine.setCrossfade).toHaveBeenCalledWith(0.2)
  })

  it('a cue-mix move drives the engine', () => {
    const engine = makeEngine()
    renderApp(engine)

    fireEvent.change(screen.getByLabelText('Cue mix'), {
      target: { value: '0.3' },
    })

    expect(engine.setCueMix).toHaveBeenLastCalledWith(0.3)
  })

  it('a hardware cue-mix intent flows through the same chain', () => {
    const engine = makeEngine()
    const bus = createControlBus()
    renderApp(engine, bus)

    act(() => bus.publish({ kind: 'cue_mix', value: 0.8 }))

    expect(engine.setCueMix).toHaveBeenLastCalledWith(0.8)
  })

  it('a hardware crossfade intent flows through the same chain', () => {
    const engine = makeEngine()
    const bus = createControlBus()
    renderApp(engine, bus)
    vi.mocked(engine.setCrossfade).mockClear()

    act(() => bus.publish({ kind: 'crossfade', value: 0.75 }))

    expect(engine.setCrossfade).toHaveBeenCalledWith(0.75)
    expect(screen.getByLabelText('Crossfade')).toHaveValue('0.75')
  })
})

// The DJ booth is ~640 nodes with ~90 labelable controls, and after any DOM
// change jsdom rebuilds every control's live `labels` list by walking the
// whole document: a `*ByLabelText` there costs ~200 ms (seconds under load).
// The drawer and theme checks ask by role and name, which names only the
// candidates of that role.
describe('App settings drawer', () => {
  it('hosts a per-deck model picker (moved out of the deck columns)', () => {
    const engine = makeEngine()
    renderApp(engine)
    // The decks no longer carry a model picker; it lives in settings now.
    expect(screen.queryByRole('combobox', { name: 'Deck A' })).toBeNull()

    fireEvent.click(screen.getByRole('button', { name: 'Settings' }))

    const settings = within(screen.getByRole('dialog', { name: 'Settings' }))
    expect(settings.getByRole('combobox', { name: 'Deck A' })).toBeInTheDocument()
    expect(settings.getByRole('combobox', { name: 'Deck B' })).toBeInTheDocument()
  })

  it('defaults performance visuals on and persists the switch across remounts', () => {
    const engine = makeEngine()
    const first = renderApp(engine)
    fireEvent.click(screen.getByRole('button', { name: 'Settings' }))

    const toggle = screen.getByRole('switch', { name: 'Performance visuals' })
    const app = document.querySelector('.app')
    expect(toggle).toHaveAttribute('aria-checked', 'true')
    expect(app).toHaveAttribute('data-performance-visuals', 'on')
    expect(document.querySelector('.performance-visuals')).not.toHaveAttribute('hidden')

    fireEvent.click(toggle)
    expect(toggle).toHaveAttribute('aria-checked', 'false')
    expect(app).toHaveAttribute('data-performance-visuals', 'off')
    expect(document.querySelector('.performance-visuals')).toHaveAttribute('hidden')
    expect(loadAppSettings().performanceVisuals).toBe(false)

    first.unmount()
    renderApp(makeEngine())
    fireEvent.click(screen.getByRole('button', { name: 'Settings' }))
    expect(
      screen.getByRole('switch', { name: 'Performance visuals' }),
    ).toHaveAttribute('aria-checked', 'false')
  })
})

describe('App theme', () => {
  it('defaults to the DJ booth', () => {
    renderApp(makeEngine())

    expect(screen.getByRole('region', { name: 'Deck a' })).toBeInTheDocument()
    expect(screen.getByRole('region', { name: 'Deck b' })).toBeInTheDocument()
    expect(screen.queryByLabelText('Loom')).toBeNull()
  })

  it('picking Loom swaps the booth for the loom and persists the theme', () => {
    renderApp(makeEngine())
    fireEvent.click(screen.getByRole('button', { name: 'Settings' }))
    expect(screen.getByRole('radiogroup', { name: 'Theme' })).toBeInTheDocument()
    expect(screen.queryByRole('switch', { name: 'Render ahead' })).toBeNull()

    fireEvent.click(screen.getByRole('radio', { name: 'Loom' }))

    expect(screen.getByLabelText('Loom')).toBeInTheDocument()
    expect(screen.queryByRole('region', { name: 'Deck a' })).toBeNull()
    expect(screen.queryByRole('region', { name: 'Deck b' })).toBeNull()
    expect(document.documentElement.dataset.theme).toBe('loom')
    expect(loadAppSettings().theme).toBe('loom')
    expect(
      screen.getByRole('switch', { name: 'Render ahead' }),
    ).toHaveAttribute('aria-checked', 'true')
  })

  it('picking DJ restores the deck columns', () => {
    updateAppSettings({ theme: 'loom' })
    renderApp(makeEngine())
    expect(screen.getByRole('region', { name: 'Loom' })).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'Settings' }))

    fireEvent.click(screen.getByRole('radio', { name: 'DJ' }))

    expect(screen.queryByRole('region', { name: 'Loom' })).toBeNull()
    expect(screen.getByRole('region', { name: 'Deck a' })).toBeInTheDocument()
    expect(screen.getByRole('region', { name: 'Deck b' })).toBeInTheDocument()
    expect(loadAppSettings().theme).toBe('dj')
  })

  it('lands an explorer song load in the loom as a root node', async () => {
    updateAppSettings({ theme: 'loom' })
    renderApp(makeEngine())

    await act(async () => {
      await explorer.props!.onLoadTrack('a', { kind: 'song', name: 'dub.wav' }, 'Dub')
    })

    // The registry prompt, its tempo stamp dropped (a weave stamps its own).
    expect(screen.getByRole('group', { name: 'warm dub' })).toBeInTheDocument()
  })

  it('mounts the explorer as a drawer over the loom, whose seed button starts a thread', async () => {
    updateAppSettings({ theme: 'loom' })
    renderApp(makeEngine())
    const loom = screen.getByRole('region', { name: 'Loom' })
    expect(within(loom).getByRole('region', { name: 'Media explorer' })).toBeInTheDocument()

    await act(async () => {
      explorer.props!.onSeedTrack!('dub.wav')
    })

    expect(await screen.findByRole('group', { name: 'warm dub' })).toBeInTheDocument()
  })

  it('keeps the seed button to the loom theme', () => {
    renderApp(makeEngine())
    expect(explorer.props!.onSeedTrack).toBeUndefined()
  })

  it('leaves the FLX4 loop section to the loom in Loom theme', () => {
    updateAppSettings({ theme: 'loom' })
    const bus = createControlBus()
    renderApp(makeEngine(), bus)
    vi.mocked(applyAppIntent).mockClear()

    act(() => bus.publish({ kind: 'track_beat_loop', deck: 'a', beats: 4 }))
    act(() => bus.publish({ kind: 'track_loop_halve', deck: 'a' }))
    act(() => bus.publish({ kind: 'track_loop_double', deck: 'b' }))
    act(() => bus.publish({ kind: 'cue_mix', value: 0.4 }))

    expect(vi.mocked(applyAppIntent).mock.calls.map(([intent]) => intent.kind)).toEqual(['cue_mix'])
  })

  it('keeps the beat loop on the deck dispatch in the DJ theme', () => {
    const bus = createControlBus()
    renderApp(makeEngine(), bus)
    vi.mocked(applyAppIntent).mockClear()

    act(() => bus.publish({ kind: 'track_beat_loop', deck: 'a', beats: 4 }))

    expect(vi.mocked(applyAppIntent).mock.calls.map(([intent]) => intent.kind)).toEqual(['track_beat_loop'])
  })

  it('persists the render-ahead switch', () => {
    updateAppSettings({ theme: 'loom' })
    renderApp(makeEngine())
    fireEvent.click(screen.getByRole('button', { name: 'Settings' }))

    fireEvent.click(screen.getByRole('switch', { name: 'Render ahead' }))

    expect(
      screen.getByRole('switch', { name: 'Render ahead' }),
    ).toHaveAttribute('aria-checked', 'false')
    expect(loadAppSettings().renderAhead).toBe(false)
  })
})
