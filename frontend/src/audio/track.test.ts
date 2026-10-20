import { describe, expect, it } from 'vitest'

import {
  beatLoopRegion,
  BEND_CATCH_UP_SECONDS,
  bendConsumed,
  bendPlan,
  clampOffset,
  clampRate,
  foldIntoLoop,
  MAX_BEND_FRACTION,
  MIN_TRACK_LOOP_SECONDS,
  NUDGE_BEND_FRACTION,
  phaseOffsetBeats,
  planLoopSet,
  positionAt,
  quantisedLoop,
  resizeLoop,
  snapToGrid,
  syncRate,
  tempoSliderToRate,
  trackPeaks,
  TRACK_RATE_RANGE,
} from './track'

describe('positionAt', () => {
  it('holds the parked offset while paused', () => {
    expect(positionAt({ state: 'paused', offset: 12.5 }, 100, 60)).toBe(12.5)
  })

  it('advances with context time while playing', () => {
    const transport = {
      state: 'playing',
      offset: 10,
      startedAt: 50,
      rate: 1,
    } as const
    expect(positionAt(transport, 53.25, 60)).toBe(13.25)
  })

  it('advances at the varispeed rate (M20)', () => {
    const transport = {
      state: 'playing',
      offset: 10,
      startedAt: 50,
      rate: 1.05,
    } as const
    expect(positionAt(transport, 52, 60)).toBeCloseTo(12.1)
  })

  it('clamps to the track end — a source that ran past its stop never reads beyond', () => {
    const transport = {
      state: 'playing',
      offset: 55,
      startedAt: 0,
      rate: 1,
    } as const
    expect(positionAt(transport, 30, 60)).toBe(60)
  })

  it('parks at the end once ended', () => {
    expect(positionAt({ state: 'ended', offset: 60 }, 999, 60)).toBe(60)
  })
})

describe('clampRate', () => {
  it('passes rates inside the varispeed range', () => {
    expect(clampRate(1.05)).toBe(1.05)
  })

  it('clamps to the ±8% envelope and sends garbage to unity', () => {
    expect(clampRate(1.5)).toBe(1 + TRACK_RATE_RANGE)
    expect(clampRate(0.5)).toBe(1 - TRACK_RATE_RANGE)
    expect(clampRate(Number.NaN)).toBe(1)
  })

  it('holds whichever tempo range the performer chose', () => {
    expect(clampRate(1.3, 0.16)).toBe(1.16)
    expect(clampRate(1.3, 0.5)).toBe(1.3)
  })
})

describe('syncRate', () => {
  it('matches like-for-like readings directly', () => {
    expect(syncRate(120, 126)).toBeCloseTo(1.05)
  })

  it('folds a half- or double-time reading onto the other clock', () => {
    // A 70 BPM reading of a 140 BPM track syncs at unity, not ×2.
    expect(syncRate(70, 140)).toBeCloseTo(1)
    expect(syncRate(140, 70)).toBeCloseTo(1)
    expect(syncRate(66, 128)).toBeCloseTo(128 / 132)
  })

  it('picks the metrical level nearest unity when neither fits', () => {
    // 100 against 150: ×0.75 lands at half of 150, closer than ×1.5.
    expect(syncRate(100, 150)).toBeCloseTo(0.75)
  })
})

