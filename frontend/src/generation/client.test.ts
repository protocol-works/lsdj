import { afterEach, describe, expect, it, vi } from 'vitest'

import { postMagentaRender, postSa3Generate } from './client'

afterEach(() => vi.unstubAllGlobals())

describe('generation client', () => {
  it('posts SA3 JSON and returns the WAV container', async () => {
    const wav = new ArrayBuffer(4)
    const fetchMock = vi.fn(async () => ({
      ok: true,
      arrayBuffer: async () => wav,
    }))
    vi.stubGlobal('fetch', fetchMock)

    await expect(
      postSa3Generate({ prompt: 'dub', seconds: 60, kind: 'track', seed: 4 }),
    ).resolves.toBe(wav)
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/generate',
      expect.objectContaining({
        body: JSON.stringify({ prompt: 'dub', seconds: 60, kind: 'track', seed: 4 }),
      }),
    )
  })

  it('keeps Magenta on its option-free route and reports backend detail', async () => {
    const fetchMock = vi.fn(async () => ({
      ok: false,
      status: 502,
      json: async () => ({ detail: 'worker unavailable' }),
    }))
    vi.stubGlobal('fetch', fetchMock)

    await expect(postMagentaRender({ prompt: 'piano', seconds: 60 })).rejects.toThrow(
      'worker unavailable',
    )
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/render',
      expect.objectContaining({ body: JSON.stringify({ prompt: 'piano', seconds: 60 }) }),
    )
  })
})
