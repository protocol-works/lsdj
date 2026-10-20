import { describe, expect, it } from 'vitest'

import { moveEdge, trimSilence } from './crop'

function peaksOf(levels: number[]) {
  return {
    min: Float32Array.from(levels, (level) => -level),
    max: Float32Array.from(levels),
  }
}

describe('trimSilence', () => {
  it('moves both edges past leading and trailing near-silence', () => {
    // 10 buckets over 10 s: sound in buckets 2..6.
    const peaks = peaksOf([0, 0.001, 0.5, 0.2, 0.3, 0.1, 0.4, 0.005, 0, 0])
    expect(trimSilence(peaks, 10)).toEqual({ start: 2, end: 7 })
  })

  it('keeps the whole clip when there is no silence to trim', () => {
    expect(trimSilence(peaksOf([0.5, 0.5]), 4)).toEqual({ start: 0, end: 4 })
  })

  it('gives up on an all-silent clip', () => {
    expect(trimSilence(peaksOf([0, 0.001, 0]), 3)).toBeNull()
  })
})

describe('moveEdge', () => {
  const crop = { start: 1, end: 3 }

  it('clamps each edge inside the clip and short of the other edge', () => {
    expect(moveEdge(crop, 'start', -2, 5)).toEqual({ start: 0, end: 3 })
    expect(moveEdge(crop, 'start', 4, 5)).toEqual({ start: 2.95, end: 3 })
    expect(moveEdge(crop, 'end', 9, 5)).toEqual({ start: 1, end: 5 })
    expect(moveEdge(crop, 'end', 0, 5)).toEqual({ start: 1, end: 1.05 })
  })

  it('ignores a value that is not a number', () => {
    expect(moveEdge(crop, 'start', Number.NaN, 5)).toBe(crop)
  })
})