describe('bend arithmetic', () => {
  it('plans a single-tick slip at the gentle minimum bend', () => {
    const plan = bendPlan(0.01, 1)
    expect(plan.rate).toBeCloseTo(1 + NUDGE_BEND_FRACTION)
    // 10ms of slip at a 5% bend takes 200ms.
    expect(plan.durationSeconds).toBeCloseTo(0.2)
  })

  it('plans a backward slip as a slower bend', () => {
    const plan = bendPlan(-0.01, 1)
    expect(plan.rate).toBeCloseTo(1 - NUDGE_BEND_FRACTION)
    expect(plan.durationSeconds).toBeCloseTo(0.2)
  })

  it('steepens with the backlog so catch-up time stays bounded', () => {
    // 100ms of slip would crawl for 2s at the minimum bend; instead
    // the bend scales to clear it in the catch-up target.
    const moderate = bendPlan(0.1, 1)
    expect(moderate.rate).toBeCloseTo(1.4)
    expect(moderate.durationSeconds).toBeCloseTo(BEND_CATCH_UP_SECONDS)
    // A spin's backlog hits the steepness cap and takes longer, but
    // never the 20s a fixed 5% would have spent on a 1s slip.
    const spin = bendPlan(1, 1)
    expect(spin.rate).toBeCloseTo(1 + MAX_BEND_FRACTION)
    expect(spin.durationSeconds).toBeCloseTo(2)
  })

  it('scales the bend with the base rate so slip stays exact under varispeed', () => {
    const plan = bendPlan(0.01, 1.08)
    expect(plan.rate).toBeCloseTo(1.08 * (1 + NUDGE_BEND_FRACTION))
    expect(plan.durationSeconds).toBeCloseTo(0.01 / (1.08 * NUDGE_BEND_FRACTION))
  })

  it('settles what an interrupted bend already consumed', () => {
    const plan = bendPlan(0.01, 1)
    // Half the duration in: half the slip is consumed.
    expect(
      bendConsumed(plan.durationSeconds / 2, 1, plan.rate),
    ).toBeCloseTo(0.005)
  })
})

describe('foldIntoLoop', () => {
  const loop = { start: 8, end: 10 }

  it('passes a playhead before the loop end through linearly', () => {
    expect(foldIntoLoop(9.5, loop)).toBe(9.5)
    // Before the region entirely: still linear — the audio only wraps
    // once it reaches the end.
    expect(foldIntoLoop(3, loop)).toBe(3)
  })

  it('wraps at the end exactly like the source does', () => {
    expect(foldIntoLoop(10, loop)).toBe(8)
    expect(foldIntoLoop(10.5, loop)).toBeCloseTo(8.5)
    // Several laps in.
    expect(foldIntoLoop(15.1, loop)).toBeCloseTo(9.1)
  })

  it('folds a playhead a quantised OUT left behind', () => {
    // OUT snapped to 10 while the playhead was already at 10.3: the
    // source wraps on its next check; the math agrees.
    expect(foldIntoLoop(10.3, loop)).toBeCloseTo(8.3)
  })
})

describe('snapToGrid', () => {
  const grid = { bpm: 120, firstBeatSeconds: 0.25 }

  it('snaps to the nearest beat in both directions', () => {
    expect(snapToGrid(1.3, grid)).toBeCloseTo(1.25)
    expect(snapToGrid(1.45, grid)).toBeCloseTo(1.25)
    expect(snapToGrid(1.55, grid)).toBeCloseTo(1.75)
  })

  it('passes through free without a grid — the consumer rule', () => {
    expect(snapToGrid(1.3, null)).toBe(1.3)
  })

  it('never snaps before the top of the track', () => {
    expect(snapToGrid(0, { bpm: 120, firstBeatSeconds: 0.4 })).toBeCloseTo(0.4)
  })
})

describe('quantisedLoop', () => {
  const grid = { bpm: 120, firstBeatSeconds: 0 }

  it('snaps both ends onto the lattice', () => {
    expect(quantisedLoop(8.1, 10.2, grid, 60)).toEqual({ start: 8, end: 10 })
  })

  it('an OUT on the IN beat owes one beat — a loop must loop', () => {
    expect(quantisedLoop(8.1, 8.2, grid, 60)).toEqual({ start: 8, end: 8.5 })
  })

  it('runs free without a grid, refusing a backwards region', () => {
    expect(quantisedLoop(8.1, 10.2, null, 60)).toEqual({ start: 8.1, end: 10.2 })
    expect(quantisedLoop(10.2, 8.1, null, 60)).toBeNull()
  })

  it('refuses a free loop shorter than the honest minimum', () => {
    expect(quantisedLoop(8, 8.01, null, 60)).toBeNull()
    expect(quantisedLoop(8, 8 + MIN_TRACK_LOOP_SECONDS, null, 60)).toEqual({
      start: 8,
      end: 8 + MIN_TRACK_LOOP_SECONDS,
    })
  })

  it('clamps the end into the track and refuses a degenerate tail', () => {
    expect(quantisedLoop(58.9, 59.9, grid, 59.1)).toEqual({
      start: 59,
      end: 59.1,
    })
    expect(quantisedLoop(59.1, 59.2, null, 59.1)).toBeNull()
  })
})

