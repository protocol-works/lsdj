/** Pure loom state (plan §5): the loom graph — a forest of nodes, each a song
 * in the library — plus the two decks' runtime threading through it. Every
 * gesture, render result and shuttle move funnels through `loomReducer`, so
 * the view and the scheduler are functions of one state object. Deck runtime
 * is never persisted (`persistable` strips it). Selectors tolerate missing
 * ids: a deck's history may name a node discarded since. */

import type { DeckId } from '../audio/types'
import type { LoraChoice } from '../models/useLoras'
import { barSeconds, MIN_BARS, TAIL_BARS } from './timing'

export type LoomNode = {
  id: string
  parentId: string | null
  /** Effective prompt, stored unstamped (the render stamps `, {bpm} BPM`). */
  prompt: string
  /** Nominal body length in bars (excludes the kept tail). */
  bars: number
  /** The root's tempo, copied down. */
  bpm: number
  seed: number
  loras: LoraChoice[]
  sa3: { cfg?: number; apg?: number; negativePrompt?: string }
  /** Songs-library filename once rendered. */
  file: string | null
  /** The clip's measured length once rendered; clamps every derived end. */
  durationSeconds: number | null
  /** Kept parent tail at the clip start (0 on a root); bar 0 sits after it. */
  tailSeconds: number
  /** Trim, in nominal bars from bar 0 (0 ≤ in < out ≤ bars). */
  in: number
  out: number
  /** Siblings woven together as takes. */
  takeGroup: string | null
  status: 'rendering' | 'ready' | 'failed' | 'missing'
  error?: string
  /** A render-ahead child. */
  auto: boolean
  /** Camelot key from the registry, when known. */
  key?: string
  createdAt: number
}

export type Loom = {
  version: 1
  id: string
  name: string
  nodes: LoomNode[]
  createdAt: number
  updatedAt: number
}

export type DeckRuntime = {
  nodeId: string | null
  queue: string[]
  history: string[]
  auto: boolean
  loopBars: 4 | 8 | 16 | null
  loopStartBar: number | null
  /** `fromBar` is in the node's nominal bars from bar 0 (like `in`/`out`);
   * negative reaches into the kept tail — the join is `in − TAIL_BARS`. The
   * audible start is `barStartSeconds(node, fromBar)`, which floors at 0. */
  cue: { nodeId: string; fromBar: number } | null
}

export type LoomState = { loom: Loom; decks: Record<DeckId, DeckRuntime>; selected: string | null }

export type LoomAction =
  | { type: 'loom_loaded'; loom: Loom }
  // `name` is supplied by the caller (localised); the reducer holds no copy.
  | { type: 'loom_new'; id: string; now: number; name?: string }
  | { type: 'rename'; name: string }
  | {
      type: 'add_root_song'
      id: string
      file: string
      title: string
      bpm: number
      durationSeconds: number
      prompt: string | null
      now: number
    }
  | {
      type: 'weave'
      parentId: string | null
      prompt: string
      takes: number
      bars: number
      bpm: number
      loras: LoraChoice[]
      sa3: LoomNode['sa3']
      seeds: number[]
      ids: string[]
      takeGroup: string
      auto: boolean
      now: number
    }
  | { type: 'rendered'; id: string; file: string; durationSeconds: number }
  | { type: 'render_failed'; id: string; error: string }
  // A failed render goes back in the queue with the same recipe.
  | { type: 'retry'; id: string }
  | { type: 'key_known'; file: string; key: string }
  | { type: 'file_missing'; file: string }
  | { type: 'trim'; id: string; in: number; out: number }
  // Discards the other takes in the group.
  | { type: 'keep'; id: string }
  // Refused for a node a deck is on / has queued / with children.
  | { type: 'discard'; id: string }
  // Append to the queue (→A = next); `front` = a cut.
  | { type: 'tie'; deck: DeckId; id: string; front?: boolean }
  | { type: 'untie'; deck: DeckId; id: string }
  // The shuttle moved onto a node (load or swap).
  | { type: 'landed'; deck: DeckId; id: string }
  | { type: 'set_auto'; deck: DeckId; on: boolean }
  | { type: 'set_loop'; deck: DeckId; bars: 4 | 8 | 16 | null; startBar: number | null }
  // `id: null` stops the cue; `fromBar: null` = from the join.
  | { type: 'cue'; deck: DeckId; id: string | null; fromBar: number | null }
  | { type: 'select'; id: string | null }

