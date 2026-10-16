/** Near-silence for Auto-trim: a peak under ≈ −40 dBFS. */
export const SILENCE_THRESHOLD = 0.01

/** The shortest crop the panel allows, in seconds. */
export const MIN_CROP_SECONDS = 0.05

export type Peaks = { min: Float32Array; max: Float32Array }

/** The window from the first to the last bucket louder than `threshold`, or null
 * for an all-silent clip. Bucket-coarse and conservative: it keeps the whole
 * bucket an onset sits in, so it never trims sound away. */
export function trimSilence(
  peaks: Peaks,
  duration: number,
  threshold = SILENCE_THRESHOLD,
): { start: number; end: number } | null {
  const buckets = peaks.min.length
  const loud = (index: number) => Math.max(-peaks.min[index], peaks.max[index]) > threshold
  let first = 0
  while (first < buckets && !loud(first)) first++
  if (first === buckets) return null
  let last = buckets - 1
  while (!loud(last)) last--
  return {
    start: roundMs((first * duration) / buckets),
    end: roundMs(((last + 1) * duration) / buckets),
  }
}

/** Seconds to the millisecond — finer than any edit, and keeps the fields tidy. */
export function roundMs(seconds: number): number {
  return Math.round(seconds * 1000) / 1000
}

/** Move one edge of a crop, keeping start < end by {@link MIN_CROP_SECONDS} and
 * both inside the clip. */
export function moveEdge(
  crop: { start: number; end: number },
  edge: 'start' | 'end',
  seconds: number,
  duration: number,
): { start: number; end: number } {
  if (!Number.isFinite(seconds)) return crop
  if (edge === 'start') {
    return { ...crop, start: roundMs(Math.min(Math.max(0, seconds), crop.end - MIN_CROP_SECONDS)) }
  }
  return { ...crop, end: roundMs(Math.max(Math.min(duration, seconds), crop.start + MIN_CROP_SECONDS)) }
}
