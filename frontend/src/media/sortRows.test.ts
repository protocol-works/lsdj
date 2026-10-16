import { describe, expect, it } from 'vitest'

import { sortRows } from './sortRows'

type Row = { id: number; state: 'pending' | 'ready'; title: string; rating?: number }

// Newest-first, as the explorer holds them; one pending row mid-list.
const rows: Row[] = [
  { id: 1, state: 'ready', title: 'beta', rating: 3 },
  { id: 2, state: 'ready', title: 'Alpha', rating: 0 },
  { id: 3, state: 'pending', title: 'zulu' },
  { id: 4, state: 'ready', title: 'gamma', rating: 5 },
  { id: 5, state: 'ready', title: 'alpha', rating: 3 },
]
const ids = (sorted: Row[]) => sorted.map((row) => row.id)

describe('sortRows', () => {
  it('keeps newest-first, pinning pending rows to the top', () => {
    expect(ids(sortRows(rows, 'newest'))).toEqual([3, 1, 2, 4, 5])
  })

  it('sorts by rating descending with unrated last and ties newest-first', () => {
    expect(ids(sortRows(rows, 'rating'))).toEqual([3, 4, 1, 5, 2])
  })

  it('sorts by title case-insensitively with ties newest-first', () => {
    expect(ids(sortRows(rows, 'title'))).toEqual([3, 2, 5, 1, 4])
  })

  it('treats an absent rating as unrated and leaves the input untouched', () => {
    const input: Row[] = [
      { id: 1, state: 'ready', title: 'a' },
      { id: 2, state: 'ready', title: 'b', rating: 1 },
    ]
    expect(ids(sortRows(input, 'rating'))).toEqual([2, 1])
    expect(ids(input)).toEqual([1, 2])
  })
})

describe('sortRows by analysis', () => {
  type Analysed = { id: number; state: 'ready'; title: string; bpm?: number; key?: string }
  const analysed: Analysed[] = [
    { id: 1, state: 'ready', title: 'a', bpm: 128, key: '10A' },
    { id: 2, state: 'ready', title: 'b' },
    { id: 3, state: 'ready', title: 'c', bpm: 96, key: '8B' },
    { id: 4, state: 'ready', title: 'd', bpm: 128, key: '8A' },
    { id: 5, state: 'ready', title: 'e', key: '1B' },
  ]

  it('sorts by BPM ascending with unknown last and ties newest-first', () => {
    expect(sortRows(analysed, 'bpm').map((row) => row.id)).toEqual([3, 1, 4, 2, 5])
  })

  it('sorts by Camelot wheel position (1A … 12B) with unknown last', () => {
    expect(sortRows(analysed, 'key').map((row) => row.id)).toEqual([5, 4, 3, 1, 2])
  })
})