const DECK_IDS: DeckId[] = ['a', 'b']

const emptyDeck = (): DeckRuntime => ({
  nodeId: null,
  queue: [],
  history: [],
  auto: false,
  loopBars: null,
  loopStartBar: null,
  cue: null,
})

export const emptyLoom = (id: string, now: number, name = ''): Loom => ({
  version: 1,
  id,
  name,
  nodes: [],
  createdAt: now,
  updatedAt: now,
})

/** A loom with both decks idle — the reducer's starting point. */
export const initialLoomState = (loom: Loom): LoomState => ({
  loom,
  decks: { a: emptyDeck(), b: emptyDeck() },
  selected: null,
})

// ---- selectors ----

export const nodeById = (nodes: LoomNode[], id: string | null) =>
  nodes.find((n) => n.id === id) ?? null

/** Children in weave order. */
export const kids = (nodes: LoomNode[], id: string) => nodes.filter((n) => n.parentId === id)

/** Root → node, inclusive; empty when the id is gone. */
export function pathTo(nodes: LoomNode[], id: string): LoomNode[] {
  const path: LoomNode[] = []
  for (let n = nodeById(nodes, id); n; n = nodeById(nodes, n.parentId)) path.unshift(n)
  return path
}

export const rootOf = (nodes: LoomNode[], id: string) => pathTo(nodes, id)[0] ?? null

/** The other members of a node's take group. */
export function takeSiblings(nodes: LoomNode[], id: string): LoomNode[] {
  const group = nodeById(nodes, id)?.takeGroup
  return group ? nodes.filter((n) => n.takeGroup === group && n.id !== id) : []
}

/** A deck is on the node or has it queued. */
export const isOnAir = (state: LoomState, id: string) =>
  DECK_IDS.some((d) => state.decks[d].nodeId === id || state.decks[d].queue.includes(id))

// ---- reducer ----

const withNodes = (state: LoomState, nodes: LoomNode[]): LoomState => ({
  ...state,
  loom: { ...state.loom, nodes },
})

/** Patch the matching nodes; the same state object back when none match, so a
 * no-op refresh (keys, missing files) neither re-renders nor autosaves. */
function patchNodes(
  state: LoomState,
  match: (n: LoomNode) => boolean,
  patch: (n: LoomNode) => Partial<LoomNode>,
): LoomState {
  let changed = false
  const nodes = state.loom.nodes.map((n) => {
    if (!match(n)) return n
    changed = true
    return { ...n, ...patch(n) }
  })
  return changed ? withNodes(state, nodes) : state
}

const withDeck = (state: LoomState, deck: DeckId, patch: Partial<DeckRuntime>): LoomState => ({
  ...state,
  decks: { ...state.decks, [deck]: { ...state.decks[deck], ...patch } },
})

/** Drop nodes, and any cue or selection pointing at them. */
function removeNodes(state: LoomState, ids: string[]): LoomState {
  const gone = new Set(ids)
  const decks = { ...state.decks }
  for (const d of DECK_IDS) {
    if (decks[d].cue && gone.has(decks[d].cue.nodeId)) decks[d] = { ...decks[d], cue: null }
  }
  return {
    ...withNodes(state, state.loom.nodes.filter((n) => !gone.has(n.id))),
    decks,
    selected: state.selected && gone.has(state.selected) ? null : state.selected,
  }
}

