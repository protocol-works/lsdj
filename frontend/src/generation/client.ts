import { getApiBaseUrl } from '../audio/nativeEngine'
import type {
  MagentaGenerationRequest,
  TrackGenerationRequest,
} from './songGeneration'

async function post(path: string, init: RequestInit): Promise<ArrayBuffer> {
  const apiBase = await getApiBaseUrl()
  const response = await fetch(`${apiBase}${path}`, { method: 'POST', ...init })
  if (!response.ok) {
    const detail = await response
      .json()
      .then((payload: { detail?: string }) => payload.detail)
      .catch(() => null)
    throw new Error(detail || `generation failed (${response.status})`)
  }
  return response.arrayBuffer()
}

function postJson(path: string, body: unknown): Promise<ArrayBuffer> {
  return post(path, {
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  })
}

/** SA3-only seam: its type can carry text steering and LoRAs. */
export function postSa3Generate(request: TrackGenerationRequest): Promise<ArrayBuffer> {
  return postJson('/api/generate', request)
}

/** SA3 audio-to-audio: the same request plus the init WAV, as the server's
 * two-part multipart form (`request` JSON + `init_audio` file). */
export function postSa3GenerateWithSource(
  request: TrackGenerationRequest,
  initAudio: ArrayBuffer,
): Promise<ArrayBuffer> {
  const form = new FormData()
  form.append('request', JSON.stringify(request))
  form.append('init_audio', new Blob([initAudio], { type: 'audio/wav' }), 'init.wav')
  return post('/api/generate', { body: form })
}

/** Magenta's separate seam cannot accept SA3-only options by construction. */
export function postMagentaRender(request: MagentaGenerationRequest): Promise<ArrayBuffer> {
  return postJson('/api/render', request)
}
