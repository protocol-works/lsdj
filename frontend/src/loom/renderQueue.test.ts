import { afterEach, describe, expect, it, vi } from 'vitest'

import { postSa3Generate } from '../generation/client'
import type { NewSong, SongEntry } from '../generation/library'
import { clipSeconds, type TrackGenerationRequest } from '../generation/songGeneration'
import { fixtureNode } from './fixture'
import { createRenderQueue, wavDurationSeconds, type RenderQueueDeps } from './renderQueue'

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

/** A PCM WAV of `seconds`, with an optional chunk before `data` (SA3's CLI
 * may write one; the parser must walk past it). */
function wav(seconds: number, opts: { rate?: number; channels?: number; bytes?: number; list?: boolean } = {}) {
  const { rate = 1000, channels = 2, bytes = 2, list = false } = opts
  const blockAlign = channels * bytes
  const dataBytes = Math.round(seconds * rate) * blockAlign
  const listBytes = list ? 12 : 0
  const buffer = new ArrayBuffer(44 + listBytes + dataBytes)
  const view = new DataView(buffer)
  const ascii = (at: number, text: string) => [...text].forEach((c, i) => view.setUint8(at + i, c.charCodeAt(0)))
  ascii(0, 'RIFF')
  view.setUint32(4, buffer.byteLength - 8, true)
  ascii(8, 'WAVE')
  ascii(12, 'fmt ')
  view.setUint32(16, 16, true)
  view.setUint16(20, bytes === 4 ? 3 : 1, true)
  view.setUint16(22, channels, true)
  view.setUint32(24, rate, true)
  view.setUint32(28, rate * blockAlign, true)
  view.setUint16(32, blockAlign, true)
  view.setUint16(34, bytes * 8, true)
  if (list) {
    ascii(36, 'LIST')
    view.setUint32(40, 4, true)
    ascii(44, 'INFO')
  }
  ascii(36 + listBytes, 'data')
  view.setUint32(40 + listBytes, dataBytes, true)
  return buffer
}

/** The shell's source clip: 44.1 kHz 16-bit stereo behind a 44-byte header. */
const shellClip = (seconds: number) => wav(seconds, { rate: 44_100 })

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (error: Error) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

function fakeDeps(overrides: Partial<RenderQueueDeps> = {}) {
  return {
    sourceClip: vi.fn(async () => shellClip(8)),
    generate: vi.fn(async () => wav(32)),
    generateWithSource: vi.fn(async () => wav(24, { rate: 48_000, channels: 1, bytes: 4, list: true })),
    save: vi.fn(async (meta: NewSong): Promise<SongEntry> => ({
      file: `${meta.title}.wav`,
      title: meta.title,
      prompt: null,
      model: 'track',
    })),
    onDone: vi.fn(),
    onError: vi.fn(),
    ...overrides,
  }
}

// 120 BPM: a bar is 2 s, so four bars are 8 s and sixteen are 32 s.
const root = fixtureNode('r', null, 'ambient dub, tape delay', 16, 120, {
  seed: 42,
  file: null,
  durationSeconds: null,
  status: 'rendering',
})
// Trimmed to 12 bars: the out-point, not the clip end, is where the child continues.
const parent = fixtureNode('p', null, 'ambient dub', 16, 120, { out: 12 })
const child = fixtureNode('c', 'p', 'dub techno', 8, 120, {
  seed: 7,
  file: null,
  durationSeconds: null,
  tailSeconds: 0,
  status: 'rendering',
})

const settle = () => new Promise((resolve) => setTimeout(resolve, 0))