describe('beatLoopRegion', () => {
  const grid = { bpm: 120, firstBeatSeconds: 0 }

  it('sets N whole beats from the snapped playhead', () => {
    // 8.1 s snaps to beat 16 (8.0 s); four beats at 120 BPM is 2 s.
    expect(beatLoopRegion(8.1, 4, grid, 60)).toEqual({ start: 8, end: 10 })
  })

  it('is inert without a grid — a beat loop is grid-defined', () => {
    expect(beatLoopRegion(8.1, 4, null, 60)).toBeNull()
  })

  it('refuses a non-positive beat count', () => {
    expect(beatLoopRegion(8, 0, grid, 60)).toBeNull()
  })

  it('clamps the end into the track, refusing a degenerate tail', () => {
    // Four beats would overrun a 59 s track — the region clamps to the end.
    expect(beatLoopRegion(58.1, 4, grid, 59)).toEqual({ start: 58, end: 59 })
    // Snapped onto the last beat, nothing honest is left to loop.
    expect(beatLoopRegion(58.9, 4, grid, 59)).toBeNull()
  })
})

describe('resizeLoop', () => {
  it('halves and doubles, anchored on the IN', () => {
    expect(resizeLoop({ start: 8, end: 10 }, 0.5, 60)).toEqual({
      start: 8,
      end: 9,
    })
    expect(resizeLoop({ start: 8, end: 10 }, 2, 60)).toEqual({
      start: 8,
      end: 12,
    })
  })

  it('keeps a clean beat fraction exact — no re-snap (M23)', () => {
    // A one-beat loop halved is half a beat, not dragged back to a whole.
    expect(resizeLoop({ start: 8, end: 8.5 }, 0.5, 60)).toEqual({
      start: 8,
      end: 8.25,
    })
  })

  it('needs no grid — scaling a free loop is honest arithmetic', () => {
    expect(resizeLoop({ start: 8.5, end: 10.5 }, 0.5, 60)).toEqual({
      start: 8.5,
      end: 9.5,
    })
  })

  it('refuses a double that would overrun the track', () => {
    expect(resizeLoop({ start: 8, end: 10 }, 2, 11)).toBeNull()
  })

  it('refuses a halve below the honest minimum', () => {
    expect(
      resizeLoop({ start: 8, end: 8 + MIN_TRACK_LOOP_SECONDS }, 0.5, 60),
    ).toBeNull()
  })
})

describe('planLoopSet', () => {
  it('refuses what cannot honestly loop', () => {
    expect(planLoopSet('playing', 5, Number.NaN, 10, 60)).toEqual({
      action: 'refuse',
    })
    expect(planLoopSet('playing', 5, -1, 10, 60)).toEqual({ action: 'refuse' })
    expect(planLoopSet('playing', 5, 8, 61, 60)).toEqual({ action: 'refuse' })
    expect(planLoopSet('playing', 5, 8, 8.01, 60)).toEqual({ action: 'refuse' })
  })

  it('refuses on a deck parked at its end — nothing is rolling', () => {
    expect(planLoopSet('ended', 60, 8, 10, 60)).toEqual({ action: 'refuse' })
  })

  it('restarts inside the region when a late OUT lands behind the playhead', () => {
    const plan = planLoopSet('playing', 10.3, 8, 10, 60)
    expect(plan.action).toBe('restart')
    expect((plan as { offset: number }).offset).toBeCloseTo(8.3)
  })

  it('restarts when a halve pulls the end behind the playhead (M23)', () => {
    // A 4-beat loop (0–2s) halved to 2 beats (0–1s) while the playhead
    // is audibly at 1.5s: resize feeds the new region through the same
    // path, and the restart rule re-fires inside it at the fold.
    const plan = planLoopSet('playing', 1.5, 0, 1, 60)
    expect(plan.action).toBe('restart')
    expect((plan as { offset: number }).offset).toBeCloseTo(0.5)
  })

  it('re-anchors a playing transport on the audible position', () => {
    expect(planLoopSet('playing', 9, 8, 10, 60)).toEqual({
      action: 'reanchor',
      offset: 9,
    })
  })

  it('parks a paused playhead past the region inside it', () => {
    expect(planLoopSet('paused', 30, 8, 10, 60)).toEqual({
      action: 'park',
      offset: 8,
    })
  })

  it('just applies for a paused playhead at or before the region', () => {
    expect(planLoopSet('paused', 5, 8, 10, 60)).toEqual({ action: 'apply' })
  })
})

