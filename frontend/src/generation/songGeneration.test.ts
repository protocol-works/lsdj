import { describe, expect, it } from 'vitest'

import {
  CFG_MAX,
  DEFAULT_SA3_STEERING,
  buildSongGeneration,
  clipSeconds,
  hasNegativeConcept,
  mintSa3Seed,
  parseFixedSeed,
  parseGenerationRecipe,
  sourceDraftOf,
  sourceWindow,
  toggleNegativeConcept,
  withSource,
  type SourceDraft,
} from './songGeneration'

describe('song generation requests', () => {
  it('captures an explicit seed for a reproducible Basic SA3 take', () => {
    expect(
      buildSongGeneration('basic', 'track', 'warm dub', 120, [], DEFAULT_SA3_STEERING, 42),
    ).toEqual({
      request: { prompt: 'warm dub', seconds: 120, kind: 'track', seed: 42 },
      recipe: {
        version: 1,
        prompt: 'warm dub',
        engine: 'track',
        seconds: 120,
        loras: [],
        sa3: { negativePrompt: '', seed: 42 },
      },
    })
  })

  it('sends and records complete Advanced SA3 steering', () => {
    expect(
      buildSongGeneration(
        'advanced',
        'track',
        'warm dub',
        120,
        [{ name: 'medium/dub', strength: 1.25 }],
        {
          negativePrompt: 'vocals, cymbals',
          guidance: 'on',
          cfg: 3.4,
          apg: 0.7,
          seed: { mode: 'random' },
        },
        42,
      ),
    ).toEqual({
      request: {
        prompt: 'warm dub',
        seconds: 120,
        kind: 'track',
        loras: [{ name: 'medium/dub', strength: 1.25 }],
        negative_prompt: 'vocals, cymbals',
        cfg: 3.4,
        apg: 0.7,
        seed: 42,
      },
      recipe: {
        version: 1,
        prompt: 'warm dub',
        engine: 'track',
        seconds: 120,
        loras: [{ name: 'medium/dub', strength: 1.25 }],
        sa3: {
          negativePrompt: 'vocals, cymbals',
          cfg: 3.4,
          apg: 0.7,
          seed: 42,
        },
      },
    })
  })

  it('sends only an explicit seed when guidance is off', () => {
    const result = buildSongGeneration(
      'advanced',
      'track',
      'ambient',
      60,
      [],
      DEFAULT_SA3_STEERING,
      7,
    )
    expect(result.request).toEqual({
      prompt: 'ambient',
      seconds: 60,
      kind: 'track',
      seed: 7,
    })
    expect(result.recipe.sa3).toEqual({ negativePrompt: '', seed: 7 })
  })

  it('pauses a remembered Avoid draft when guidance is turned off', () => {
    const result = buildSongGeneration(
      'advanced',
      'track',
      'ambient',
      60,
      [],
      {
        ...DEFAULT_SA3_STEERING,
        negativePrompt: 'vocals, trumpets',
        guidance: 'off',
      },
      8,
    )
    expect(result.request).toEqual({
      prompt: 'ambient',
      seconds: 60,
      kind: 'track',
      seed: 8,
    })
    expect(result.recipe.sa3).toEqual({ negativePrompt: '', seed: 8 })
  })

  it('rejects guidance outside the curated musical range', () => {
    expect(CFG_MAX).toBe(4)
    expect(() =>
      buildSongGeneration(
        'advanced',
        'track',
        'ambient',
        60,
        [],
        { ...DEFAULT_SA3_STEERING, guidance: 'on', cfg: 4.1 },
        7,
      ),
    ).toThrow('invalid SA3 guidance')
  })

  it('never sends SA3 or LoRA fields to Magenta', () => {
    const result = buildSongGeneration(
      'advanced',
      'magenta',
      'piano',
      60,
      [{ name: 'medium/piano', strength: 1 }],
      { ...DEFAULT_SA3_STEERING, negativePrompt: 'drums', guidance: 'on' },
      9,
    )
    expect(result.request).toEqual({ prompt: 'piano', seconds: 60 })
    expect(result.recipe).toEqual({
      version: 1,
      prompt: 'piano',
      engine: 'magenta',
      seconds: 60,
      loras: [],
    })
  })
})

describe('SA3 steering helpers', () => {
  it('validates the full signed-positive seed range without coercion', () => {
    expect(parseFixedSeed('0')).toBe(0)
    expect(parseFixedSeed('2147483647')).toBe(2_147_483_647)
    expect(parseFixedSeed('-1')).toBeNull()
    expect(parseFixedSeed('3.5')).toBeNull()
    expect(parseFixedSeed('2147483648')).toBeNull()
  })

  it('mints a non-negative 31-bit seed', () => {
    expect(mintSa3Seed((target) => {
      target[0] = 0xffff_ffff
      return target
    })).toBe(2_147_483_647)
  })

  it('toggles canonical negative concepts without adding negation words', () => {
    expect(toggleNegativeConcept('vocals', 'drums')).toBe('vocals, drums')
    expect(hasNegativeConcept('vocals, drums', 'drums')).toBe(true)
    expect(toggleNegativeConcept('vocals, drums', 'drums')).toBe('vocals')
  })
})