describe('render queue', () => {
  it('renders a root from its BPM-stamped prompt and fixed seed, and saves it as a track', async () => {
    const deps = fakeDeps()
    createRenderQueue(deps).enqueue({ nodeId: 'r', node: root, parent: null })
    await vi.waitFor(() => expect(deps.onDone).toHaveBeenCalled())

    expect(deps.generate).toHaveBeenCalledWith({
      prompt: 'ambient dub, tape delay, 120 BPM',
      seconds: 32,
      kind: 'track',
      seed: 42,
    })
    const [meta] = vi.mocked(deps.save).mock.calls[0]
    expect(meta).toMatchObject({
      title: 'ambient dub, tape delay',
      prompt: 'ambient dub, tape delay, 120 BPM',
      model: 'track',
      recipe: { version: 1, engine: 'track', prompt: 'ambient dub, tape delay, 120 BPM', seconds: 32 },
    })
    expect(meta.recipe.sa3?.seed).toBe(42)
    expect(deps.onDone).toHaveBeenCalledWith('r', 'ambient dub, tape delay.wav', 32)
  })

  it("continues a child from the parent's last four bars before its out-point", async () => {
    const deps = fakeDeps()
    createRenderQueue(deps).enqueue({ nodeId: 'c', node: child, parent })
    await vi.waitFor(() => expect(deps.onDone).toHaveBeenCalled())

    expect(deps.sourceClip).toHaveBeenCalledWith('p.wav', 16, 24)
    const [request, clip] = vi.mocked(deps.generateWithSource).mock.calls[0] as unknown as [
      TrackGenerationRequest,
      ArrayBuffer,
    ]
    expect(request).toMatchObject({ prompt: 'dub techno, 120 BPM', seconds: 24, inpaint_range: [8, 24], seed: 7 })
    expect(clipSeconds(clip)).toBe(8)
    expect(deps.generate).not.toHaveBeenCalled()
    expect(vi.mocked(deps.save).mock.calls[0][0].recipe.source).toEqual({
      file: 'p.wav',
      mode: 'continue',
      start: 16,
      end: 24,
      inpaintRange: [8, 24],
    })
    // The output is 48 kHz float with a LIST chunk: its duration comes from the header.
    expect(deps.onDone).toHaveBeenCalledWith('c', 'dub techno.wav', 24)
  })

  it('passes a LoRA stack through unchanged', async () => {
    const deps = fakeDeps()
    const loras = [
      { name: 'industrial-techno', strength: 1 },
      { name: 'colundi-idm', strength: 0.8 },
    ]
    createRenderQueue(deps).enqueue({ nodeId: 'r', node: { ...root, loras }, parent: null })
    await vi.waitFor(() => expect(deps.onDone).toHaveBeenCalled())

    expect(vi.mocked(deps.generate).mock.calls[0]).toEqual([expect.objectContaining({ loras })])
    expect(vi.mocked(deps.save).mock.calls[0][0].recipe.loras).toEqual(loras)
  })

  it('sends no guidance fields when the node sets none', async () => {
    const deps = fakeDeps()
    createRenderQueue(deps).enqueue({ nodeId: 'r', node: root, parent: null })
    await vi.waitFor(() => expect(deps.onDone).toHaveBeenCalled())

    const [request] = vi.mocked(deps.generate).mock.calls[0] as unknown as [TrackGenerationRequest]
    expect(request).not.toHaveProperty('cfg')
    expect(request).not.toHaveProperty('apg')
    expect(request).not.toHaveProperty('negative_prompt')
  })

  it("turns guidance on with the node's cfg, defaulting apg", async () => {
    const deps = fakeDeps()
    const node = { ...root, sa3: { cfg: 2, negativePrompt: 'vocals' } }
    createRenderQueue(deps).enqueue({ nodeId: 'r', node, parent: null })
    await vi.waitFor(() => expect(deps.onDone).toHaveBeenCalled())

    expect(vi.mocked(deps.generate).mock.calls[0]).toEqual([
      expect.objectContaining({ cfg: 2, apg: 1, negative_prompt: 'vocals' }),
    ])
  })

  it('names takes from the prompt cut at a word, with the take number', async () => {
    const deps = fakeDeps()
    const node = { ...root, prompt: 'deep minimal techno, hypnotic, sub pulse' }
    createRenderQueue(deps).enqueue({ nodeId: 'r', node, parent: null, take: 2 })
    await vi.waitFor(() => expect(deps.onDone).toHaveBeenCalled())

    expect(vi.mocked(deps.save).mock.calls[0][0].title).toBe('deep minimal techno · take 2')
  })

  it('runs one job at a time, first in first out', async () => {
    const renders = [deferred<ArrayBuffer>(), deferred<ArrayBuffer>(), deferred<ArrayBuffer>()]
    let call = 0
    const deps = fakeDeps({ generate: vi.fn(() => renders[call++].promise) })
    const queue = createRenderQueue(deps)
    for (const id of ['a', 'b', 'c']) queue.enqueue({ nodeId: id, node: { ...root, id }, parent: null })

    expect(deps.generate).toHaveBeenCalledTimes(1)
    expect(queue.pending()).toEqual(['a', 'b', 'c'])
    renders[0].resolve(wav(32))
    await vi.waitFor(() => expect(deps.generate).toHaveBeenCalledTimes(2))
    expect(vi.mocked(deps.onDone).mock.calls.map(([id]) => id)).toEqual(['a'])
    renders[1].resolve(wav(32))
    renders[2].resolve(wav(32))
    await vi.waitFor(() => expect(deps.onDone).toHaveBeenCalledTimes(3))
    expect(vi.mocked(deps.onDone).mock.calls.map(([id]) => id)).toEqual(['a', 'b', 'c'])
    expect(queue.pending()).toEqual([])
  })

  it('drops a cancelled job that has not started', async () => {
    const first = deferred<ArrayBuffer>()
    const deps = fakeDeps({ generate: vi.fn().mockReturnValueOnce(first.promise).mockResolvedValue(wav(32)) })
    const queue = createRenderQueue(deps)
    queue.enqueue({ nodeId: 'a', node: root, parent: null })
    queue.enqueue({ nodeId: 'b', node: root, parent: null })
    queue.cancel('b')

    expect(queue.pending()).toEqual(['a'])
    first.resolve(wav(32))
    await vi.waitFor(() => expect(deps.onDone).toHaveBeenCalledWith('a', expect.any(String), 32))
    await settle()
    expect(deps.generate).toHaveBeenCalledTimes(1)
    expect(deps.onError).not.toHaveBeenCalled()
  })

  it('still saves a cancelled in-flight render but reports nothing', async () => {
    const render = deferred<ArrayBuffer>()
    const deps = fakeDeps({ generate: vi.fn(() => render.promise) })
    const queue = createRenderQueue(deps)
    queue.enqueue({ nodeId: 'a', node: root, parent: null })
    queue.cancel('a')

    expect(queue.pending()).toEqual([])
    render.resolve(wav(32))
    await vi.waitFor(() => expect(deps.save).toHaveBeenCalled())
    await settle()
    expect(deps.onDone).not.toHaveBeenCalled()
    expect(deps.onError).not.toHaveBeenCalled()
  })

  it("reports the backend's detail and moves on to the next job", async () => {
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockResolvedValueOnce({ ok: false, status: 422, json: async () => ({ detail: "'seconds' must be 1-380" }) })
        .mockResolvedValue({ ok: true, arrayBuffer: async () => wav(32) }),
    )
    const deps = fakeDeps({ generate: postSa3Generate })
    const queue = createRenderQueue(deps)
    queue.enqueue({ nodeId: 'a', node: root, parent: null })
    queue.enqueue({ nodeId: 'b', node: root, parent: null })

    await vi.waitFor(() => expect(deps.onDone).toHaveBeenCalledWith('b', expect.any(String), 32))
    expect(deps.onError).toHaveBeenCalledWith('a', "'seconds' must be 1-380")
    expect(deps.save).toHaveBeenCalledTimes(1)
  })

  it('turns a request it cannot build into an error, without posting', async () => {
    const deps = fakeDeps()
    createRenderQueue(deps).enqueue({ nodeId: 'r', node: { ...root, seed: -1 }, parent: null })
    await vi.waitFor(() => expect(deps.onError).toHaveBeenCalledWith('r', 'invalid SA3 seed'))
    expect(deps.generate).not.toHaveBeenCalled()
  })

  it('ticks progress with the elapsed time and the render estimate', async () => {
    vi.useFakeTimers()
    const render = deferred<ArrayBuffer>()
    const onProgress = vi.fn()
    const deps = fakeDeps({ generate: vi.fn(() => render.promise), onProgress })
    createRenderQueue(deps).enqueue({ nodeId: 'r', node: root, parent: null })

    // 3 s start-up + 0.125 × 32 s of audio.
    expect(onProgress).toHaveBeenLastCalledWith('r', 0, 7)
    vi.advanceTimersByTime(2000)
    expect(onProgress).toHaveBeenLastCalledWith('r', 2, 7)
    render.resolve(wav(32))
    await vi.waitFor(() => expect(deps.onDone).toHaveBeenCalled())
    const ticks = onProgress.mock.calls.length
    vi.advanceTimersByTime(3000)
    expect(onProgress).toHaveBeenCalledTimes(ticks)
  })
})

describe('wavDurationSeconds', () => {
  it("reads the shell's 44.1 kHz clip the same as clipSeconds", () => {
    const clip = shellClip(7.5)
    expect(wavDurationSeconds(clip)).toBe(clipSeconds(clip))
  })

  it('refuses bytes that are not a WAV', () => {
    expect(wavDurationSeconds(new ArrayBuffer(64))).toBeNull()
  })
})
