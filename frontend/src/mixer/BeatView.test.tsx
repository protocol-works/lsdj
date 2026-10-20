import { fireEvent, render, screen } from '@testing-library/react'
import type { ComponentProps } from 'react'
import { describe, expect, it, vi } from 'vitest'

import { BeatView } from './BeatView'

describe('BeatView', () => {
  it('stacks both decks’ close-ups', () => {
    render(<BeatView getSourceA={() => null} getSourceB={() => null} />)
    expect(
      screen.getByRole('img', { name: 'Deck A close-up' }),
    ).toBeInTheDocument()
    expect(
      screen.getByRole('img', { name: 'Deck B close-up' }),
    ).toBeInTheDocument()
  })

  it('turns the strips on their side in the vertical layout', () => {
    render(
      <BeatView vertical getSourceA={() => null} getSourceB={() => null} />,
    )
    const strip = screen.getByRole('img', { name: 'Deck A close-up' })
    expect(strip.className).toContain('ui-zoomstrip--vertical')
    // The canvas swaps its axes: time runs down the long side.
    expect(Number(strip.getAttribute('height'))).toBeGreaterThan(
      Number(strip.getAttribute('width')),
    )
  })
})

describe('BeatView grab', () => {
  const source = {
    bands: { baseHop: 0, endHop: 0, copyWindow: () => {} },
    playheadHop: 1000,
    realSecondsPerHop: 0.01,
    beat: null,
    cues: [],
    loop: null,
  }

  const setup = (track: { playing: boolean } | null, mode = 'playback') => {
    const deck = {
      mode,
      track: track && { ...track, rate: 1 },
      seekTrack: vi.fn(),
      nudgeTrackPhase: vi.fn(),
    } as unknown as ComponentProps<typeof BeatView>['deckA']
    render(
      <BeatView
        getSourceA={() => source}
        getSourceB={() => null}
        deckA={deck}
      />,
    )
    const strip = screen.getByRole('img', { name: 'Deck A close-up' })
    strip.setPointerCapture = vi.fn()
    strip.getBoundingClientRect = () => ({ width: 720, height: 88 }) as DOMRect
    return { deck: deck!, strip }
  }

  const drag = async (strip: HTMLElement, fromX: number, toX: number) => {
    fireEvent.pointerDown(strip, { pointerId: 1, button: 0, clientX: fromX })
    await new Promise((resolve) => setTimeout(resolve, 20))
    fireEvent.pointerMove(strip, { pointerId: 1, clientX: toX })
    fireEvent.pointerUp(strip, { pointerId: 1, clientX: toX })
  }

  it('cues a paused track by dragging the waveform', async () => {
    const { deck, strip } = setup({ playing: false })
    expect(strip.className).toContain('ui-zoomstrip--grabbable')
    // Half the strip leftward = half the 400-hop window later.
    await drag(strip, 400, 40)
    const hopSeconds = 512 / 48_000
    expect(deck.seekTrack).toHaveBeenCalledWith(
      expect.closeTo((1000 + 200) * hopSeconds, 6),
    )
    expect(deck.nudgeTrackPhase).not.toHaveBeenCalled()
  })

  it('nudges a playing track’s phase instead of seeking', async () => {
    const { deck, strip } = setup({ playing: true })
    await drag(strip, 400, 40)
    expect(deck.seekTrack).not.toHaveBeenCalled()
    expect(deck.nudgeTrackPhase).toHaveBeenCalledTimes(1)
    const nudge = vi.mocked(deck.nudgeTrackPhase).mock.calls[0][0]
    // Ahead, but only as far as a gentle bend allows.
    expect(nudge).toBeGreaterThan(0)
    expect(nudge).toBeLessThan(0.01)
  })

  it('leaves a live deck’s strip inert', async () => {
    const { deck, strip } = setup(null, 'realtime')
    expect(strip.className).not.toContain('ui-zoomstrip--grabbable')
    await drag(strip, 400, 40)
    expect(deck.seekTrack).not.toHaveBeenCalled()
  })
})
