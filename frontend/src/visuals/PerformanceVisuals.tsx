import { useEffect, useRef, useState, type RefObject } from 'react'

import type { DeckId } from '../audio/types'
import {
  INITIAL_VISUAL_FRAME_STATE,
  clamp01,
  reduceVisualFrame,
  reducedMotionWash,
  type VisualBeatClock,
  type VisualFrameState,
} from './performanceMath'

const REDUCED_MOTION_QUERY = '(prefers-reduced-motion: reduce)'

const PERFORMANCE_VISUAL_PROPERTIES = [
  '--performance-wash-a',
  '--performance-wash-b',
  '--performance-pulse-a',
  '--performance-pulse-b',
] as const

export type PerformanceDeckSource = {
  audible: boolean
  getLevel: () => number
  getBeat: () => VisualBeatClock | null
}

export type PerformanceVisualsProps = {
  enabled: boolean
  rootRef: RefObject<HTMLElement | null>
  crossfade: number
  getContextTime: () => number | null
  getMasterLevel: () => number
  decks: Record<DeckId, PerformanceDeckSource>
}

type LatestSources = Omit<PerformanceVisualsProps, 'enabled' | 'rootRef'>
type VisualOutput = Pick<VisualFrameState, 'wash' | 'pulse'>

function motionIsReduced(): boolean {
  return (
    typeof window !== 'undefined' &&
    typeof window.matchMedia === 'function' &&
    window.matchMedia(REDUCED_MOTION_QUERY).matches
  )
}

function clearVisualProperties(root: HTMLElement) {
  for (const property of PERFORMANCE_VISUAL_PROPERTIES) {
    root.style.removeProperty(property)
  }
}

function formatVisualValue(value: number): string {
  return clamp01(value).toFixed(4)
}

function writeVisualProperties(
  root: HTMLElement,
  output: VisualOutput,
  previous: Partial<Record<(typeof PERFORMANCE_VISUAL_PROPERTIES)[number], string>>,
) {
  const values = {
    '--performance-wash-a': formatVisualValue(output.wash.a),
    '--performance-wash-b': formatVisualValue(output.wash.b),
    '--performance-pulse-a': formatVisualValue(output.pulse.a),
    '--performance-pulse-b': formatVisualValue(output.pulse.b),
  } satisfies Record<(typeof PERFORMANCE_VISUAL_PROPERTIES)[number], string>

  for (const property of PERFORMANCE_VISUAL_PROPERTIES) {
    const value = values[property]
    if (previous[property] === value) continue
    root.style.setProperty(property, value)
    previous[property] = value
  }
}

function readNumber(getter: () => number, fallback = 0): number {
  try {
    const value = getter()
    return Number.isFinite(value) ? value : fallback
  } catch {
    return fallback
  }
}

function readContextTime(getter: () => number | null): number | null {
  try {
    const value = getter()
    return value !== null && Number.isFinite(value) ? value : null
  } catch {
    return null
  }
}

function readBeat(getter: () => VisualBeatClock | null): VisualBeatClock | null {
  try {
    return getter()
  } catch {
    return null
  }
}

export function PerformanceVisuals({
  enabled,
  rootRef,
  crossfade,
  getContextTime,
  getMasterLevel,
  decks,
}: PerformanceVisualsProps) {
  const [reducedMotion, setReducedMotion] = useState(motionIsReduced)
  const latestSources = useRef<LatestSources>({
    crossfade,
    getContextTime,
    getMasterLevel,
    decks,
  })

  useEffect(() => {
    latestSources.current = {
      crossfade,
      getContextTime,
      getMasterLevel,
      decks,
    }
  })

  useEffect(() => {
    if (!enabled || typeof window.matchMedia !== 'function') return

    const preference = window.matchMedia(REDUCED_MOTION_QUERY)
    const updatePreference = () => setReducedMotion(preference.matches)
    updatePreference()
    preference.addEventListener('change', updatePreference)
    return () => preference.removeEventListener('change', updatePreference)
  }, [enabled])

  const motionReduced = enabled && (reducedMotion || motionIsReduced())

  useEffect(() => {
    const root = rootRef.current
    if (!root || !enabled || motionReduced) return
    if (typeof window.requestAnimationFrame !== 'function') {
      clearVisualProperties(root)
      return
    }

    const lastOutput: Partial<
      Record<(typeof PERFORMANCE_VISUAL_PROPERTIES)[number], string>
    > = {}
    let frameState = INITIAL_VISUAL_FRAME_STATE
    let previousTimestamp: number | null = null
    let frameId: number | null = null

    const drawFrame: FrameRequestCallback = (timestamp) => {
      const sources = latestSources.current
      const deckA = sources.decks.a
      const deckB = sources.decks.b
      const contextTime = readContextTime(sources.getContextTime)
      const deltaSeconds =
        previousTimestamp === null ? 0 : (timestamp - previousTimestamp) / 1000
      previousTimestamp = timestamp

      frameState = reduceVisualFrame(frameState, {
        deltaSeconds,
        contextTime,
        crossfade: sources.crossfade,
        masterLevel: readNumber(sources.getMasterLevel),
        decks: {
          a: {
            level: readNumber(deckA.getLevel),
            audible: deckA.audible,
            beat: deckA.audible ? readBeat(deckA.getBeat) : null,
          },
          b: {
            level: readNumber(deckB.getLevel),
            audible: deckB.audible,
            beat: deckB.audible ? readBeat(deckB.getBeat) : null,
          },
        },
      })
      writeVisualProperties(root, frameState, lastOutput)
      frameId = window.requestAnimationFrame(drawFrame)
    }

    frameId = window.requestAnimationFrame(drawFrame)
    return () => {
      if (frameId !== null && typeof window.cancelAnimationFrame === 'function') {
        window.cancelAnimationFrame(frameId)
      }
      clearVisualProperties(root)
    }
  }, [enabled, motionReduced, rootRef])

  useEffect(() => {
    const root = rootRef.current
    if (!root) return
    if (!enabled) {
      clearVisualProperties(root)
      return
    }
    if (!motionReduced) return

    const lastOutput = {}
    const wash = reducedMotionWash(crossfade, {
      a: decks.a.audible,
      b: decks.b.audible,
    })
    writeVisualProperties(
      root,
      { wash, pulse: { a: 0, b: 0 } },
      lastOutput,
    )
    return () => clearVisualProperties(root)
  }, [crossfade, decks.a.audible, decks.b.audible, enabled, motionReduced, rootRef])

  return (
    <div className="performance-visuals" aria-hidden="true" hidden={!enabled}>
      <span className="performance-visuals__glow performance-visuals__glow--a" />
      <span className="performance-visuals__glow performance-visuals__glow--b" />
      <span className="performance-visuals__grid" />
    </div>
  )
}
