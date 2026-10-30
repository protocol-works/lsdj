import { useEffect, useRef } from 'react'

import type { DeckId } from '../audio/types'
import type { BeatClock } from '../deck/useDeck'
import { equalPowerGains, perceptualLevel, SILENCE_EPSILON } from '../visuals/performanceMath'
import { useReducedMotion } from './visuals'

export type LoomScopeDeck = {
  /** The channel's post-fader level, 0..1 (`ChannelControls.getLevel`). */
  getLevel: () => number
  /** Sets how fast the deck's figure turns; null turns it at 120 BPM. */
  getBeat: () => BeatClock | null
}

export type LoomScopeProps = {
  decks: Record<DeckId, LoomScopeDeck>
  crossfade: number
}

const DECKS: DeckId[] = ['a', 'b']
const STEPS = 180
// The mockup's drift: each figure turns about two-thirds of a radian a beat.
const TURN_PER_BEAT = 0.66
const FALLBACK_PERIOD = 0.5
const MAX_FRAME_SECONDS = 0.1
// What each frame erases of the last (destination-out): the trails.
const FADE = 'rgba(0, 0, 0, 0.22)'
// Deck A draws 3:2, deck B 2:3, so the two never coincide.
const RATIOS: Record<DeckId, [number, number]> = { a: [3, 2], b: [2, 3] }

/** The scope window's Lissajous figure (the mockup's `scope()`): one curve
 * per deck in its colour, sized by its level through the crossfade and
 * turning at its tempo, over the fading trails of the last frames. One rAF,
 * idle while the page is hidden and stopped under reduced motion; React never
 * sees a frame. */
export function LoomScope({ decks, crossfade }: LoomScopeProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null)
  const latest = useRef({ decks, crossfade })
  useEffect(() => {
    latest.current = { decks, crossfade }
  })
  const reduced = useReducedMotion()

  useEffect(() => {
    const canvas = canvasRef.current
    const context = canvas?.getContext('2d')
    if (!canvas || !context || reduced) return
    const ratio = window.devicePixelRatio || 1
    const fit = () => {
      canvas.width = Math.round(canvas.clientWidth * ratio)
      canvas.height = Math.round(canvas.clientHeight * ratio)
    }
    fit()
    // Sized off the frame path: reading the layout per frame would force it.
    const observer = typeof ResizeObserver === 'function' ? new ResizeObserver(fit) : null
    observer?.observe(canvas)
    // The deck colours are tokens themed by <html data-accent>; re-read on a
    // switch, the cheap dataset check keeping frames off style recalc.
    const styles = getComputedStyle(canvas)
    const readColours = () => ({
      a: styles.getPropertyValue('--color-deck-a').trim(),
      b: styles.getPropertyValue('--color-deck-b').trim(),
    })
    let accent = document.documentElement.dataset.accent ?? ''
    let colours = readColours()
    const phase = { a: 0, b: 0 }
    let last: number | null = null

    let frame = requestAnimationFrame(function draw(now: number) {
      frame = requestAnimationFrame(draw)
      if (document.visibilityState === 'hidden') {
        last = null
        return
      }
      const seconds = last === null ? 0 : Math.min(MAX_FRAME_SECONDS, (now - last) / 1000)
      last = now
      if ((document.documentElement.dataset.accent ?? '') !== accent) {
        accent = document.documentElement.dataset.accent ?? ''
        colours = readColours()
      }
      const { width, height } = canvas
      context.globalCompositeOperation = 'destination-out'
      context.fillStyle = FADE
      context.fillRect(0, 0, width, height)
      context.globalCompositeOperation = 'source-over'
      const gains = equalPowerGains(latest.current.crossfade)
      for (const deck of DECKS) {
        const source = latest.current.decks[deck]
        const level = source.getLevel()
        if (!(level > SILENCE_EPSILON)) continue
        phase[deck] += (seconds / (source.getBeat()?.periodSeconds ?? FALLBACK_PERIOD)) * TURN_PER_BEAT
        const amplitude = Math.min(1, gains[deck] * (0.55 + 0.45 * perceptualLevel(level)))
        if (amplitude < 0.03) continue
        const [fx, fy] = RATIOS[deck]
        context.beginPath()
        for (let i = 0; i <= STEPS; i++) {
          const t = (i / STEPS) * Math.PI * 2
          const x = width / 2 + Math.sin(t * fx + phase[deck]) * width * 0.44 * amplitude
          const y = height / 2 + Math.sin(t * fy) * height * 0.42 * amplitude
          if (i === 0) context.moveTo(x, y)
          else context.lineTo(x, y)
        }
        context.globalAlpha = 0.85
        context.strokeStyle = colours[deck]
        context.shadowColor = colours[deck]
        context.shadowBlur = 9 * ratio
        context.lineWidth = 1.1 * ratio
        context.stroke()
        context.globalAlpha = 1
        context.shadowBlur = 0
      }
    })

    return () => {
      cancelAnimationFrame(frame)
      observer?.disconnect()
    }
  }, [reduced])

  return <canvas ref={canvasRef} className="loom-scope" aria-hidden="true" />
}
