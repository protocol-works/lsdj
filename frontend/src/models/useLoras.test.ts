import { act, renderHook } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import type { LoraAdapter } from '../audio/nativeEngine'
import { MAX_LORA_STACK, stackForKind, useLoraStack } from './useLoras'

describe('useLoraStack', () => {
  it('restores the last non-zero strength after bypass and remove/re-apply', () => {
    const { result } = renderHook(() => useLoraStack())

    act(() => result.current.toggle('medium/maqam'))
    act(() => result.current.setStrength('medium/maqam', 1.5))
    act(() => result.current.toggleBypass('medium/maqam'))
    expect(result.current.stack).toEqual([{ name: 'medium/maqam', strength: 0 }])

    act(() => result.current.toggleBypass('medium/maqam'))
    expect(result.current.stack).toEqual([{ name: 'medium/maqam', strength: 1.5 }])

    act(() => result.current.toggle('medium/maqam'))
    act(() => result.current.toggle('medium/maqam'))
    expect(result.current.stack).toEqual([{ name: 'medium/maqam', strength: 1.5 }])
  })

  it('enforces the shared stack limit', () => {
    const { result } = renderHook(() => useLoraStack())

    for (let index = 0; index <= MAX_LORA_STACK; index += 1) {
      act(() => result.current.toggle(`medium/adapter-${index}`))
    }

    expect(result.current.stack).toHaveLength(MAX_LORA_STACK)
    expect(result.current.stack.map(({ name }) => name)).not.toContain(
      `medium/adapter-${MAX_LORA_STACK}`,
    )
  })

  it('replaces a stack from a recipe while enforcing control bounds', () => {
    const { result } = renderHook(() => useLoraStack())
    const choices = Array.from({ length: MAX_LORA_STACK + 1 }, (_, index) => ({
      name: `medium/adapter-${index}`,
      strength: index === 0 ? 3 : 1,
    }))

    act(() => result.current.replace(choices))

    expect(result.current.stack).toHaveLength(MAX_LORA_STACK)
    expect(result.current.stack[0].strength).toBe(2)
  })
})

describe('stackForKind', () => {
  const adapters: LoraAdapter[] = [
    {
      name: 'medium/maqam',
      base: 'medium',
      slug: 'maqam',
      sizeBytes: 1,
      source: null,
      adapterType: 'lora',
      rank: 8,
    },
    {
      name: 'small/crackle',
      base: 'small',
      slug: 'crackle',
      sizeBytes: 1,
      source: null,
      adapterType: 'lora',
      rank: 8,
    },
  ]

  it('drops deleted and wrong-base choices at request time while retaining bypass', () => {
    const stack = [
      { name: 'medium/maqam', strength: 0 },
      { name: 'small/crackle', strength: 1 },
      { name: 'medium/deleted', strength: 1 },
    ]

    expect(stackForKind(stack, adapters, 'track')).toEqual([
      { name: 'medium/maqam', strength: 0 },
    ])
    expect(stackForKind(stack, adapters, 'sfx')).toEqual([
      { name: 'small/crackle', strength: 1 },
    ])
  })
})
