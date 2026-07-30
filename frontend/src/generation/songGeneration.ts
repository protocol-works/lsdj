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

export type SongGenerationRecipeV1 = {
  version: 1
  prompt: string
  engine: SongGenerationEngine
  seconds: number
  loras: LoraChoice[]
  sa3?: Sa3Recipe
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
      !isBoundedNumber(choice.strength, 0, 2)
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

  return {
    status: 'supported',
    recipe: {
      version: 1,
      prompt: value.prompt,
      engine: value.engine,
      seconds: value.seconds,
      loras,
      ...(sa3 ? { sa3 } : {}),
    },
  }
}
