/** Render ahead (plan decision 7): behind the node a playing deck is on, grow
 * one auto continuation so a hold is a choice, not an accident. LoRA threads
 * are skipped — a ≈2 min render is no safety net — and the caller's deck tag
 * says so (`node.loras.length > 0`). */

import type { DeckId } from '../audio/types'
import { kids, nodeById, type LoomNode, type LoomState } from './loomState'

/** The node to weave an auto child behind, or `null`. `inFlight`: node ids the
 * caller has a render pending for (the queue's `pending()`, plus a node whose
 * auto child it has just asked for and the state does not show yet). The
 * caller weaves `{ prompt: node.prompt, bars: DEFAULT_BARS, takes: 1, auto: true }`. */
export function renderAheadCandidate(
  state: LoomState,
  deck: DeckId,
  inFlight: Set<string>,
): LoomNode | null {
  const node = nodeById(state.loom.nodes, state.decks[deck].nodeId)
  if (!node || node.status !== 'ready' || node.loras.length > 0 || inFlight.has(node.id)) return null
  return kids(state.loom.nodes, node.id).length === 0 ? node : null
}