const removable = (state: LoomState, id: string) =>
  !isOnAir(state, id) && kids(state.loom.nodes, id).length === 0

function weave(state: LoomState, a: Extract<LoomAction, { type: 'weave' }>): LoomState {
  const parent = a.parentId === null ? null : nodeById(state.loom.nodes, a.parentId)
  if (a.parentId !== null && !parent) return state
  const takes = a.ids.slice(0, a.takes).map(
    (id, i): LoomNode => ({
      id,
      parentId: a.parentId,
      prompt: a.prompt,
      bars: a.bars,
      bpm: parent ? parent.bpm : a.bpm,
      seed: a.seeds[i] ?? 0,
      loras: a.loras,
      sa3: a.sa3,
      file: null,
      durationSeconds: null,
      // Set by `rendered`: the pipeline measures the clip.
      tailSeconds: 0,
      in: 0,
      out: a.bars,
      takeGroup: a.takes > 1 ? a.takeGroup : null,
      status: 'rendering',
      auto: a.auto,
      createdAt: a.now,
    }),
  )
  return withNodes(state, [...state.loom.nodes, ...takes])
}

/** Snap to whole bars, keep `0 ≤ in < out ≤ bars` with at least `MIN_BARS`
 * of body. The handle that did not move anchors the clamp, so dragging one
 * edge never shoves the other. */
function trim(node: LoomNode, inBar: number, outBar: number): Pick<LoomNode, 'in' | 'out'> {
  const minBody = Math.min(MIN_BARS, node.bars)
  const nextIn = Math.round(inBar)
  const nextOut = Math.round(outBar)
  if (nextIn !== node.in) {
    const out = Math.min(Math.max(nextOut, minBody), node.bars)
    return { in: Math.min(Math.max(nextIn, 0), out - minBody), out }
  }
  const start = Math.min(Math.max(nextIn, 0), node.bars - minBody)
  return { in: start, out: Math.min(Math.max(nextOut, start + minBody), node.bars) }
}

/** →A is *next*: a downstream node is reached through the tree (the
 * intermediate threads queue too); anything else jumps or returns at the
 * seam. A node already queued stays where it is. */
function tie(state: LoomState, deck: DeckId, id: string, front: boolean): LoomState {
  const { queue, nodeId } = state.decks[deck]
  const path = pathTo(state.loom.nodes, id).map((n) => n.id)
  if (path.length === 0) return state
  if (front) return withDeck(state, deck, { queue: [id, ...queue.filter((q) => q !== id)] })
  if (queue.includes(id)) return state
  const from = path.indexOf(queue.at(-1) ?? nodeId ?? '')
  const added = from >= 0 ? path.slice(from + 1).filter((q) => !queue.includes(q)) : [id]
  // Tying the node a deck is already headed for (or on, with nothing queued) is a no-op.
  return added.length ? withDeck(state, deck, { queue: [...queue, ...added] }) : state
}

