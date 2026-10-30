/** Loom timing (ADR-0038 draft, plan decision 3): every seam, loop, cut and
 * trim sits on the loom's *nominal* grid — bars of `240 / bpm` seconds
 * measured from a node's own bar 0, which falls after the kept parent tail
 * at the clip start. The detected beatgrid is ignored: SA3 was told the
 * tempo, and a wrong detection must not move a seam. Every derived time is
 * clamped to the real clip, because the engine refuses a loop past the
 * buffer and a playhead that runs off the end parks silent. */

import type { LoraChoice } from '../models/useLoras'
import type { LoomNode } from './loomState'

/** A hold loops the last four bars before the out-point. */
export const HOLD_BARS = 4
/** A continuation is conditioned on (and starts with) the parent's last four bars. */
export const TAIL_BARS = 4
/** The shortest body a trim or a dropped song may have. */
export const MIN_BARS = 4
export const DEFAULT_BARS = 16
export const DEFAULT_BPM = 124

/** Measured render cost, in seconds. `rate`: SA3 medium composes 2:00 in
 * 14.9 s wall (ROADMAP M19), ≈0.125× realtime; `base`: the per-request CLI
 * start (ADR-0012, nothing stays warm); `mergePerLora`: the adapter merge
 * into the DiT, 127.7 s flat for one adapter (docs/mcp-findings.md #16) and
 * worse than linear for a stack (#17) — so per adapter is a lower bound. */
export const RENDER = { base: 3, rate: 0.125, mergePerLora: 128 }

/** The fields the timing maths reads; any `LoomNode` fits. */
export type NodeSpan = Pick<LoomNode, 'bpm' | 'tailSeconds' | 'in' | 'out' | 'durationSeconds'>

export const barSeconds = (bpm: number) => 240 / bpm

/** Clip time where nominal bar `bar` starts, clamped into the clip. A negative
 * bar reaches back into the kept tail (the join); 0 floors it. */
export function barStartSeconds(node: NodeSpan, bar: number): number {
  const seconds = node.tailSeconds + bar * barSeconds(node.bpm)
  return Math.min(Math.max(0, seconds), node.durationSeconds ?? Infinity)
}

export const nodeStartSeconds = (node: NodeSpan) => barStartSeconds(node, node.in)

/** The seam: the out-point, or the clip end when the clip is shorter. */
export const nodeEndSeconds = (node: NodeSpan) => barStartSeconds(node, node.out)

/** Fractional nominal bar at a clip position (negative inside the kept tail). */
export const barAt = (node: NodeSpan, positionSeconds: number) =>
  (positionSeconds - node.tailSeconds) / barSeconds(node.bpm)

// Positions sitting exactly on a bar line come back a hair short through
// floating point; this keeps them on their own bar.
const EPSILON_BARS = 1e-6

/** The next bar line strictly after `position` — where a cut lands. */
export const nextBarSeconds = (node: NodeSpan, position: number) =>
  barStartSeconds(node, Math.floor(barAt(node, position) + EPSILON_BARS) + 1)

/** A `bars`-long loop from `startBar`, kept inside the body. When the end
 * clamps (the out-point, or a clip a few frames short) the start slides back
 * to keep the length, so a long hold never drifts off the tempo. */
export function loopRegion(node: NodeSpan, startBar: number, bars: number) {
  const end = Math.min(barStartSeconds(node, Math.max(startBar, node.in) + bars), nodeEndSeconds(node))
  const start = Math.max(nodeStartSeconds(node), end - bars * barSeconds(node.bpm))
  return { start, end }
}

/** The hold at an untied seam: the last `HOLD_BARS` bars before the out-point. */
export const holdRegion = (node: NodeSpan) => loopRegion(node, node.out - HOLD_BARS, HOLD_BARS)

/** Whole bars left before the seam — a countdown that only reads 0 at the seam. */
export const barsToSeam = (node: NodeSpan, position: number) =>
  Math.max(0, Math.ceil((nodeEndSeconds(node) - position) / barSeconds(node.bpm) - EPSILON_BARS))

/** Seconds to render a clip of `tailBars + bars` at `bpm` with a LoRA stack. */
export function renderEta(job: { bars: number; tailBars: number; bpm: number; loras: LoraChoice[] }): number {
  const clip = (job.tailBars + job.bars) * barSeconds(job.bpm)
  return RENDER.base + RENDER.rate * clip + RENDER.mergePerLora * job.loras.length
}

/** How late a render will be for the seam; 0 when it is in time. */
export const lateBy = (etaSeconds: number, secondsToSeam: number) => Math.max(0, etaSeconds - secondsToSeam)

/** `m:ss`, rounded, never negative. */
export function fmtClock(seconds: number): string {
  const s = Math.max(0, Math.round(seconds))
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`
}
