import { getApiBaseUrl } from '../audio/nativeEngine'
import type {
  MagentaGenerationRequest,
  TrackGenerationRequest,
} from './songGeneration'

async function postJson(path: string, body: unknown): Promise<ArrayBuffer> {
  const apiBase = await getApiBaseUrl()
  const response = await fetch(`${apiBase}${path}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  })
  if (!response.ok) {
    const detail = await response
      .json()
      .then((payload: { detail?: string }) => payload.detail)
      .catch(() => null)
    throw new Error(detail || `generation failed (${response.status})`)
  }
  return response.arrayBuffer()
}

/** SA3-only seam: its type can carry text steering and LoRAs. */
export function postSa3Generate(request: TrackGenerationRequest): Promise<ArrayBuffer> {
  return postJson('/api/generate', request)
}

/** Magenta's separate seam cannot accept SA3-only options by construction. */
export function postMagentaRender(request: MagentaGenerationRequest): Promise<ArrayBuffer> {
  return postJson('/api/render', request)
}
