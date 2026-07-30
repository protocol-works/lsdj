import { useCallback, useRef, useState } from 'react'

import {
  type LoraAdapter,
  type LoraBase,
} from '../audio/nativeEngine'
import { useLoraLibrary } from './loraContext'

/** Which DiT family each SA3 generation kind rides (mirrors the backend's
 * `loras.KIND_BASES`): the pad kinds share the small DiTs, tracks run medium. */
export const KIND_BASES: Record<'sfx' | 'music' | 'track', LoraBase> = {
  sfx: 'small',
  music: 'small',
  track: 'medium',
}

/** One slot of a generation's LoRA stack (a `/api/generate` `loras` entry,
 * issue #66): an adapter in the mix at a merge strength. */
export type LoraChoice = { name: string; strength: number }

/** Adapters per generation — mirrors the backend's `loras.MAX_LORA_STACK`. */
export const MAX_LORA_STACK = 4

/** The installed SA3 LoRA adapters from the app-wide lifecycle provider. */
export function useLoras(): LoraAdapter[] {
  return useLoraLibrary().loras
}

/** The adapters that can ride a generation kind (base-matched; the backend
 * refuses a mismatch, so the rack never offers one). */
export function adaptersForKind(
  loras: LoraAdapter[],
  kind: 'sfx' | 'music' | 'track',
): LoraAdapter[] {
  return loras.filter((adapter) => adapter.base === KIND_BASES[kind])
}

/** One form's LoRA stack, driving a contextual LoRA control: apply/remove an
 * adapter, trim its strength, or bypass it. The last non-zero strength is
 * remembered per adapter so both remove/re-apply and bypass/enable restore the
 * user's session value. */
export function useLoraStack(): {
  stack: LoraChoice[]
  toggle: (name: string) => void
  setStrength: (name: string, strength: number) => void
  toggleBypass: (name: string) => void
} {
  const [stack, setStack] = useState<LoraChoice[]>([])
  const strengths = useRef(new Map<string, number>())
  const activeStrengths = useRef(new Map<string, number>())
  const toggle = useCallback((name: string) => {
    setStack((current) => {
      if (current.some((entry) => entry.name === name)) {
        return current.filter((entry) => entry.name !== name)
      }
      if (current.length >= MAX_LORA_STACK) return current
      const strength = strengths.current.get(name) ?? 1
      if (strength > 0) activeStrengths.current.set(name, strength)
      return [...current, { name, strength }]
    })
  }, [])
  const setStrength = useCallback((name: string, strength: number) => {
    strengths.current.set(name, strength)
    if (strength > 0) activeStrengths.current.set(name, strength)
    setStack((current) =>
      current.map((entry) => (entry.name === name ? { ...entry, strength } : entry)),
    )
  }, [])
  const toggleBypass = useCallback((name: string) => {
    setStack((current) =>
      current.map((entry) => {
        if (entry.name !== name) return entry
        const strength = entry.strength === 0 ? (activeStrengths.current.get(name) ?? 1) : 0
        strengths.current.set(name, strength)
        if (entry.strength > 0) activeStrengths.current.set(name, entry.strength)
        return { ...entry, strength }
      }),
    )
  }, [])
  return { stack, toggle, setStrength, toggleBypass }
}

/** The stack filtered to what still resolves for this kind at request time —
 * an adapter deleted mid-session, or orphaned by an engine switch to the
 * other base, silently drops from the request (the stale-choice rule,
 * applied per slot). */
export function stackForKind(
  stack: LoraChoice[],
  loras: LoraAdapter[],
  kind: 'sfx' | 'music' | 'track',
): LoraChoice[] {
  const matched = new Set(adaptersForKind(loras, kind).map((adapter) => adapter.name))
  return stack.filter((entry) => matched.has(entry.name))
}
