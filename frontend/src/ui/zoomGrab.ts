/** The close-up's window: a few bars at club tempo. */
export const WINDOW_SECONDS = 4
/** Scratch capacity: the worst case is varispeed's fast end packing
 * more hops into the window. */
export const MAX_HOPS = 512

/** How many hops the zoom strip spans edge to edge at this deck's
 * wall-time-per-hop — the one geometry the drawing and the grab share. */
export function zoomWindowHops(realSecondsPerHop: number): number {
  return Math.min(MAX_HOPS - 1, Math.ceil(WINDOW_SECONDS / realSecondsPerHop))
}

/** A pointer drag along the strip's time axis, in hops: the content
 * moves with the finger, so dragging against the scroll (left, or up
 * on a vertical strip) pulls later audio under the playhead — positive. */
export function dragHops(
  deltaPx: number,
  axisPx: number,
  windowHops: number,
): number {
  if (axisPx <= 0) return 0
  return (-deltaPx * windowHops) / axisPx
}

/** The steepest pitch bend a grab may cause while playing: a beatmatching
 * touch on the platter edge, never a scratch. */
export const GRAB_MAX_BEND = 0.1
/** Longest gap between pointer moves that still earns bend allowance, so
 * a pause-then-flick can't bank a big slip. */
const GRAB_MAX_ELAPSED_SECONDS = 0.05

/** Turn a playing grab's drag (track seconds since the last move) into
 * a phase nudge the engine's bend can absorb within ±GRAB_MAX_BEND of
 * the set rate: the nudge per move is capped at what that bend clears
 * in the elapsed time. The engine bends in proportion to its slip
 * backlog, so a steady feed at this cap settles at the cap. Excess drag
 * is dropped, not queued — release lands where the bend got to. */
export function grabNudgeSeconds(
  dragSeconds: number,
  elapsedSeconds: number,
  rate: number,
): number {
  if (!Number.isFinite(dragSeconds) || !(elapsedSeconds > 0)) return 0
  const limit =
    GRAB_MAX_BEND * rate * Math.min(elapsedSeconds, GRAB_MAX_ELAPSED_SECONDS)
  return Math.max(-limit, Math.min(limit, dragSeconds))
}
