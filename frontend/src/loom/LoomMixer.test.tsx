import { fireEvent, render, screen, within } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import type { ChannelControls } from '../mixer/MixerStrip'
import { LoomMixer, type LoomDeckPanel, type LoomMixerProps } from './LoomMixer'

function makeChannel(overrides: Partial<ChannelControls> = {}): ChannelControls {
  return {
    volume: 0.8,
    eq: { low: 0.5, mid: 0.5, high: 0.5 },
    cue: false,
    trim: { mode: 'auto', db: 0 },
    onSetVolume: vi.fn(),
    onSetEqBand: vi.fn(),
    onSetCue: vi.fn(),
    onSetTrimDb: vi.fn(),
    onEnableAutoTrim: vi.fn(),
    getLevel: () => 0,
    ...overrides,
  }
}

function makeDeck(overrides: Partial<LoomDeckPanel> = {}): LoomDeckPanel {
  return {
    title: 'acid dawn',
    next: { label: '12 → breakbeat', warn: false },
    bar: 'bar 9 / 32',
    playing: false,
    onPlay: vi.fn(),
    onStop: vi.fn(),
    onSync: vi.fn(),
    syncDisabled: false,
    auto: false,
    onAuto: vi.fn(),
    loopBars: null,
    onLoop: vi.fn(),
    fx: { kind: null, amount: 0 },
    onSetFx: vi.fn(),
    onSetFxAmount: vi.fn(),
    ...overrides,
  }
}

function makeProps(overrides: Partial<LoomMixerProps> = {}): LoomMixerProps {
  return {
    channels: { a: makeChannel(), b: makeChannel() },
    decks: { a: makeDeck(), b: makeDeck({ title: 'tape hiss' }) },
    crossfade: 0.5,
    onCrossfade: vi.fn(),
    cueMix: 0.5,
    onCueMix: vi.fn(),
    getMasterLevel: () => 0,
    getPhaseOffset: () => null,
    ...overrides,
  }
}

const deckPanel = (id: 'a' | 'b') => screen.getByRole('group', { name: `Deck ${id}` })

describe('LoomMixer status line', () => {
  it('shows each deck title with its next tag and bar', () => {
    render(<LoomMixer {...makeProps()} />)
    expect(within(deckPanel('a')).getByText('acid dawn')).toBeInTheDocument()
    expect(within(deckPanel('b')).getByText('tape hiss')).toBeInTheDocument()
    expect(within(deckPanel('a')).getByText('12 → breakbeat')).toBeInTheDocument()
    expect(within(deckPanel('a')).getByText('bar 9 / 32')).toBeInTheDocument()
  })

  it('turns the next tag amber when it warns', () => {
    const decks = {
      a: makeDeck({ next: { label: '6 → HOLD', warn: true } }),
      b: makeDeck(),
    }
    render(<LoomMixer {...makeProps({ decks })} />)
    expect(within(deckPanel('a')).getByText('6 → HOLD')).toHaveClass('loom-mixer__next--warn')
    expect(within(deckPanel('b')).getByText('12 → breakbeat')).not.toHaveClass(
      'loom-mixer__next--warn',
    )
  })

  it('shows the nothing-tied placeholder and disables play on an empty deck', () => {
    const decks = { a: makeDeck({ title: null, next: null, bar: null }), b: makeDeck() }
    render(<LoomMixer {...makeProps({ decks })} />)
    const a = deckPanel('a')
    expect(within(a).getByText('Nothing tied — tie a node to this deck')).toBeInTheDocument()
    expect(within(a).getByRole('button', { name: 'Play' })).toBeDisabled()
  })

  it('offers bounce only when the deck can bounce', () => {
    const onBounce = vi.fn()
    render(<LoomMixer {...makeProps({ decks: { a: makeDeck({ onBounce }), b: makeDeck() } })} />)
    const bounce = within(deckPanel('a')).getByRole('button', {
      name: 'Bounce what this deck played to songs',
    })
    fireEvent.click(bounce)
    expect(onBounce).toHaveBeenCalled()
    expect(
      within(deckPanel('b')).queryByRole('button', { name: 'Bounce what this deck played to songs' }),
    ).toBeNull()
  })
})

