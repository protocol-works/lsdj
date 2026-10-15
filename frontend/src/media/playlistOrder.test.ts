import { describe, expect, it } from 'vitest'

import { insertFile, moveFile, removeFile } from './playlistOrder'

describe('playlist order helpers', () => {
  const files = Object.freeze(['a.wav', 'b.wav', 'c.wav']) as string[]

  it('appends a new file by default and inserts at an index', () => {
    expect(insertFile(files, 'd.wav')).toEqual(['a.wav', 'b.wav', 'c.wav', 'd.wav'])
    expect(insertFile(files, 'd.wav', 1)).toEqual(['a.wav', 'd.wav', 'b.wav', 'c.wav'])
    expect(insertFile(files, 'd.wav', 0)).toEqual(['d.wav', 'a.wav', 'b.wav', 'c.wav'])
  })

  it('leaves a file already present where it is', () => {
    expect(insertFile(files, 'b.wav', 0)).toEqual(['a.wav', 'b.wav', 'c.wav'])
  })

  it('moves a file up, down, and across the list', () => {
    expect(moveFile(files, 1, 0)).toEqual(['b.wav', 'a.wav', 'c.wav'])
    expect(moveFile(files, 1, 2)).toEqual(['a.wav', 'c.wav', 'b.wav'])
    expect(moveFile(files, 0, 2)).toEqual(['b.wav', 'c.wav', 'a.wav'])
    expect(moveFile(files, 2, 0)).toEqual(['c.wav', 'a.wav', 'b.wav'])
  })

  it('removes a file and ignores one that is absent', () => {
    expect(removeFile(files, 'b.wav')).toEqual(['a.wav', 'c.wav'])
    expect(removeFile(files, 'z.wav')).toEqual(['a.wav', 'b.wav', 'c.wav'])
  })

  it('never mutates its input', () => {
    // Frozen: an in-place edit would throw.
    insertFile(files, 'd.wav', 1)
    moveFile(files, 0, 2)
    removeFile(files, 'a.wav')
    expect(files).toEqual(['a.wav', 'b.wav', 'c.wav'])
  })
})
