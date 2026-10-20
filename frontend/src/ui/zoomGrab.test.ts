import { describe, expect, it } from 'vitest'

import {
  GRAB_MAX_BEND,
  MAX_HOPS,
  dragHops,
  grabNudgeSeconds,
  zoomWindowHops,
} from './zoomGrab'

describe('zoomWindowHops', () => {
  it('spans the window in hops, capped by the scratch buffer', () => {
    expect(zoomWindowHops(0.01)).toBe(400)
    expect(zoomWindowHops(0.0001)).toBe(MAX_HOPS - 1)
  })
})

describe('dragHops', () => {
  it('maps the full strip to the full window', () => {
    expect(dragHops(-720, 720, 400)).toBe(400)
  })

  it('pulls later audio when dragged against the scroll', () => {
    expect(dragHops(-36, 720, 400)).toBeGreaterThan(0)
    expect(dragHops(36, 720, 400)).toBeLessThan(0)
  })

  it('is inert on a zero-size strip', () => {
    expect(dragHops(10, 0, 400)).toBe(0)
  })
})

describe('grabNudgeSeconds', () => {
  it('passes a gentle drag through untouched', () => {
    expect(grabNudgeSeconds(0.0005, 0.016, 1)).toBe(0.0005)
    expect(grabNudgeSeconds(-0.0005, 0.016, 1)).toBe(-0.0005)
  })

  it('caps a fast drag at the max bend for the elapsed time', () => {
    expect(grabNudgeSeconds(0.5, 0.016, 1)).toBeCloseTo(GRAB_MAX_BEND * 0.016)
    expect(grabNudgeSeconds(-0.5, 0.016, 1)).toBeCloseTo(-GRAB_MAX_BEND * 0.016)
  })

  it('scales the cap with the set rate', () => {
    expect(grabNudgeSeconds(0.5, 0.016, 1.08)).toBeCloseTo(
      GRAB_MAX_BEND * 1.08 * 0.016,
    )
  })

  it('banks no allowance across a long pause', () => {
    expect(grabNudgeSeconds(1, 2, 1)).toBeCloseTo(GRAB_MAX_BEND * 0.05)
  })

  it('ignores garbage', () => {
    expect(grabNudgeSeconds(Number.NaN, 0.016, 1)).toBe(0)
    expect(grabNudgeSeconds(0.01, 0, 1)).toBe(0)
  })
})
