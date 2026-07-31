/** Pure signal-to-visual math for the performance backdrop (issue #94).
 * The caller supplies both clocks: RAF delta for smoothing, and the native
 * audio-context time for speaker-aligned beat phase. No DOM or React state
 * belongs in this module. */

export const LEVEL_ATTACK_SECONDS = 0.045
export const LEVEL_RELEASE_SECONDS = 0.3
export const PULSE_DECAY_SECONDS = 0.14
export const SILENCE_EPSILON = 0.002
export const MAX_FRAME_DELTA_SECONDS = 0.1
export const REDUCED_MOTION_WASH = 0.5

export type VisualBeatClock = {
  periodSeconds: number
  beatAtContext: number
}

export type VisualDeckFrameInput = {
  level: number
  audible: boolean
  beat: VisualBeatClock | null
}

export type VisualFrameInput = {
  deltaSeconds: number
  contextTime: number | null
  crossfade: number
  masterLevel: number
  decks: {
    a: VisualDeckFrameInput
    b: VisualDeckFrameInput
  }
}

export type VisualFrameState = {
  smoothedMaster: number
  smoothedLevels: { a: number; b: number }
  wash: { a: number; b: number }
  pulse: { a: number; b: number }
}

export const INITIAL_VISUAL_FRAME_STATE: VisualFrameState = {
  smoothedMaster: 0,
  smoothedLevels: { a: 0, b: 0 },
  wash: { a: 0, b: 0 },
  pulse: { a: 0, b: 0 },
}

export function clamp01(value: number): number {
  if (!Number.isFinite(value)) return 0
  return Math.max(0, Math.min(1, value))
}

export function positiveModulo(value: number, modulus: number): number {
  if (!Number.isFinite(value) || !Number.isFinite(modulus) || modulus <= 0) {
    return 0
  }
  const remainder = value % modulus
  return remainder < 0 ? remainder + modulus : remainder || 0
}

/** The exact equal-power law used by the Rust graph. An invalid position is
 * inert rather than guessed, matching the visual layer's honesty rule. */
export function equalPowerGains(position: number): { a: number; b: number } {
  if (!Number.isFinite(position)) return { a: 0, b: 0 }
  const clamped = clamp01(position)
  if (clamped === 0) return { a: 1, b: 0 }
  if (clamped === 1) return { a: 0, b: 1 }
  const angle = clamped * Math.PI / 2
  return { a: Math.cos(angle), b: Math.sin(angle) }
}

export function smoothLevel(
  previous: number,
  target: number,
  deltaSeconds: number,
): number {
  const from = clamp01(previous)
  const to = clamp01(target)
  const delta = Math.max(
    0,
    Math.min(
      MAX_FRAME_DELTA_SECONDS,
      Number.isFinite(deltaSeconds) ? deltaSeconds : 0,
    ),
  )
  if (delta === 0 || from === to) return from
  const timeConstant = to > from ? LEVEL_ATTACK_SECONDS : LEVEL_RELEASE_SECONDS
  const alpha = 1 - Math.exp(-delta / timeConstant)
  return clamp01(from + (to - from) * alpha)
}

export function perceptualLevel(value: number): number {
  return Math.sqrt(clamp01(value))
}

/** Normalized deck shares from post-fader levels plus the engine's crossfade.
 * Audibility is a hard gate after smoothing: a primed/faded-out deck leaves no
 * misleading visual release tail. */
export function contributionBalance(
  levels: { a: number; b: number },
  audible: { a: boolean; b: boolean },
  crossfade: number,
): { a: number; b: number } {
  const gains = equalPowerGains(crossfade)
  const rawA = audible.a ? clamp01(levels.a) * gains.a : 0
  const rawB = audible.b ? clamp01(levels.b) * gains.b : 0
  const total = rawA + rawB
  if (!Number.isFinite(total) || total <= SILENCE_EPSILON) {
    return { a: 0, b: 0 }
  }
  return { a: rawA / total, b: rawB / total }
}

export function secondsSinceBeat(
  contextTime: number | null,
  beat: VisualBeatClock | null,
): number | null {
  if (
    contextTime === null ||
    !Number.isFinite(contextTime) ||
    !beat ||
    !Number.isFinite(beat.periodSeconds) ||
    beat.periodSeconds <= 0 ||
    !Number.isFinite(beat.beatAtContext)
  ) {
    return null
  }
  return positiveModulo(contextTime - beat.beatAtContext, beat.periodSeconds)
}

export function beatPulse(
  contextTime: number | null,
  beat: VisualBeatClock | null,
): number {
  const sinceBeat = secondsSinceBeat(contextTime, beat)
  return sinceBeat === null
    ? 0
    : clamp01(Math.exp(-sinceBeat / PULSE_DECAY_SECONDS))
}

/** Static, interaction-derived wash for reduced motion. It intentionally does
 * not sample live levels and never produces a pulse. */
export function reducedMotionWash(
  crossfade: number,
  audible: { a: boolean; b: boolean },
): { a: number; b: number } {
  const balance = contributionBalance(
    { a: audible.a ? 1 : 0, b: audible.b ? 1 : 0 },
    audible,
    crossfade,
  )
  return {
    a: balance.a * REDUCED_MOTION_WASH,
    b: balance.b * REDUCED_MOTION_WASH,
  }
}

export function reduceVisualFrame(
  previous: VisualFrameState,
  input: VisualFrameInput,
): VisualFrameState {
  const smoothedLevels = {
    a: smoothLevel(previous.smoothedLevels.a, input.decks.a.level, input.deltaSeconds),
    b: smoothLevel(previous.smoothedLevels.b, input.decks.b.level, input.deltaSeconds),
  }
  const smoothedMaster = smoothLevel(
    previous.smoothedMaster,
    input.masterLevel,
    input.deltaSeconds,
  )
  const balance = contributionBalance(
    smoothedLevels,
    { a: input.decks.a.audible, b: input.decks.b.audible },
    input.crossfade,
  )
  const energy = perceptualLevel(smoothedMaster)
  const wash = {
    a: clamp01(balance.a * energy),
    b: clamp01(balance.b * energy),
  }
  return {
    smoothedMaster,
    smoothedLevels,
    wash,
    pulse: {
      a:
        input.decks.a.audible && wash.a > SILENCE_EPSILON
          ? clamp01(beatPulse(input.contextTime, input.decks.a.beat) * wash.a)
          : 0,
      b:
        input.decks.b.audible && wash.b > SILENCE_EPSILON
          ? clamp01(beatPulse(input.contextTime, input.decks.b.beat) * wash.b)
          : 0,
    },
  }
}
