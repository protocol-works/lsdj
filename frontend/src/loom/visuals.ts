import { useEffect, useSyncExternalStore, type RefObject } from 'react'

import type { DeckId } from '../audio/types'
import type { BeatClock } from '../deck/useDeck'
import { secondsSinceBeat } from '../visuals/performanceMath'

const DECKS: DeckId[] = ['a', 'b']
const REDUCED_MOTION = '(prefers-reduced-motion: reduce)'

const motionQuery = () =>
  typeof window.matchMedia === 'function' ? window.matchMedia(REDUCED_MOTION) : null
const subscribeMotion = (onChange: () => void) => {
  const query = motionQuery()
  query?.addEventListener('change', onChange)
  return () => query?.removeEventListener('change', onChange)
}
const motionReduced = () => motionQuery()?.matches ?? false

/** The OS "reduce motion" preference, live: the loom's frame loops stop on it. */
export const useReducedMotion = () => useSyncExternalStore(subscribeMotion, motionReduced)

/** Each deck's beat phase (0 on the beat → 1 just before the next) as
 * `--loom-beat-a/b` on `root`, from one rAF; loom.css pulses the deck badge,
 * the playhead and a tied node from them. A frame is two style writes and no
 * React state. Off, with the variables cleared, while `enabled` is false or
 * motion is reduced; a deck with no beat clock leaves its variable unset. */
export function useBeatPulse(
  root: RefObject<HTMLElement | null>,
  enabled: boolean,
  getBeatA: () => BeatClock | null,
  getBeatB: () => BeatClock | null,
  getContextTime: () => number | null,
) {
  const reduced = useReducedMotion()
  const run = enabled && !reduced
  useEffect(() => {
    const element = root.current
    if (!run || !element) return
    const clocks = { a: getBeatA, b: getBeatB }
    let frame = requestAnimationFrame(function pulse() {
      frame = requestAnimationFrame(pulse)
      const now = getContextTime()
      for (const deck of DECKS) {
        const beat = clocks[deck]()
        const since = secondsSinceBeat(now, beat)
        if (beat && since !== null) {
          element.style.setProperty(`--loom-beat-${deck}`, (since / beat.periodSeconds).toFixed(3))
        } else {
          element.style.removeProperty(`--loom-beat-${deck}`)
        }
      }
    })
    return () => {
      cancelAnimationFrame(frame)
      for (const deck of DECKS) element.style.removeProperty(`--loom-beat-${deck}`)
    }
  }, [run, root, getBeatA, getBeatB, getContextTime])
}
