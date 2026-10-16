import type { LoraChoice } from '../models/useLoras'

export type GenerationMode = 'basic' | 'advanced'
export type SongGenerationEngine = 'track' | 'magenta'
export type GuidanceMode = 'off' | 'on'
export type SeedDraft =
  | { mode: 'random' }
  | { mode: 'fixed'; value: string }

export type Sa3SteeringDraft = {
  negativePrompt: string
  guidance: GuidanceMode
  cfg: number
  apg: number
  seed: SeedDraft
}

export type Sa3Recipe = {
  negativePrompt: string
  seed: number
  cfg?: number
  apg?: number
}

export type SourceMode = 'remix' | 'replace' | 'continue'
export const SOURCE_MODES: SourceMode[] = ['remix', 'replace', 'continue']

/** Where an audio-to-audio take came from — the shell's `SourceRecipe`, which the
 * MCP `generate_track` records too. Output time 0 is `start` in the source. */
export type SourceRecipe = {
  file: string
  mode: SourceMode
  start: number
  end: number
  initNoiseLevel?: number
  inpaintRange?: [number, number]
}

export type SongGenerationRecipeV1 = {
  version: 1
  prompt: string
  engine: SongGenerationEngine
  seconds: number
  loras: LoraChoice[]
  sa3?: Sa3Recipe
  source?: SourceRecipe
}

export type TrackGenerationRequest = {
  prompt: string
  seconds: number
  kind: 'track'
  loras?: LoraChoice[]
  negative_prompt?: string
  cfg?: number
  apg?: number
  seed?: number
  init_noise_level?: number
  inpaint_range?: [number, number]
}

export type MagentaGenerationRequest = {
  prompt: string
  seconds: number
}

export type SongGenerationBuild = {
  request: TrackGenerationRequest | MagentaGenerationRequest
  recipe: SongGenerationRecipeV1
}

export type ParsedGenerationRecipe =
  | { status: 'supported'; recipe: SongGenerationRecipeV1 }
  | { status: 'unsupported' }
  | { status: 'invalid' }

export const CFG_MIN = 1.1
// The pinned Medium/SAME-L path hard-clips increasingly above 4.0. Keep the
// product range musical; the backend's broader validation remains authoritative
// for non-UI callers.
export const CFG_MAX = 4
export const CFG_DEFAULT = 3
export const APG_MIN = 0
export const APG_MAX = 1
export const APG_DEFAULT = 1
export const SA3_SEED_MAX = 2_147_483_647

export const DEFAULT_SA3_STEERING: Sa3SteeringDraft = {
  negativePrompt: '',
  guidance: 'off',
  cfg: CFG_DEFAULT,
  apg: APG_DEFAULT,
  seed: { mode: 'random' },
}

export function parseFixedSeed(value: string): number | null {
  if (!/^\d+$/.test(value)) return null
  const parsed = Number(value)
  return Number.isSafeInteger(parsed) && parsed <= SA3_SEED_MAX ? parsed : null
}

/** Mint a backend-safe signed 32-bit seed for an Advanced take. */
export function mintSa3Seed(
  randomValues: (value: Uint32Array<ArrayBuffer>) => Uint32Array<ArrayBuffer> = (value) =>
    crypto.getRandomValues(value),
): number {
  const value = new Uint32Array(new ArrayBuffer(Uint32Array.BYTES_PER_ELEMENT))
  randomValues(value)
  return value[0] & SA3_SEED_MAX
}

/** Toggle one canonical SA3 concept in the comma-separated negative prompt. */
export function toggleNegativeConcept(prompt: string, concept: string): string {
  const entries = prompt
    .split(',')
    .map((entry) => entry.trim())
    .filter(Boolean)
  const index = entries.findIndex(
    (entry) => entry.localeCompare(concept, undefined, { sensitivity: 'accent' }) === 0,
  )
  if (index >= 0) entries.splice(index, 1)
  else entries.push(concept)
  return entries.join(', ')
}

export function hasNegativeConcept(prompt: string, concept: string): boolean {
  return prompt
    .split(',')
    .some(
      (entry) =>
        entry.trim().localeCompare(concept, undefined, { sensitivity: 'accent' }) === 0,
    )
}

