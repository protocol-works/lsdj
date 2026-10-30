/** The loom's render queue (plan §6): one SA3 render at a time, FIFO, each a
 * song in the library. A root renders from its prompt; a child continues its
 * parent's last `TAIL_BARS` bars up to the parent's out-point (SA3 continue
 * mode), so the clip starts with that kept tail. Pure logic over injected
 * deps; a failure never throws out of the queue, it reports through
 * `onError`. */

import { GENERATE_PROMPT_MAX_LENGTH } from '../deck/useDeck'
import type { postSa3Generate, postSa3GenerateWithSource } from '../generation/client'
import type { saveGeneratedSong } from '../generation/library'
import {
  APG_DEFAULT,
  CFG_DEFAULT,
  buildSongGeneration,
  clipSeconds,
  withSource,
  type Sa3SteeringDraft,
  type TrackGenerationRequest,
} from '../generation/songGeneration'
import { randomSongTitle } from '../media/songTitle'
import type { LoomNode } from './loomState'
import { barSeconds, nodeEndSeconds, renderEta, TAIL_BARS } from './timing'

/** `take` (1-based) names the song `… · take k` when a weave asked for several. */
export type RenderJob = { nodeId: string; node: LoomNode; parent: LoomNode | null; take?: number }

export type RenderQueueDeps = {
  /** `invoke('song_source_clip', { name, start, end })`: 44.1 kHz 16-bit WAV. */
  sourceClip: (file: string, start: number, end: number) => Promise<ArrayBuffer>
  generate: typeof postSa3Generate
  generateWithSource: typeof postSa3GenerateWithSource
  save: typeof saveGeneratedSong
  onDone: (nodeId: string, file: string, durationSeconds: number) => void
  onError: (nodeId: string, message: string) => void
  /** Once when a job starts, then once a second, for the ghost bar. */
  onProgress?: (nodeId: string, elapsedSeconds: number, etaSeconds: number) => void
}

export type RenderQueue = {
  enqueue: (job: RenderJob) => void
  /** Drop a queued job; an in-flight one still saves (it is a song) but reports nothing. */
  cancel: (nodeId: string) => void
  /** The in-flight job first (unless cancelled), then the queued ones. */
  pending: () => string[]
}

const TITLE_CHARS = 24

/** The first `TITLE_CHARS` of the prompt, cut at a word. */
function songTitle(prompt: string, take?: number): string {
  const text = prompt.trim().replace(/\s+/g, ' ')
  const cut =
    text.length <= TITLE_CHARS
      ? text
      : text.slice(0, TITLE_CHARS + 1).replace(/\s+\S*$/, '') || text.slice(0, TITLE_CHARS)
  const title = cut.replace(/[\s,;:.]+$/, '') || randomSongTitle()
  return take ? `${title} · take ${take}` : title
}

/** Seconds of audio in a WAV, read from its `fmt ` and `data` chunks (the SA3
 * output's layout is the CLI's, so nothing about it is assumed). */
export function wavDurationSeconds(wav: ArrayBuffer): number | null {
  const view = new DataView(wav)
  const tag = (at: number) => String.fromCharCode(...new Uint8Array(wav, at, 4))
  if (wav.byteLength < 12 || tag(0) !== 'RIFF' || tag(8) !== 'WAVE') return null
  let frameBytesPerSecond = 0
  for (let at = 12; at + 8 <= wav.byteLength; ) {
    const size = view.getUint32(at + 4, true)
    if (tag(at) === 'fmt ' && size >= 16) {
      frameBytesPerSecond = view.getUint32(at + 12, true) * view.getUint16(at + 20, true)
    } else if (tag(at) === 'data') {
      // A streamed writer may leave the size unset; the bytes present win.
      const bytes = Math.min(size, wav.byteLength - at - 8)
      return frameBytesPerSecond > 0 ? bytes / frameBytesPerSecond : null
    }
    at += 8 + size + (size % 2)
  }
  return null
}

