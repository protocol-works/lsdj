import { describe, expect, it } from 'vitest'

import {
  barAt,
  barSeconds,
  barStartSeconds,
  barsToSeam,
  fmtClock,
  holdRegion,
  lateBy,
  loopRegion,
  nextBarSeconds,
  nodeEndSeconds,
  nodeStartSeconds,
  renderEta,
  type NodeSpan,
} from './timing'

// 120 BPM: a bar is 2 s. A child with a 4-bar (8 s) kept tail and a 16-bar body.
const child = (patch: Partial<NodeSpan> = {}): NodeSpan => ({
  bpm: 120,
  tailSeconds: 8,
  in: 0,
  out: 16,
  durationSeconds: 40,
  ...patch,
})

describe('the nominal grid', () => {
  it('has bars of four beats', () => {
    expect(barSeconds(120)).toBe(2)
    expect(barSeconds(124)).toBeCloseTo(1.9355, 4)
  })

  it('starts a node at bar 0 after its kept tail, plus the in-point', () => {
    expect(nodeStartSeconds(child())).toBe(8)
    expect(nodeStartSeconds(child({ in: 3 }))).toBe(14)
  })

  it('ends a node at its out-point', () => {
    expect(nodeEndSeconds(child())).toBe(40)
    expect(nodeEndSeconds(child({ out: 12 }))).toBe(32)
  })

  it('clamps the end of a clip shorter than its out-point to the clip', () => {
    expect(nodeEndSeconds(child({ durationSeconds: 39.98 }))).toBe(39.98)
  })

  it('leaves the end unclamped while the clip is not measured yet', () => {
    expect(nodeEndSeconds(child({ durationSeconds: null }))).toBe(40)
  })

  it('maps clip time to bars and back', () => {
    expect(barAt(child(), 13)).toBe(2.5)
    expect(barStartSeconds(child(), 2.5)).toBe(13)
  })

  it('reads the kept tail as negative bars and floors the join at the clip start', () => {
    expect(barAt(child(), 4)).toBe(-2)
    expect(barStartSeconds(child(), -4)).toBe(0)
    expect(barStartSeconds(child({ tailSeconds: 0 }), -4)).toBe(0)
  })
})

describe('nextBarSeconds', () => {
  it('lands a cut on the next bar line', () => {
    expect(nextBarSeconds(child(), 13)).toBe(14)
  })

  it('moves on a full bar from exactly on a bar line', () => {
    expect(nextBarSeconds(child(), 14)).toBe(16)
    // 124 BPM bar lines are not exact in floating point.
    const n = child({ bpm: 124, tailSeconds: 0 })
    expect(nextBarSeconds(n, 7 * barSeconds(124))).toBeCloseTo(8 * barSeconds(124), 9)
  })

  it('never passes the clip end', () => {
    expect(nextBarSeconds(child({ durationSeconds: 39.5 }), 39)).toBe(39.5)
  })
})

describe('loops', () => {
  it('holds the last four bars before the out-point', () => {
    expect(holdRegion(child())).toEqual({ start: 32, end: 40 })
    expect(holdRegion(child({ out: 12 }))).toEqual({ start: 24, end: 32 })
  })

  it('keeps a hold four bars long when the clip comes back short', () => {
    const { start, end } = holdRegion(child({ durationSeconds: 39.9 }))
    expect(end).toBe(39.9)
    expect(end - start).toBeCloseTo(8, 9)
  })

  it('loops from a bar', () => {
    expect(loopRegion(child(), 4, 8)).toEqual({ start: 16, end: 32 })
  })

  it('slides a loop that would cross the out-point back inside the body', () => {
    expect(loopRegion(child(), 14, 8)).toEqual({ start: 24, end: 40 })
  })

  it('never starts a loop before the in-point', () => {
    expect(loopRegion(child({ in: 2 }), -1, 4)).toEqual({ start: 12, end: 20 })
  })

  it('shortens a loop longer than the body to the body', () => {
    expect(loopRegion(child({ in: 8, out: 12 }), 8, 16)).toEqual({ start: 24, end: 32 })
  })
})

describe('the countdown', () => {
  it('counts the bars left to the seam, reading 0 only at the seam', () => {
    expect(barsToSeam(child(), 8)).toBe(16)
    expect(barsToSeam(child(), 8.5)).toBe(16)
    expect(barsToSeam(child(), 38)).toBe(1)
    expect(barsToSeam(child(), 40)).toBe(0)
    expect(barsToSeam(child(), 41)).toBe(0)
  })

  it('counts to the clamped end of a short clip', () => {
    expect(barsToSeam(child({ durationSeconds: 30 }), 26)).toBe(2)
  })

  it('reports how late a render is for the seam', () => {
    expect(lateBy(20, 12)).toBe(8)
    expect(lateBy(7, 12)).toBe(0)
  })
})

describe('renderEta', () => {
  const lora = [{ name: 'industrial-techno', strength: 1 }]

  it('puts a plain 16-bar continuation at 124 BPM at about 7–8 s', () => {
    const eta = renderEta({ bars: 16, tailBars: 4, bpm: 124, loras: [] })
    expect(eta).toBeGreaterThan(7)
    expect(eta).toBeLessThan(8)
  })

  it('puts a plain 16-bar root at 124 BPM just under 7 s', () => {
    expect(renderEta({ bars: 16, tailBars: 0, bpm: 124, loras: [] })).toBeCloseTo(6.87, 2)
  })

  it('adds the adapter merge for a LoRA, about 135 s in all', () => {
    expect(renderEta({ bars: 16, tailBars: 4, bpm: 124, loras: lora })).toBeCloseTo(135, -1)
  })

  it('charges the merge per adapter', () => {
    const one = renderEta({ bars: 16, tailBars: 4, bpm: 124, loras: lora })
    const two = renderEta({ bars: 16, tailBars: 4, bpm: 124, loras: [...lora, { name: 'opera', strength: 0.5 }] })
    expect(two - one).toBeCloseTo(128, 9)
  })
})

describe('fmtClock', () => {
  it('formats m:ss, rounded and never negative', () => {
    expect(fmtClock(0)).toBe('0:00')
    expect(fmtClock(6.87)).toBe('0:07')
    expect(fmtClock(59.6)).toBe('1:00')
    expect(fmtClock(134.9)).toBe('2:15')
    expect(fmtClock(-3)).toBe('0:00')
  })
})
