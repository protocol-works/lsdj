import { afterEach, describe, expect, it, vi } from 'vitest'

import { postMagentaRender, postSa3Generate, startSa3Generate } from './client'

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

  it('isolates job status and sends authenticated cancellation before aborting', async () => {
    let finishGenerate!: (response: Response) => void
    const statuses: string[] = []
    const fetchMock = vi.fn((path: string, _init?: RequestInit) => {
      void _init
      if (path === '/api/generate') {
        return new Promise<Response>((resolve) => {
          finishGenerate = resolve
        })
      }
      if (path.endsWith('/cancel')) {
        return Promise.resolve({ ok: true, status: 200 } as Response)
      }
      return Promise.resolve({
        ok: true,
        json: async () => ({
          jobId: path.split('/').at(-1),
          state: 'running',
          progress: null,
          detail: null,
        }),
      } as Response)
    })
    vi.stubGlobal('fetch', fetchMock)

    const task = startSa3Generate(
      { prompt: 'dub', seconds: 60, kind: 'track', seed: 4 },
      (status) => statuses.push(status.state),
    )
    await vi.waitFor(() => {
      expect(fetchMock.mock.calls.some(([path]) => path === '/api/generate')).toBe(true)
    })
    const generateCall = fetchMock.mock.calls.find(([path]) => path === '/api/generate')!
    expect((generateCall[1]?.headers as Headers).get('x-lsdj-job-id')).toBe(task.jobId)

    await task.cancel()
    expect(task.wasCancelled()).toBe(true)
    expect(
      fetchMock.mock.calls.some(
        ([path, init]) => path === `/api/jobs/${task.jobId}/cancel` && init?.method === 'POST',
      ),
    ).toBe(true)
    expect((generateCall[1]?.signal as AbortSignal).aborted).toBe(true)

    finishGenerate({
      ok: true,
      arrayBuffer: async () => new ArrayBuffer(4),
    } as Response)
    await task.result
    expect(statuses).toContain('running')
  })
})
