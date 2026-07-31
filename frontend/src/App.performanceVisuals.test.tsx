import { render } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { ReactNode } from 'react'

import App from './App'
import { AudioEngineProvider } from './audio/AudioEngineProvider'
import type { AudioEngine } from './audio/types'
import { createControlBus } from './control/bus'
import { ControlBusProvider } from './control/ControlBusProvider'
import type { DeckControls } from './deck/useDeck'
import type { PerformanceVisualsProps } from './visuals/PerformanceVisuals'

const harness = vi.hoisted(() => ({
  decks: {} as Record<'a' | 'b', unknown>,
  visualProps: [] as unknown[],
}))

vi.mock('./deck/useDeck', () => ({
  useDeck: (deckId: 'a' | 'b') => harness.decks[deckId],
}))
vi.mock('./visuals/PerformanceVisuals', () => ({
  PerformanceVisuals: (props: unknown) => {
    harness.visualProps.push(props)
    return null
  },
}))
vi.mock('./deck/DeckColumn', () => ({ DeckColumn: () => null }))
vi.mock('./mixer/BeatView', () => ({ BeatView: () => null }))
vi.mock('./mixer/MixerStrip', () => ({ MixerStrip: () => null }))
vi.mock('./mixer/RecordControl', () => ({ RecordControl: () => null }))
vi.mock('./media/MediaExplorer', () => ({ MediaExplorer: () => null }))
vi.mock('./control/MidiControls', () => ({ MidiControls: () => null }))
vi.mock('./control/useMidi', () => ({
  useMidi: () => ({
    connected: false,
    deviceName: null,
    devices: [],
    selectDevice: vi.fn(),
  }),
}))
vi.mock('./models/LoraProvider', () => ({
  LoraProvider: ({ children }: { children: ReactNode }) => children,
}))
vi.mock('./ui/Drawer', () => ({ Drawer: () => null }))
vi.mock('./ui/HypercubeMark', () => ({ HypercubeMark: () => null }))
vi.mock('./audio/interfaceStore', () => ({
  useInterfaceStore: () => null,
  useProjected: (_external: unknown, initial: number) => [initial, vi.fn()],
}))
vi.mock('./audio/nativeEngine', () => ({
  FX_ARG: {},
  getMcpInfo: vi.fn(async () => null),
  invoke: vi.fn(async () => undefined),
  rotateMcpToken: vi.fn(async () => ''),
  setMcpPort: vi.fn(async (port: number) => port),
  setRecordingsFolder: vi.fn(async () => undefined),
  styleApplyPreset: vi.fn(),
  subscribeDeckCommand: vi.fn(() => () => {}),
  subscribeLoadSample: vi.fn(() => () => {}),
  subscribeLoadTrack: vi.fn(() => () => {}),
}))

const LIVE_CLOCK = { periodSeconds: 0.5, beatAtContext: 10 }
const TRACK_CLOCK = { periodSeconds: 0.4, beatAtContext: 20 }

function makeDeck(
  overrides: Omit<Partial<DeckControls>, 'state'> & {
    state?: Partial<DeckControls['state']>
  } = {},
): DeckControls {
  const noop = vi.fn()
  const { state: stateOverrides, ...controlOverrides } = overrides
  const base = {
    state: {
      availableModels: [],
      model: null,
      ramInfo: null,
      playing: false,
      connection: 'open',
      switchingModel: false,
      ...stateOverrides,
    },
    volume: 0.8,
    eq: { low: 0.5, mid: 0.5, high: 0.5 },
    cue: false,
    trim: { mode: 'auto', db: 0 },
    fx: { kind: null, amount: 0 },
    loop: { slots: [], active: null, layering: [], seconds: 4 },
    generateError: null,
    bpm: null,
    mode: 'realtime',
    track: null,
    primed: false,
    getLiveBeat: vi.fn(() => LIVE_CLOCK),
    getTrackBeat: vi.fn(() => TRACK_CLOCK),
    getChannelLevel: vi.fn(() => 1),
  }
  return new Proxy(
    { ...base, ...controlOverrides } as unknown as DeckControls,
    {
      get(target, property, receiver) {
        const value = Reflect.get(target, property, receiver)
        return value === undefined ? noop : value
      },
    },
  )
}

function makeEngine(): AudioEngine {
  return {
    createDeckChannel: vi.fn(),
    resume: vi.fn(async () => {}),
    getContextTime: vi.fn(() => 10),
    setCrossfade: vi.fn(),
    setCueMix: vi.fn(),
    auditionPlay: vi.fn(async () => {}),
    auditionStop: vi.fn(),
    listOutputDevices: vi.fn(async () => []),
    setMainDevice: vi.fn(async () => {}),
    setCueDevice: vi.fn(async () => {}),
    startRecording: vi.fn(async () => '/Downloads/lsdj-take.wav'),
    stopRecording: vi.fn(async () => {}),
    getMasterLevel: vi.fn(() => 1),
    getMasterGainReduction: vi.fn(() => 0),
  }
}

function renderApp() {
  const engine = makeEngine()
  return render(
    <AudioEngineProvider engine={engine}>
      <ControlBusProvider bus={createControlBus()}>
        <App />
      </ControlBusProvider>
    </AudioEngineProvider>,
  )
}

function latestVisualProps(): PerformanceVisualsProps {
  return harness.visualProps.at(-1) as PerformanceVisualsProps
}

beforeEach(() => {
  harness.visualProps = []
  harness.decks.a = makeDeck()
  harness.decks.b = makeDeck()
})

describe('App performance-visual source wiring', () => {
  it('hard-gates a primed realtime deck and selects its live clock', () => {
    const getLiveBeat = vi.fn(() => LIVE_CLOCK)
    harness.decks.a = makeDeck({
      state: { playing: true },
      primed: true,
      getLiveBeat,
    })
    renderApp()

    const source = latestVisualProps().decks.a
    expect(source.audible).toBe(false)
    expect(source.getBeat()).toEqual(LIVE_CLOCK)
    expect(getLiveBeat).toHaveBeenCalledTimes(1)
  })

  it('selects the track clock and gates paused or gridless playback', () => {
    const getTrackBeat = vi.fn(() => TRACK_CLOCK)
    harness.decks.b = makeDeck({
      mode: 'playback',
      track: { playing: true, bpm: 150, rate: 1 } as DeckControls['track'],
      getTrackBeat,
    })
    const view = renderApp()

    let source = latestVisualProps().decks.b
    expect(source.audible).toBe(true)
    expect(source.getBeat()).toEqual(TRACK_CLOCK)
    expect(getTrackBeat).toHaveBeenCalledTimes(1)

    harness.decks.b = makeDeck({
      mode: 'playback',
      track: { playing: false } as DeckControls['track'],
      getTrackBeat: vi.fn(() => null),
    })
    view.rerender(
      <AudioEngineProvider engine={makeEngine()}>
        <ControlBusProvider bus={createControlBus()}>
          <App />
        </ControlBusProvider>
      </AudioEngineProvider>,
    )
    source = latestVisualProps().decks.b
    expect(source.audible).toBe(false)
    expect(source.getBeat()).toBeNull()
  })
})