describe('clampOffset', () => {
  it('passes a position inside the track through', () => {
    expect(clampOffset(30, 60)).toBe(30)
  })

  it('clamps past-the-end to the end', () => {
    expect(clampOffset(75, 60)).toBe(60)
  })

  it('sends negatives and non-finite garbage to the top', () => {
    expect(clampOffset(-3, 60)).toBe(0)
    expect(clampOffset(Number.NaN, 60)).toBe(0)
    expect(clampOffset(Infinity, 60)).toBe(0)
  })
})

describe('trackPeaks', () => {
  it('finds the min/max envelope per bucket across both channels', () => {
    // Float32-exact values, so the assertions compare cleanly.
    const left = Float32Array.from([0.5, -0.25, 0.125, 0.75])
    const right = Float32Array.from([-0.625, 0.25, -0.0625, 0.375])
    const { min, max } = trackPeaks(left, right, 2)
    expect(Array.from(min)).toEqual([-0.625, -0.0625])
    expect(Array.from(max)).toEqual([0.5, 0.75])
  })

  it('covers every frame when buckets do not divide the length', () => {
    const left = Float32Array.from([0, 0, 1])
    const right = Float32Array.from([0, 0, -1])
    const { min, max } = trackPeaks(left, right, 2)
    // The last frame's extremes must land in the final bucket.
    expect(max[1]).toBe(1)
    expect(min[1]).toBe(-1)
  })

  it('returns silent envelopes for an empty buffer', () => {
    const { min, max } = trackPeaks(new Float32Array(0), new Float32Array(0), 3)
    expect(Array.from(min)).toEqual([0, 0, 0])
    expect(Array.from(max)).toEqual([0, 0, 0])
  })
})

describe('tempoSliderToRate', () => {
  it('maps centre to unity and the ends to the envelope edges (orientation measured on device)', () => {
    expect(tempoSliderToRate(0.5)).toBeCloseTo(1)
    expect(tempoSliderToRate(0)).toBeCloseTo(1 - TRACK_RATE_RANGE)
    expect(tempoSliderToRate(1)).toBeCloseTo(1 + TRACK_RATE_RANGE)
  })

  it('spans the chosen tempo range', () => {
    expect(tempoSliderToRate(0, 0.16)).toBeCloseTo(0.84)
    expect(tempoSliderToRate(1, 0.16)).toBeCloseTo(1.16)
  })
})

describe('phaseOffsetBeats', () => {
  const clock = (beatAtContext: number, periodSeconds = 0.5) => ({
    beatAtContext,
    periodSeconds,
  })

  it('is zero when the beats coincide on the lattice', () => {
    expect(phaseOffsetBeats(clock(10), clock(10))).toBeCloseTo(0)
    expect(phaseOffsetBeats(clock(12.5), clock(10))).toBeCloseTo(0)
  })

  it('reports a late deck as a positive fraction of a beat', () => {
    expect(phaseOffsetBeats(clock(10.1), clock(10))).toBeCloseTo(0.2)
  })

  it('wraps to the nearest direction', () => {
    expect(phaseOffsetBeats(clock(10.45), clock(10))).toBeCloseTo(-0.1)
  })
})