export function buildSongGeneration(
  mode: GenerationMode,
  engine: SongGenerationEngine,
  prompt: string,
  seconds: number,
  loras: LoraChoice[],
  steering: Sa3SteeringDraft,
  takeSeed?: number,
): SongGenerationBuild {
  const recipeBase = { version: 1 as const, prompt, engine, seconds }
  if (engine === 'magenta') {
    return {
      request: { prompt, seconds },
      recipe: { ...recipeBase, loras: [] },
    }
  }

  const loraFields = loras.length > 0 ? { loras } : {}
  const seed =
    mode === 'basic'
      ? takeSeed
      : steering.seed.mode === 'fixed'
        ? parseFixedSeed(steering.seed.value)
        : takeSeed
  if (seed == null || !Number.isInteger(seed) || seed < 0 || seed > SA3_SEED_MAX) {
    throw new Error('invalid SA3 seed')
  }
  if (mode === 'basic') {
    return {
      request: { prompt, seconds, kind: 'track', ...loraFields, seed },
      recipe: {
        ...recipeBase,
        loras,
        sa3: { negativePrompt: '', seed },
      },
    }
  }

  const guidanceEnabled = steering.guidance === 'on'
  // Keep the authored Avoid draft in the form when Guidance is paused, but do
  // not let that hidden draft silently force guidance back on. Recipes capture
  // the effective request, so an unguided take records an empty Avoid value.
  const negativePrompt = guidanceEnabled ? steering.negativePrompt.trim() : ''
  if (
    guidanceEnabled &&
    (!isBoundedNumber(steering.cfg, CFG_MIN, CFG_MAX) ||
      !isBoundedNumber(steering.apg, APG_MIN, APG_MAX))
  ) {
    throw new Error('invalid SA3 guidance')
  }
  const steeringFields = guidanceEnabled
    ? {
        ...(negativePrompt ? { negative_prompt: negativePrompt } : {}),
        cfg: steering.cfg,
        apg: steering.apg,
      }
    : {}
  const sa3: Sa3Recipe = {
    negativePrompt,
    seed,
    ...(guidanceEnabled ? { cfg: steering.cfg, apg: steering.apg } : {}),
  }
  return {
    request: {
      prompt,
      seconds,
      kind: 'track',
      ...loraFields,
      ...steeringFields,
      seed,
    },
    recipe: { ...recipeBase, loras, sa3 },
  }
}

/** The Generate tab's source form: seconds stay typed text until generate. */
export type SourceDraft = {
  file: string
  title: string
  mode: SourceMode
  /** Window cut from the source; blank start = 0, blank end = the song's end. */
  start: string
  end: string
  /** Remix: 0.01 keeps the source, 1 regenerates fully. */
  noiseLevel: number
  /** Replace: output seconds to regenerate. */
  replaceStart: string
  replaceEnd: string
}

export const SOURCE_NOISE_DEFAULT = 0.6

export type SourceError = 'windowInvalid' | 'replaceInvalid' | 'continueTooShort'

/** A typed seconds value: null when blank, NaN when not a non-negative number. */
function typedSeconds(text: string): number | null {
  if (!text.trim()) return null
  const value = Number(text)
  return Number.isFinite(value) && value >= 0 ? value : Number.NaN
}

/** The window to cut, or an error for typed numbers that make no window. */
export function sourceWindow(
  draft: SourceDraft,
): { start: number; end?: number } | SourceError {
  const start = typedSeconds(draft.start) ?? 0
  const end = typedSeconds(draft.end)
  if (Number.isNaN(start) || Number.isNaN(end) || (end !== null && end <= start)) {
    return 'windowInvalid'
  }
  return end === null ? { start } : { start, end }
}

/** The shell's source clip is 44.1 kHz 16-bit stereo behind a 44-byte header. */
export function clipSeconds(wav: ArrayBuffer): number {
  return (wav.byteLength - 44) / (44_100 * 4)
}

/** Fold a cut source into an SA3 build — the webview twin of the MCP
 * `source_plan`: continue extends the window to the chosen length, replace
 * regenerates typed output seconds, remix redraws all of it at the noise level.
 * Replace and remix take the window's length. */
export function withSource(
  build: SongGenerationBuild,
  draft: SourceDraft,
  start: number,
  clipLength: number,
): SongGenerationBuild | SourceError {
  if (!('kind' in build.request)) return 'windowInvalid'
  const base = { file: draft.file, mode: draft.mode, start, end: start + clipLength }
  let seconds = clipLength
  let fields: Pick<TrackGenerationRequest, 'init_noise_level' | 'inpaint_range'>
  let source: SourceRecipe
  if (draft.mode === 'continue') {
    seconds = build.request.seconds
    if (seconds <= clipLength) return 'continueTooShort'
    const range: [number, number] = [clipLength, seconds]
    fields = { inpaint_range: range }
    source = { ...base, inpaintRange: range }
  } else if (draft.mode === 'replace') {
    const from = typedSeconds(draft.replaceStart)
    const to = typedSeconds(draft.replaceEnd)
    if (from === null || to === null || !(from < to && to <= seconds)) return 'replaceInvalid'
    fields = { inpaint_range: [from, to] }
    source = { ...base, inpaintRange: [from, to] }
  } else {
    fields = { init_noise_level: draft.noiseLevel }
    source = { ...base, initNoiseLevel: draft.noiseLevel }
  }
  return {
    request: { ...build.request, seconds, ...fields },
    recipe: { ...build.recipe, seconds, source },
  }
}

