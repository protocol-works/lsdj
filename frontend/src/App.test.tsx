/** App-level wiring tests with a fake engine: the crossfade and cue-mix
 * chains (audio bus + persistence) are owned by App and must hold from
 * both the on-screen control and the hardware intent path. */

import { act, fireEvent, render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import App from './App'
import { AudioEngineProvider } from './audio/AudioEngineProvider'
import type { AudioEngine } from './audio/types'
import { createControlBus, type ControlBus } from './control/bus'
import { ControlBusProvider } from './control/ControlBusProvider'
import { loadAppSettings } from './persistence'

// The LSDJ brand mark renders through three.js / react-three-fiber, which needs
// a real WebGL context and ResizeObserver — neither exists in jsdom. These
// tests exercise App's crossfade/cue-mix wiring, not the logo, so stub it out.
vi.mock('./ui/HypercubeMark', () => ({ HypercubeMark: () => null }))

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

describe('App settings drawer', () => {
  it('hosts a per-deck model picker (moved out of the deck columns)', () => {
    const engine = makeEngine()
    renderApp(engine)
    // The decks no longer carry a model picker; it lives in settings now.
    expect(screen.queryByLabelText('Deck A')).toBeNull()

    fireEvent.click(screen.getByRole('button', { name: 'Settings' }))

    expect(screen.getByLabelText('Deck A')).toBeInTheDocument()
    expect(screen.getByLabelText('Deck B')).toBeInTheDocument()
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
