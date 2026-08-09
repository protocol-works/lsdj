import { fetchGenerationApi } from '../audio/nativeEngine'
import type {
  MagentaGenerationRequest,
  TrackGenerationRequest,
} from './songGeneration'

export type GenerationJobStatus = {
  jobId: string
  state: string
  progress: {
    stage: string
    current: number
    total: number
    message: string
  } | null
  detail: string | null
}

export type Sa3GenerationTask = {
  jobId: string
  result: Promise<ArrayBuffer>
  cancel: () => Promise<void>
  wasCancelled: () => boolean
}

export type Sa3GenerationRequest = Omit<TrackGenerationRequest, 'kind'> & {
  kind: 'sfx' | 'music' | 'track'
}

function mintJobId(): string {
  const cryptoApi = globalThis.crypto
  if (!cryptoApi) throw new Error('secure generation job IDs are unavailable')
  if (typeof cryptoApi.randomUUID === 'function') {
    return cryptoApi.randomUUID()
  }
  const bytes = new Uint8Array(16)
  cryptoApi.getRandomValues(bytes)
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('')
}

async function backendDetail(response: Response): Promise<string | null> {
  return response
    .json()
    .then((payload: { detail?: string }) => payload.detail ?? null)
    .catch(() => null)
}

async function postJson(
  path: string,
  body: unknown,
  options: { signal?: AbortSignal; jobId?: string } = {},
): Promise<ArrayBuffer> {
  const headers: Record<string, string> = { 'content-type': 'application/json' }
  if (options.jobId) headers['x-lsdj-job-id'] = options.jobId
  const response = await fetchGenerationApi(path, {
    method: 'POST',
    headers,
    body: JSON.stringify(body),
    signal: options.signal,
  })
  if (!response.ok) {
    const detail = await backendDetail(response)
    throw new Error(detail || `generation failed (${response.status})`)
  }
  return response.arrayBuffer()
}

export function startSa3Generate(
  request: Sa3GenerationRequest,
  onStatus?: (status: GenerationJobStatus) => void,
): Sa3GenerationTask {
  const jobId = mintJobId()
  const controller = new AbortController()
  let stopped = false
  let cancelled = false
  let pollInFlight = false

  const poll = async () => {
    if (stopped || pollInFlight || !onStatus) return
    pollInFlight = true
    try {
      const response = await fetchGenerationApi(`/api/jobs/${encodeURIComponent(jobId)}`)
      if (response.ok) onStatus((await response.json()) as GenerationJobStatus)
    } catch {
      // The POST owns user-visible errors. Poll failures are transient and must
      // never become an unhandled rejection or create a second polling loop.
    } finally {
      pollInFlight = false
    }
  }
  const pollTimer = onStatus ? globalThis.setInterval(() => void poll(), 400) : null
  if (onStatus) void poll()

  const result = postJson('/api/generate', request, {
    signal: controller.signal,
    jobId,
  }).finally(() => {
    stopped = true
    if (pollTimer !== null) globalThis.clearInterval(pollTimer)
  })

  const cancel = async () => {
    if (cancelled || stopped) return
    cancelled = true
    // Registration and the user's click can race. Briefly retry 404 before
    // aborting the response stream; the server's job event remains authoritative.
    for (let attempt = 0; attempt < 8 && !stopped; attempt += 1) {
      const response = await fetchGenerationApi(
        `/api/jobs/${encodeURIComponent(jobId)}/cancel`,
        { method: 'POST' },
      ).catch(() => null)
      if (response && response.status !== 404) break
      await new Promise((resolve) => globalThis.setTimeout(resolve, 25))
    }
    controller.abort()
  }

  return { jobId, result, cancel, wasCancelled: () => cancelled }
}

/** SA3-only seam: its type can carry text steering and LoRAs. */
export function postSa3Generate(request: Sa3GenerationRequest): Promise<ArrayBuffer> {
  return startSa3Generate(request).result
}

/** Magenta's separate seam cannot accept SA3-only options by construction. */
export function postMagentaRender(request: MagentaGenerationRequest): Promise<ArrayBuffer> {
  return postJson('/api/render', request)
}
