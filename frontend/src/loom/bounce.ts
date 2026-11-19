/** Bounce (plan decision 14): a loom path stitched into one songs-library take
 * by the shell (`stitch_songs`). Only the parts' seconds cross IPC, never PCM
 * (ADR-0030); the explorer re-lists on the shell's `library://changed`. */
import type { TFunction } from 'i18next'

import { invoke } from '../audio/nativeEngine'
import type { DeckId } from '../audio/types'
import type { SongEntry } from '../generation/library'
import { nodeById, pathTo, type LoomState } from './loomState'
import { nodeEndSeconds, nodeStartSeconds } from './timing'

/** One slice of a bounce (Rust `commands::StitchPart`). */
export type BouncePart = { name: string; startSeconds: number; endSeconds: number }
export type Bounce = { parts: BouncePart[]; title: string; prompt: string }

/** What a deck played: its history, then the node it is on. */
export function deckPath(state: LoomState, deck: DeckId): string[] {
  const { history, nodeId } = state.decks[deck]
  return nodeId ? [...history, nodeId] : history
}

/** Root → node. */
export const nodePath = (state: LoomState, id: string) => pathTo(state.loom.nodes, id).map((node) => node.id)

/** Each rendered node's trimmed body, in order; `endAt` (a deck's playhead)
 * cuts the last id short. Ids gone from the loom, nodes without a file and
 * bodies left empty drop out; null when nothing is left. */
export function pathParts(state: LoomState, ids: string[], t: TFunction, endAt?: number): Bounce | null {
  const parts: BouncePart[] = []
  const prompts: string[] = []
  ids.forEach((id, index) => {
    const node = nodeById(state.loom.nodes, id)
    if (!node?.file || node.status !== 'ready') return
    const startSeconds = nodeStartSeconds(node)
    const end = nodeEndSeconds(node)
    const endSeconds = index === ids.length - 1 && endAt !== undefined ? Math.min(end, endAt) : end
    if (endSeconds <= startSeconds) return
    parts.push({ name: node.file, startSeconds, endSeconds })
    prompts.push(node.prompt)
  })
  if (parts.length === 0) return null
  return {
    parts,
    title: t('loom.bounce.title', { loom: state.loom.name || t('loom.bar.defaultName'), node: prompts.at(-1) }),
    prompt: prompts.join(' → '),
  }
}

/** Stitch a bounce into a new songs-library row. */
export const stitchSongs = ({ parts, title, prompt }: Bounce) =>
  invoke<SongEntry>('stitch_songs', { parts, title, prompt })
