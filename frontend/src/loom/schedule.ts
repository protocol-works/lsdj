/** The shuttle's scheduler (plan §7): a pure function from the loom and one
 * deck's runtime to what the engine should hold for that deck. `useLoom`
 * applies the plan idempotently on every 4 Hz poll. Every playing deck with a
 * node has a loop in its plan — the hold is the floor, so the deck never runs
 * off the end of its clip while a tie renders, fails or is still to come. */

import type { DeckId } from '../audio/types'
import { kids, nodeById, type LoomNode, type LoomState } from './loomState'
import {
  barsToSeam,
  holdRegion,
  lateBy,
  loopRegion,
  nextBarSeconds,
  nodeEndSeconds,
  nodeStartSeconds,
  renderEta,
  TAIL_BARS,
} from './timing'

export type Region = { start: number; end: number }

export type DeckPlan =
  | { kind: 'idle' }
  // An untied seam: loop the last bars.
  | { kind: 'hold'; loop: Region }
  // The DJ's loop: the loom's 4/8/16, or a loop the plan did not place.
  | { kind: 'loop'; loop: Region }
  // Hand over at the seam; the hold stays armed underneath, and the engine
  // swaps before it folds, because `switchAt` is the hold's end.
  | { kind: 'queue'; nodeId: string; file: string; startSeconds: number; switchAt: number; loop: Region }

/** What a deck plays next at its seam: the first tie, else (autopilot on) the
 * first child — `null` when the seam is untied. Not necessarily ready. */
export function nextNode(state: LoomState, deck: DeckId, current: LoomNode): LoomNode | null {
  const runtime = state.decks[deck]
  const tied = nodeById(state.loom.nodes, runtime.queue[0] ?? null)
  if (tied || !runtime.auto) return tied
  return kids(state.loom.nodes, current.id).find((k) => k.status === 'ready') ?? null
}

/** What waits at a deck's seam, for the shuttle tag and the deck panel: the
 * bars left, the next node, whether autopilot picked it, how late its render
 * will be, and whether the deck will hold. `progress` is the render queue's
 * elapsed/eta by node id. */
export function deckSeam(
  state: LoomState,
  deck: DeckId,
  node: LoomNode,
  position: number,
  progress: Record<string, { elapsed: number; eta: number }>,
) {
  const next = nextNode(state, deck, node)
  let late = 0
  if (next?.status === 'rendering') {
    const eta =
      progress[next.id]?.eta ??
      renderEta({ bars: next.bars, tailBars: next.parentId ? TAIL_BARS : 0, bpm: next.bpm, loras: next.loras })
    late = lateBy(eta - (progress[next.id]?.elapsed ?? 0), nodeEndSeconds(node) - position)
  }
  const hold = !next || next.status === 'failed' || next.status === 'missing' || late > 0
  const auto = next !== null && state.decks[deck].queue[0] !== next.id
  return { bars: barsToSeam(node, position), next, auto, late, hold }
}

/** `userLoop`: a loop region the engine holds that the plan did not place (the
 * FLX4's loop in/out); it wins until it is gone. */
export function planDeck(state: LoomState, deck: DeckId, userLoop: Region | null = null): DeckPlan {
  const runtime = state.decks[deck]
  const node = nodeById(state.loom.nodes, runtime.nodeId)
  if (!node) return { kind: 'idle' }
  if (userLoop) return { kind: 'loop', loop: userLoop }
  if (runtime.loopBars) {
    return { kind: 'loop', loop: loopRegion(node, runtime.loopStartBar ?? node.in, runtime.loopBars) }
  }
  const hold = holdRegion(node)
  const next = nextNode(state, deck, node)
  // A tie still rendering (or failed, or missing) holds: it goes once ready.
  if (next?.status !== 'ready' || !next.file) return { kind: 'hold', loop: hold }
  return {
    kind: 'queue',
    nodeId: next.id,
    file: next.file,
    startSeconds: nodeStartSeconds(next),
    switchAt: nodeEndSeconds(node),
    loop: hold,
  }
}

/** How far ahead a cut must land: the polled position is up to one 250 ms
 * poll stale, and the shell decodes and analyses the clip before the engine
 * has it — a switch point already passed would swap at once, off the grid.
 * ponytail: a fixed guess; measure the queue round-trip (P2) and tune. */
export const CUT_LEAD_SECONDS = 2

/** A cut: hand over to `next` on the first bar line `CUT_LEAD_SECONDS` out,
 * never past the seam (a hold folds there, and a switch beyond it would never
 * be reached). */
export const cutPlan = (current: LoomNode, position: number, next: LoomNode) => ({
  startSeconds: nodeStartSeconds(next),
  switchAt: Math.min(nextBarSeconds(current, position + CUT_LEAD_SECONDS), nodeEndSeconds(current)),
})
