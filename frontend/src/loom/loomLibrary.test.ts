import { beforeEach, describe, expect, it, vi } from 'vitest'

const invokeMock = vi.fn<(cmd: string, args?: unknown) => Promise<unknown>>(async () => undefined)

vi.mock('../audio/nativeEngine', () => ({
  invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
}))

import { auditionSongClip, deleteLoom, listLooms, readLoom, saveLoom } from './loomLibrary'

describe('loomLibrary', () => {
  beforeEach(() => invokeMock.mockClear())

  it('lists looms with no args and passes the summaries through', async () => {
    const rows = [{ id: 'abcd1234', name: 'Set', updatedAt: 2, nodes: 3 }]
    invokeMock.mockResolvedValueOnce(rows)
    await expect(listLooms()).resolves.toEqual(rows)
    expect(invokeMock).toHaveBeenCalledWith('list_looms', undefined)
  })

  it('reads, saves and deletes a loom by id', async () => {
    const loom = { version: 1, nodes: [] }
    await readLoom('abcd1234')
    await saveLoom('abcd1234', 'Warm-up', loom)
    await deleteLoom('abcd1234')
    expect(invokeMock.mock.calls).toEqual([
      ['read_loom', { id: 'abcd1234' }],
      ['save_loom', { id: 'abcd1234', name: 'Warm-up', loom }],
      ['delete_loom', { id: 'abcd1234' }],
    ])
  })

  it('cues a song window with camelCase seconds and a null open end', async () => {
    await auditionSongClip('Dub.wav', 4, 12)
    await auditionSongClip('Dub.wav', 8)
    expect(invokeMock.mock.calls).toEqual([
      ['audition_song_clip', { name: 'Dub.wav', startSeconds: 4, endSeconds: 12 }],
      ['audition_song_clip', { name: 'Dub.wav', startSeconds: 8, endSeconds: null }],
    ])
  })
})