describe('LoomMixer transport', () => {
  it('calls play on a stopped deck and stop on a playing one', () => {
    const a = makeDeck()
    const b = makeDeck({ playing: true })
    render(<LoomMixer {...makeProps({ decks: { a, b } })} />)
    fireEvent.click(within(deckPanel('a')).getByRole('button', { name: 'Play' }))
    expect(a.onPlay).toHaveBeenCalled()
    fireEvent.click(within(deckPanel('b')).getByRole('button', { name: 'Stop' }))
    expect(b.onStop).toHaveBeenCalled()
  })

  it('disables SYNC when the deck cannot sync', () => {
    const decks = { a: makeDeck({ syncDisabled: true }), b: makeDeck() }
    render(<LoomMixer {...makeProps({ decks })} />)
    expect(within(deckPanel('a')).getByRole('button', { name: 'Sync' })).toBeDisabled()
    fireEvent.click(within(deckPanel('b')).getByRole('button', { name: 'Sync' }))
    expect(decks.b.onSync).toHaveBeenCalled()
  })

  it('toggles autopilot and lights it while on', () => {
    const a = makeDeck()
    const b = makeDeck({ auto: true })
    render(<LoomMixer {...makeProps({ decks: { a, b } })} />)
    fireEvent.click(within(deckPanel('a')).getByRole('button', { name: 'Auto' }))
    expect(a.onAuto).toHaveBeenCalledWith(true)
    const autoB = within(deckPanel('b')).getByRole('button', { name: 'Auto' })
    expect(autoB).toHaveAttribute('aria-pressed', 'true')
    fireEvent.click(autoB)
    expect(b.onAuto).toHaveBeenCalledWith(false)
  })

  it('routes CUE to the deck channel', () => {
    const a = makeChannel({ cue: true })
    render(<LoomMixer {...makeProps({ channels: { a, b: makeChannel() } })} />)
    const cue = within(deckPanel('a')).getByRole('button', { name: 'Cue' })
    expect(cue).toHaveAttribute('aria-pressed', 'true')
    fireEvent.click(cue)
    expect(a.onSetCue).toHaveBeenCalledWith(false)
  })

  it('sets a beat loop and releases it from the lit length', () => {
    const onLoop = vi.fn()
    const props = makeProps({ decks: { a: makeDeck({ onLoop }), b: makeDeck() } })
    const { rerender } = render(<LoomMixer {...props} />)
    fireEvent.click(within(deckPanel('a')).getByRole('radio', { name: '8' }))
    expect(onLoop).toHaveBeenLastCalledWith(8)

    rerender(
      <LoomMixer {...props} decks={{ a: makeDeck({ onLoop, loopBars: 8 }), b: makeDeck() }} />,
    )
    const eight = within(deckPanel('a')).getByRole('radio', { name: '8' })
    expect(eight).toHaveAttribute('aria-checked', 'true')
    fireEvent.click(eight)
    expect(onLoop).toHaveBeenLastCalledWith(null)
  })
})

describe('LoomMixer channel strip', () => {
  it('routes fader, EQ and trim moves to the right channel', () => {
    const a = makeChannel({ trim: { mode: 'manual', db: 0 } })
    const b = makeChannel()
    render(<LoomMixer {...makeProps({ channels: { a, b } })} />)

    fireEvent.change(within(deckPanel('a')).getByLabelText('Volume'), {
      target: { value: '0.3' },
    })
    expect(a.onSetVolume).toHaveBeenCalledWith(0.3)
    expect(b.onSetVolume).not.toHaveBeenCalled()

    fireEvent.change(within(deckPanel('b')).getByLabelText('EQ Low'), { target: { value: '0' } })
    expect(b.onSetEqBand).toHaveBeenCalledWith('low', 0)
    expect(a.onSetEqBand).not.toHaveBeenCalled()

    // 0.75 of the trim sweep is +6 dB on the ±12 dB range, as on the DJ mixer.
    fireEvent.change(within(deckPanel('a')).getByLabelText('Trim'), { target: { value: '0.75' } })
    expect(a.onSetTrimDb).toHaveBeenCalledWith(6)
  })

  it('stacks the EQ knobs Hi, Mid, Low after the trim', () => {
    render(<LoomMixer {...makeProps()} />)
    const labels = within(deckPanel('a'))
      .getAllByText(/^(Trim|EQ Hi|EQ Mid|EQ Low)$/)
      .map((node) => node.textContent)
    expect(labels).toEqual(['Trim', 'EQ Hi', 'EQ Mid', 'EQ Low'])
  })

  it('picks a Color FX and moves its amount', () => {
    const a = makeDeck({ fx: { kind: 'filter', amount: 0.5 } })
    render(<LoomMixer {...makeProps({ decks: { a, b: makeDeck() } })} />)
    const panel = deckPanel('a')
    fireEvent.change(within(panel).getByLabelText('Effect'), { target: { value: 'dub_echo' } })
    expect(a.onSetFx).toHaveBeenCalledWith('dub_echo')
    fireEvent.change(within(panel).getByLabelText('Effect'), { target: { value: '' } })
    expect(a.onSetFx).toHaveBeenLastCalledWith(null)
    fireEvent.change(within(panel).getByLabelText('FX amount'), { target: { value: '0.2' } })
    expect(a.onSetFxAmount).toHaveBeenCalledWith(0.2)
  })
})

describe('LoomMixer core', () => {
  it('reports crossfader and cue-mix moves and keeps the crossfade shortcut', () => {
    const props = makeProps()
    render(<LoomMixer {...props} />)
    const crossfade = screen.getByLabelText('Crossfade')
    expect(crossfade).toHaveAttribute('data-shortcut', 'crossfade')
    fireEvent.change(crossfade, { target: { value: '0.1' } })
    expect(props.onCrossfade).toHaveBeenCalledWith(0.1)
    fireEvent.change(screen.getByLabelText('Cue mix'), { target: { value: '0.2' } })
    expect(props.onCueMix).toHaveBeenCalledWith(0.2)
  })

  it('renders the scope slot inside the scope window', () => {
    const { container } = render(
      <LoomMixer {...makeProps({ scope: <canvas data-testid="scope" /> })} />,
    )
    expect(container.querySelector('.loom-mixer__scope')).toContainElement(
      screen.getByTestId('scope'),
    )
  })
})