export function loomReducer(state: LoomState, action: LoomAction): LoomState {
  switch (action.type) {
    case 'loom_loaded':
      return initialLoomState(action.loom)
    case 'loom_new':
      return initialLoomState(emptyLoom(action.id, action.now, action.name))
    case 'rename': {
      // An empty name would fail every autosave (the shell validates titles).
      const name = action.name.trim()
      return name ? { ...state, loom: { ...state.loom, name } } : state
    }
    case 'add_root_song': {
      // Floor, so the nominal end never passes the clip.
      const bars = Math.max(MIN_BARS, Math.floor(action.durationSeconds / barSeconds(action.bpm)))
      const node: LoomNode = {
        id: action.id,
        parentId: null,
        prompt: action.prompt ?? action.title,
        bars,
        bpm: action.bpm,
        seed: 0,
        loras: [],
        sa3: {},
        file: action.file,
        durationSeconds: action.durationSeconds,
        tailSeconds: 0,
        in: 0,
        out: bars,
        takeGroup: null,
        status: 'ready',
        auto: false,
        createdAt: action.now,
      }
      return withNodes(state, [...state.loom.nodes, node])
    }
    case 'weave':
      return weave(state, action)
    case 'rendered':
      return patchNodes(
        state,
        (n) => n.id === action.id,
        (n) => ({
          file: action.file,
          durationSeconds: action.durationSeconds,
          status: 'ready',
          error: undefined,
          // The kept tail is the surplus over the nominal body; a clip that
          // came back short has none, and the end clamp in timing does the rest.
          tailSeconds:
            n.parentId === null
              ? 0
              : Math.max(0, action.durationSeconds - n.bars * barSeconds(n.bpm)),
        }),
      )
    case 'render_failed':
      return patchNodes(
        state,
        (n) => n.id === action.id,
        () => ({ status: 'failed', error: action.error }),
      )
    case 'retry':
      return patchNodes(
        state,
        (n) => n.id === action.id && n.status === 'failed',
        () => ({ status: 'rendering', error: undefined }),
      )
    case 'key_known':
      return patchNodes(
        state,
        (n) => n.file === action.file && n.key !== action.key,
        () => ({ key: action.key }),
      )
    case 'file_missing':
      return patchNodes(
        state,
        (n) => n.file === action.file && n.status !== 'missing',
        () => ({ status: 'missing' }),
      )
    case 'trim': {
      // A NaN from a pointer maths slip would poison the saved loom.
      if (!Number.isFinite(action.in) || !Number.isFinite(action.out)) return state
      // The out-point of a parent is where its children start: refused. The
      // in-point always moves; a leaf's out-point moves its seam.
      const target = nodeById(state.loom.nodes, action.id)
      const movesOut = target !== null && Math.round(action.out) !== target.out
      if (movesOut && kids(state.loom.nodes, action.id).length > 0) return state
      return patchNodes(
        state,
        (n) => n.id === action.id,
        (n) => trim(n, action.in, action.out),
      )
    }
    case 'keep': {
      const group = nodeById(state.loom.nodes, action.id)?.takeGroup
      if (!group) return state
      // Never refused: unprotected takes go, the rest stay as ordinary nodes.
      // A take with children is protected too — removing it would orphan its
      // branch, and parseLoom refuses a loom with an orphan.
      const others = takeSiblings(state.loom.nodes, action.id)
      const gone = others.filter((n) => removable(state, n.id)).map((n) => n.id)
      return patchNodes(
        removeNodes(state, gone),
        (n) => n.takeGroup === group,
        () => ({ takeGroup: null }),
      )
    }
    case 'discard':
      // An on-air node is never in a queue once discarded: isOnAir refuses it.
      if (!nodeById(state.loom.nodes, action.id) || !removable(state, action.id)) return state
      return removeNodes(state, [action.id])
    case 'tie':
      return tie(state, action.deck, action.id, action.front === true)
    case 'untie':
      return withDeck(state, action.deck, {
        queue: state.decks[action.deck].queue.filter((q) => q !== action.id),
      })
    case 'landed': {
      const deck = state.decks[action.deck]
      return withDeck(state, action.deck, {
        history: deck.nodeId ? [...deck.history, deck.nodeId] : deck.history,
        nodeId: action.id,
        queue: deck.queue[0] === action.id ? deck.queue.slice(1) : deck.queue,
        loopBars: null,
        loopStartBar: null,
      })
    }
    case 'set_auto':
      return withDeck(state, action.deck, { auto: action.on })
    case 'set_loop':
      return withDeck(state, action.deck, { loopBars: action.bars, loopStartBar: action.startBar })
    case 'cue': {
      const node = nodeById(state.loom.nodes, action.id)
      if (action.id !== null && !node) return state
      const cue = node ? { nodeId: node.id, fromBar: action.fromBar ?? node.in - TAIL_BARS } : null
      // One audition path: a new cue replaces the other deck's.
      const decks = { ...state.decks }
      for (const d of DECK_IDS) decks[d] = { ...decks[d], cue: d === action.deck ? cue : null }
      return { ...state, decks }
    }
    case 'select':
      return { ...state, selected: action.id }
  }
}

