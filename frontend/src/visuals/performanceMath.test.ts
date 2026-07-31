import { describe, expect, it } from 'vitest'

import {
  INITIAL_VISUAL_FRAME_STATE,
  MAX_FRAME_DELTA_SECONDS,
  PULSE_DECAY_SECONDS,
  REDUCED_MOTION_WASH,
  beatPulse,
  clamp01,
  contributionBalance,
  equalPowerGains,
  positiveModulo,
  reduceVisualFrame,
  reducedMotionWash,
  secondsSinceBeat,
  smoothLevel,
  type VisualFrameInput,
  type VisualFrameState,
} from './performanceMath'

const CLOCK = { periodSeconds: 0.5, beatAtContext: 10 }

function frame(overrides: Partial<VisualFrameInput> = {}): VisualFrameInput {
  return {
    deltaSeconds: 1 / 60,
    contextTime: 10,
    crossfade: 0.5,
    masterLevel: 1,
    decks: {
      a: { level: 1, audible: true, beat: CLOCK },
      b: { level: 1, audible: true, beat: CLOCK },
    },
    ...overrides,
  }
}

describe('performance visual math', () => {
  it('clamps finite values and makes invalid values inert', () => {
    expect(clamp01(-1)).toBe(0)
    expect(clamp01(0.4)).toBe(0.4)
    expect(clamp01(2)).toBe(1)
    expect(clamp01(Number.NaN)).toBe(0)
    expect(clamp01(Number.POSITIVE_INFINITY)).toBe(0)
  })

  it('wraps positive and negative beat phases', () => {
    expect(positiveModulo(0.6, 0.5)).toBeCloseTo(0.1)
    expect(positiveModulo(-0.1, 0.5)).toBeCloseTo(0.4)
    expect(positiveModulo(-0.5, 0.5)).toBe(0)
    expect(positiveModulo(1, 0)).toBe(0)
  })

  it('matches the engine equal-power law at endpoints and center', () => {
    expect(equalPowerGains(0)).toEqual({ a: 1, b: 0 })
    expect(equalPowerGains(1).a).toBeCloseTo(0)
    expect(equalPowerGains(1).b).toBeCloseTo(1)
    expect(equalPowerGains(0.5).a).toBeCloseTo(Math.SQRT1_2)
    expect(equalPowerGains(0.5).b).toBeCloseTo(Math.SQRT1_2)
    expect(equalPowerGains(Number.NaN)).toEqual({ a: 0, b: 0 })
  })

  it('normalizes audible contribution and guards silence', () => {
    const centered = contributionBalance(
      { a: 0.5, b: 0.5 },
      { a: true, b: true },
      0.5,
    )
    expect(centered.a).toBeCloseTo(0.5)
    expect(centered.b).toBeCloseTo(0.5)
    expect(
      contributionBalance(
        { a: 0.8, b: 0.8 },
        { a: true, b: true },
        0,
      ),
    ).toEqual({ a: 1, b: 0 })
    expect(
      contributionBalance(
        { a: 1, b: 1 },
        { a: false, b: false },
        0.5,
      ),
    ).toEqual({ a: 0, b: 0 })
  })

  it('hard-gates an inaudible deck even while its smoothed level is hot', () => {
    const state = reduceVisualFrame(
      {
        smoothedMaster: 1,
        smoothedLevels: { a: 1, b: 1 },
        wash: { a: 0.5, b: 0.5 },
        pulse: { a: 0.5, b: 0.5 },
      },
      frame({
        decks: {
          a: { level: 1, audible: false, beat: CLOCK },
          b: { level: 1, audible: true, beat: CLOCK },
        },
      }),
    )

    expect(state.smoothedLevels.a).toBe(1)
    expect(state.wash.a).toBe(0)
    expect(state.pulse.a).toBe(0)
    expect(state.wash.b).toBe(1)
  })

  it('uses a quicker attack than release and clamps long frame gaps', () => {
    const attack = smoothLevel(0.5, 1, 0.05) - 0.5
    const release = 0.5 - smoothLevel(0.5, 0, 0.05)
    expect(attack).toBeGreaterThan(release)
    expect(smoothLevel(0, 1, 10)).toBeCloseTo(
      smoothLevel(0, 1, MAX_FRAME_DELTA_SECONDS),
    )
    expect(smoothLevel(0.4, 1, Number.NaN)).toBe(0.4)
  })

  it('derives beat time on either side of the anchor', () => {
    expect(secondsSinceBeat(10.1, CLOCK)).toBeCloseTo(0.1)
    expect(secondsSinceBeat(9.9, CLOCK)).toBeCloseTo(0.4)
    expect(secondsSinceBeat(null, CLOCK)).toBeNull()
    expect(secondsSinceBeat(10, null)).toBeNull()
    expect(
      secondsSinceBeat(10, { periodSeconds: 0, beatAtContext: 10 }),
    ).toBeNull()
  })

  it('peaks on the beat and follows the declared exponential decay', () => {
    expect(beatPulse(10, CLOCK)).toBe(1)
    expect(beatPulse(10 + PULSE_DECAY_SECONDS, CLOCK)).toBeCloseTo(Math.exp(-1))
    expect(beatPulse(10 + 3 * PULSE_DECAY_SECONDS, CLOCK)).toBeCloseTo(
      Math.exp(-3),
    )
    expect(beatPulse(10, null)).toBe(0)
  })

  it('keeps the master output as the total visual energy gate', () => {
    const hot: VisualFrameState = {
      smoothedMaster: 1,
      smoothedLevels: { a: 1, b: 1 },
      wash: { a: 0, b: 0 },
      pulse: { a: 0, b: 0 },
    }
    const audible = reduceVisualFrame(hot, frame())
    const silentMaster = reduceVisualFrame(
      { ...hot, smoothedMaster: 0 },
      frame({ masterLevel: 0 }),
    )

    expect(audible.wash.a).toBeCloseTo(0.5)
    expect(audible.wash.b).toBeCloseTo(0.5)
    expect(silentMaster.wash).toEqual({ a: 0, b: 0 })
    expect(silentMaster.pulse).toEqual({ a: 0, b: 0 })
  })

  it('keeps aligned clocks together and offset clocks independent', () => {
    const previous: VisualFrameState = {
      smoothedMaster: 1,
      smoothedLevels: { a: 1, b: 1 },
      wash: { a: 0, b: 0 },
      pulse: { a: 0, b: 0 },
    }
    const aligned = reduceVisualFrame(previous, frame())
    const offset = reduceVisualFrame(
      previous,
      frame({
        decks: {
          a: { level: 1, audible: true, beat: CLOCK },
          b: {
            level: 1,
            audible: true,
            beat: { periodSeconds: 0.5, beatAtContext: 9.75 },
          },
        },
      }),
    )

    expect(aligned.pulse.a).toBeCloseTo(aligned.pulse.b)
    expect(offset.pulse.a).toBeGreaterThan(offset.pulse.b)
  })

  it('produces bounded output for hostile input', () => {
    const state = reduceVisualFrame(INITIAL_VISUAL_FRAME_STATE, {
      ...frame(),
      deltaSeconds: Number.POSITIVE_INFINITY,
      contextTime: Number.NaN,
      crossfade: -100,
      masterLevel: 100,
      decks: {
        a: { level: 100, audible: true, beat: CLOCK },
        b: { level: Number.NaN, audible: true, beat: CLOCK },
      },
    })

    for (const value of [
      state.smoothedMaster,
      state.smoothedLevels.a,
      state.smoothedLevels.b,
      state.wash.a,
      state.wash.b,
      state.pulse.a,
      state.pulse.b,
    ]) {
      expect(Number.isFinite(value)).toBe(true)
      expect(value).toBeGreaterThanOrEqual(0)
      expect(value).toBeLessThanOrEqual(1)
    }
    expect(state.pulse).toEqual({ a: 0, b: 0 })
  })

  it('builds a half-strength static wash for reduced motion', () => {
    expect(reducedMotionWash(0, { a: true, b: true })).toEqual({
      a: REDUCED_MOTION_WASH,
      b: 0,
    })
    const centered = reducedMotionWash(0.5, { a: true, b: true })
    expect(centered.a).toBeCloseTo(REDUCED_MOTION_WASH / 2)
    expect(centered.b).toBeCloseTo(REDUCED_MOTION_WASH / 2)
    expect(reducedMotionWash(0.5, { a: false, b: false })).toEqual({
      a: 0,
      b: 0,
    })
  })
})
