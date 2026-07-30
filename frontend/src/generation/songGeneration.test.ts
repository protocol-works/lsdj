import { describe, expect, it } from 'vitest'

import {
  CFG_MAX,
  DEFAULT_SA3_STEERING,
  buildSongGeneration,
  hasNegativeConcept,
  mintSa3Seed,
  parseFixedSeed,
  parseGenerationRecipe,
  toggleNegativeConcept,
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