/** The loom as saved: runtime stripped, `updatedAt` bumped to `now`. */
export const persistable = (state: LoomState, now: number): Loom => ({ ...state.loom, updatedAt: now })

// ---- parse (a `read_loom` payload is untrusted JSON) ----

// `queued` is a render-queue state a saved loom may carry; it parses like `rendering`.
const STATUSES = ['queued', 'rendering', 'ready', 'failed', 'missing']

const isRecord = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null
const isNum = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v)
const isStr = (v: unknown): v is string => typeof v === 'string'
const optional = <T>(v: unknown, is: (v: unknown) => v is T): v is T | undefined => v === undefined || is(v)

function parseNode(v: unknown, seen: Set<string>): LoomNode | null {
  if (!isRecord(v)) return null
  const { id, parentId, prompt, bars, bpm, seed, loras, sa3, file, durationSeconds } = v
  const { tailSeconds, takeGroup, status, error, auto, key, createdAt } = v
  const inBar = v.in
  const outBar = v.out
  // A parent must come earlier in the list — which also rules out cycles.
  if (!isStr(id) || id === '' || seen.has(id)) return null
  if (parentId !== null && !(isStr(parentId) && seen.has(parentId))) return null
  if (!isStr(prompt) || !isNum(bars) || !isNum(bpm) || bpm <= 0 || !isNum(seed)) return null
  if (!Array.isArray(loras) || !loras.every((l) => isRecord(l) && isStr(l.name) && isNum(l.strength))) return null
  if (!isRecord(sa3) || !optional(sa3.cfg, isNum) || !optional(sa3.apg, isNum)) return null
  if (!optional(sa3.negativePrompt, isStr)) return null
  if (!(file === null || isStr(file)) || !(durationSeconds === null || isNum(durationSeconds))) return null
  if (!isNum(tailSeconds) || tailSeconds < 0) return null
  if (!isNum(inBar) || !isNum(outBar) || inBar < 0 || inBar >= outBar || outBar > bars) return null
  if (!(takeGroup === null || isStr(takeGroup)) || !isStr(status) || !STATUSES.includes(status)) return null
  if (!optional(error, isStr) || typeof auto !== 'boolean' || !optional(key, isStr) || !isNum(createdAt)) return null
  return {
    id,
    parentId,
    prompt,
    bars,
    bpm,
    seed,
    loras: loras.map((l) => ({ name: l.name, strength: l.strength })),
    sa3: { cfg: sa3.cfg, apg: sa3.apg, negativePrompt: sa3.negativePrompt },
    file,
    durationSeconds,
    tailSeconds,
    in: inBar,
    out: outBar,
    takeGroup,
    // The render queue lives in memory: a node saved queued or mid-render has
    // no job after a relaunch, so it reopens failed (a retry re-weaves it),
    // never a ghost stuck behind a leaf forever.
    status: status === 'queued' || status === 'rendering' ? 'failed' : (status as LoomNode['status']),
    error,
    auto,
    key,
    createdAt,
  }
}

/** Validate a `read_loom` payload; `null` on any violation. Unknown fields are
 * dropped. */
export function parseLoom(value: unknown): Loom | null {
  if (!isRecord(value) || value.version !== 1) return null
  const { id, name, nodes, createdAt, updatedAt } = value
  if (!isStr(id) || !isStr(name) || !isNum(createdAt) || !isNum(updatedAt) || !Array.isArray(nodes)) return null
  const seen = new Set<string>()
  const parsed: LoomNode[] = []
  for (const raw of nodes) {
    const node = parseNode(raw, seen)
    if (!node) return null
    seen.add(node.id)
    parsed.push(node)
  }
  return { version: 1, id, name, nodes: parsed, createdAt, updatedAt }
}
