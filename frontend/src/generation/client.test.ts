import { afterEach, describe, expect, it, vi } from 'vitest'

import { postMagentaRender, postSa3Generate, postSa3GenerateWithSource } from './client'

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

  it('posts SA3 audio-to-audio as the request JSON plus the init WAV', async () => {
    const fetchMock = vi.fn(async () => ({ ok: true, arrayBuffer: async () => new ArrayBuffer(4) }))
    vi.stubGlobal('fetch', fetchMock)
    const request = {
      prompt: 'dub',
      seconds: 120,
      kind: 'track' as const,
      seed: 4,
      inpaint_range: [60, 120] as [number, number],
    }

    await postSa3GenerateWithSource(request, new ArrayBuffer(48))
    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit]
    expect(url).toBe('/api/generate')
    const form = init.body as FormData
    expect(JSON.parse(form.get('request') as string)).toEqual(request)
    expect((form.get('init_audio') as File).size).toBe(48)
    expect(init.headers).toBeUndefined()
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