describe('generation recipes', () => {
  it('parses a supported recipe and rejects unsafe shapes', () => {
    const recipe = {
      version: 1,
      prompt: 'dub',
      engine: 'track',
      seconds: 120,
      loras: [{ name: 'medium/dub', strength: 1 }],
      sa3: { negativePrompt: 'vocals', cfg: 3, apg: 1, seed: 10 },
    }
    expect(parseGenerationRecipe(recipe)).toEqual({ status: 'supported', recipe })
    expect(
      parseGenerationRecipe({
        ...recipe,
        sa3: { negativePrompt: '', cfg: null, apg: null, seed: 11 },
      }),
    ).toEqual({
      status: 'supported',
      recipe: {
        ...recipe,
        sa3: { negativePrompt: '', seed: 11 },
      },
    })
    expect(parseGenerationRecipe({ ...recipe, version: 2 })).toEqual({
      status: 'unsupported',
    })
    expect(parseGenerationRecipe({ ...recipe, sa3: { ...recipe.sa3, seed: -1 } })).toEqual({
      status: 'invalid',
    })
    expect(
      parseGenerationRecipe({
        ...recipe,
        sa3: { negativePrompt: 'drums', seed: 4 },
      }),
    ).toEqual({ status: 'invalid' })
  })
})

describe('audio-to-audio sources', () => {
  const basic = buildSongGeneration('basic', 'track', 'dub', 240, [], DEFAULT_SA3_STEERING, 9)
  const draft: SourceDraft = {
    file: 'dub.wav',
    title: 'Dub',
    mode: 'continue',
    start: '',
    end: '',
    noiseLevel: 0.6,
    replaceStart: '',
    replaceEnd: '',
  }

  it('reads a blank window as the whole song and refuses an end before its start', () => {
    expect(sourceWindow(draft)).toEqual({ start: 0 })
    expect(sourceWindow({ ...draft, start: '10', end: '90' })).toEqual({ start: 10, end: 90 })
    expect(sourceWindow({ ...draft, start: '90', end: '10' })).toBe('windowInvalid')
    expect(sourceWindow({ ...draft, start: '-1' })).toBe('windowInvalid')
  })

  it('measures a clip from its 44.1 kHz 16-bit stereo bytes', () => {
    expect(clipSeconds(new ArrayBuffer(44 + 44_100 * 4 * 90))).toBe(90)
  })

  it('continues a window to the chosen length, regenerating only the new part', () => {
    const sourced = withSource(basic, draft, 10, 90)
    expect(sourced).toMatchObject({
      request: { seconds: 240, inpaint_range: [90, 240], seed: 9 },
      recipe: {
        seconds: 240,
        source: { file: 'dub.wav', mode: 'continue', start: 10, end: 100, inpaintRange: [90, 240] },
      },
    })
    expect(withSource(basic, draft, 0, 240)).toBe('continueTooShort')
  })

  it('replaces typed seconds within the window and takes the window length', () => {
    const replace = { ...draft, mode: 'replace' as const, replaceStart: '120', replaceEnd: '150' }
    expect(withSource(basic, replace, 0, 213)).toMatchObject({
      request: { seconds: 213, inpaint_range: [120, 150] },
      recipe: { seconds: 213, source: { mode: 'replace', inpaintRange: [120, 150] } },
    })
    expect(withSource(basic, { ...replace, replaceEnd: '300' }, 0, 213)).toBe('replaceInvalid')
    expect(withSource(basic, { ...replace, replaceStart: '' }, 0, 213)).toBe('replaceInvalid')
  })

  it('remixes the whole window at the noise level', () => {
    const sourced = withSource(basic, { ...draft, mode: 'remix' }, 0, 30)
    expect(sourced).toMatchObject({
      request: { seconds: 30, init_noise_level: 0.6 },
      recipe: { source: { mode: 'remix', initNoiseLevel: 0.6 } },
    })
    expect(sourced).not.toHaveProperty('request.inpaint_range')
  })

  it('round-trips a source through the saved recipe and back into the form', () => {
    const sourced = withSource(basic, draft, 10, 90)
    if (typeof sourced === 'string') throw new Error(sourced)
    const parsed = parseGenerationRecipe(JSON.parse(JSON.stringify(sourced.recipe)))
    expect(parsed).toEqual({ status: 'supported', recipe: sourced.recipe })
    if (parsed.status !== 'supported' || !parsed.recipe.source) throw new Error('no source')
    expect(sourceDraftOf(parsed.recipe.source, 'Dub')).toMatchObject({
      file: 'dub.wav',
      mode: 'continue',
      start: '10',
      end: '100',
    })
  })

  it('rejects a recipe whose source is malformed', () => {
    const recipe = { ...basic.recipe, source: { file: 'dub.wav', mode: 'stretch', start: 0, end: 1 } }
    expect(parseGenerationRecipe(recipe)).toEqual({ status: 'invalid' })
  })
})