/** A recalled source back into the form. */
export function sourceDraftOf(source: SourceRecipe, title: string): SourceDraft {
  const [replaceStart, replaceEnd] =
    source.mode === 'replace' && source.inpaintRange
      ? source.inpaintRange.map(String)
      : ['', '']
  return {
    file: source.file,
    title,
    mode: source.mode,
    start: String(source.start),
    end: String(source.end),
    noiseLevel: source.initNoiseLevel ?? SOURCE_NOISE_DEFAULT,
    replaceStart,
    replaceEnd,
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value != null && typeof value === 'object' && !Array.isArray(value)
}

function isBoundedNumber(value: unknown, min: number, max: number): value is number {
  return typeof value === 'number' && Number.isFinite(value) && value >= min && value <= max
}

/** Parse persisted recipe metadata without trusting the registry shape. */
export function parseGenerationRecipe(value: unknown): ParsedGenerationRecipe {
  if (!isRecord(value)) return { status: 'invalid' }
  if (value.version !== 1) {
    return typeof value.version === 'number'
      ? { status: 'unsupported' }
      : { status: 'invalid' }
  }
  if (
    typeof value.prompt !== 'string' ||
    (value.engine !== 'track' && value.engine !== 'magenta') ||
    typeof value.seconds !== 'number' ||
    !Number.isFinite(value.seconds) ||
    !Array.isArray(value.loras) ||
    value.loras.length > 4
  ) {
    return { status: 'invalid' }
  }
  const loras: LoraChoice[] = []
  const loraNames = new Set<string>()
  for (const choice of value.loras) {
    if (
      !isRecord(choice) ||
      typeof choice.name !== 'string' ||
      !choice.name ||
      loraNames.has(choice.name) ||
      // The backend's (and the MCP's) 0-4 range; the rack clamps to its own 2.
      !isBoundedNumber(choice.strength, 0, 4)
    ) {
      return { status: 'invalid' }
    }
    loraNames.add(choice.name)
    loras.push({ name: choice.name, strength: choice.strength })
  }
  if (value.engine === 'magenta' && loras.length > 0) return { status: 'invalid' }

  let sa3: Sa3Recipe | undefined
  if (value.sa3 !== undefined) {
    if (
      value.engine !== 'track' ||
      !isRecord(value.sa3) ||
      typeof value.sa3.negativePrompt !== 'string' ||
      !Number.isInteger(value.sa3.seed) ||
      !isBoundedNumber(value.sa3.seed, 0, SA3_SEED_MAX)
    ) {
      return { status: 'invalid' }
    }
    // Rust used to serialize absent Option values as null. Treat those existing
    // Basic recipes exactly like omitted guidance fields, then normalize them
    // out of the parsed recipe.
    const hasCfg = value.sa3.cfg != null
    const hasApg = value.sa3.apg != null
    if (
      hasCfg !== hasApg ||
      (hasCfg && !isBoundedNumber(value.sa3.cfg, CFG_MIN, CFG_MAX)) ||
      (hasApg && !isBoundedNumber(value.sa3.apg, APG_MIN, APG_MAX))
    ) {
      return { status: 'invalid' }
    }
    if (value.sa3.negativePrompt.trim() && !hasCfg) return { status: 'invalid' }
    sa3 = {
      negativePrompt: value.sa3.negativePrompt,
      seed: value.sa3.seed as number,
      ...(hasCfg ? { cfg: value.sa3.cfg as number, apg: value.sa3.apg as number } : {}),
    }
  }

  let source: SourceRecipe | undefined
  if (value.source !== undefined) {
    const raw = value.source
    if (
      value.engine !== 'track' ||
      !isRecord(raw) ||
      typeof raw.file !== 'string' ||
      !raw.file ||
      !SOURCE_MODES.includes(raw.mode as SourceMode) ||
      !isBoundedNumber(raw.start, 0, Infinity) ||
      !isBoundedNumber(raw.end, raw.start, Infinity) ||
      (raw.initNoiseLevel !== undefined && !isBoundedNumber(raw.initNoiseLevel, 0.01, 5)) ||
      (raw.inpaintRange !== undefined &&
        !(
          Array.isArray(raw.inpaintRange) &&
          raw.inpaintRange.length === 2 &&
          raw.inpaintRange.every((edge) => isBoundedNumber(edge, 0, Infinity))
        ))
    ) {
      return { status: 'invalid' }
    }
    source = {
      file: raw.file,
      mode: raw.mode as SourceMode,
      start: raw.start,
      end: raw.end,
      ...(raw.initNoiseLevel !== undefined ? { initNoiseLevel: raw.initNoiseLevel as number } : {}),
      ...(raw.inpaintRange !== undefined
        ? { inpaintRange: raw.inpaintRange as [number, number] }
        : {}),
    }
  }

  return {
    status: 'supported',
    recipe: {
      version: 1,
      prompt: value.prompt,
      engine: value.engine,
      seconds: value.seconds,
      loras,
      ...(sa3 ? { sa3 } : {}),
      ...(source ? { source } : {}),
    },
  }
}
