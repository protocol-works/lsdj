import { describe, expect, it } from 'vitest'

import { keyHue } from './keyHue'

describe('keyHue', () => {
  it('walks the circle of fifths a Camelot number at a time', () => {
    expect(keyHue('8A')).toBe(0)
    expect(keyHue('9A')).toBe(30)
    expect(keyHue('7A')).toBe(330)
    expect(keyHue('12B')).toBe(120)
    expect(keyHue('1A')).toBe(150)
  })

  it('gives a relative minor and major the same hue', () => {
    for (let n = 1; n <= 12; n++) expect(keyHue(`${n}A`)).toBe(keyHue(`${n}B`))
  })

  it('is null for an unknown or missing key', () => {
    for (const key of [undefined, null, '', '0A', '13B', '8C', 'Am', '8']) {
      expect(keyHue(key)).toBeNull()
    }
  })
})