async function render(job: RenderJob, deps: RenderQueueDeps) {
  const { node, parent } = job
  const bar = barSeconds(node.bpm)
  const body = node.bars * bar
  const prompt = `${node.prompt.trim().slice(0, GENERATE_PROMPT_MAX_LENGTH)}, ${Math.round(node.bpm)} BPM`
  const { cfg, apg, negativePrompt } = node.sa3
  // apg and Avoid need cfg ≠ 1, so guidance carries them only when switched on.
  const steering: Sa3SteeringDraft = {
    negativePrompt: negativePrompt ?? '',
    guidance: cfg !== undefined || apg !== undefined || negativePrompt?.trim() ? 'on' : 'off',
    cfg: cfg ?? CFG_DEFAULT,
    apg: apg ?? APG_DEFAULT,
    seed: { mode: 'fixed', value: String(node.seed) },
  }
  const build = (seconds: number) =>
    buildSongGeneration('advanced', 'track', prompt, seconds, node.loras, steering)

  let generation = build(body)
  let wav: ArrayBuffer
  if (parent === null) {
    wav = await deps.generate(generation.request as TrackGenerationRequest)
  } else {
    if (!parent.file) throw new Error('the parent has not rendered yet')
    const end = nodeEndSeconds(parent)
    const start = Math.max(0, end - TAIL_BARS * bar)
    const clip = await deps.sourceClip(parent.file, start, end)
    const tail = clipSeconds(clip)
    const sourced = withSource(
      build(tail + body),
      {
        file: parent.file,
        title: '',
        mode: 'continue',
        start: String(start),
        end: String(end),
        noiseLevel: 0,
        replaceStart: '',
        replaceEnd: '',
      },
      start,
      tail,
    )
    if (typeof sourced === 'string') throw new Error(sourced)
    generation = sourced
    wav = await deps.generateWithSource(sourced.request as TrackGenerationRequest, clip)
  }
  const durationSeconds = wavDurationSeconds(wav)
  if (durationSeconds === null) throw new Error('the render is not a readable WAV')
  const entry = await deps.save(
    { title: songTitle(node.prompt, job.take), prompt, model: 'track', recipe: generation.recipe },
    wav,
  )
  return { file: entry.file, durationSeconds }
}

export function createRenderQueue(deps: RenderQueueDeps): RenderQueue {
  const queue: RenderJob[] = []
  let running: { nodeId: string; dropped: boolean } | null = null

  async function next() {
    const job = queue.shift()
    if (!job) return
    const run = { nodeId: job.nodeId, dropped: false }
    running = run
    const eta = renderEta({
      bars: job.node.bars,
      tailBars: job.parent ? TAIL_BARS : 0,
      bpm: job.node.bpm,
      loras: job.node.loras,
    })
    const started = Date.now()
    const tick = () => deps.onProgress?.(job.nodeId, (Date.now() - started) / 1000, eta)
    tick()
    const timer = setInterval(tick, 1000)
    let outcome: { file: string; durationSeconds: number } | { error: string }
    try {
      outcome = await render(job, deps)
    } catch (error) {
      outcome = { error: error instanceof Error ? error.message : String(error) }
    }
    clearInterval(timer)
    running = null
    try {
      if (run.dropped) return
      if ('error' in outcome) deps.onError(job.nodeId, outcome.error)
      else deps.onDone(job.nodeId, outcome.file, outcome.durationSeconds)
    } finally {
      // A throwing callback must not stall the jobs behind it.
      void next()
    }
  }

  return {
    enqueue(job) {
      queue.push(job)
      if (!running) void next()
    },
    cancel(nodeId) {
      const at = queue.findIndex((job) => job.nodeId === nodeId)
      if (at >= 0) queue.splice(at, 1)
      if (running?.nodeId === nodeId) running.dropped = true
    },
    pending: () => [
      ...(running && !running.dropped ? [running.nodeId] : []),
      ...queue.map((job) => job.nodeId),
    ],
  }
}
