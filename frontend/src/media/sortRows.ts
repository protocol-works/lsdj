/** The Generate/Samples list order (a persisted per-user preference). */
export type MediaSort = 'newest' | 'rating' | 'title' | 'bpm' | 'key'

/** A Camelot code ("8A") as a wheel position, 1A=0 … 12B=23; unknown sorts last. */
function camelotRank(key: string | undefined): number {
  const match = key?.match(/^(\d{1,2})([AB])$/)
  return match ? (Number(match[1]) - 1) * 2 + (match[2] === 'B' ? 1 : 0) : Infinity
}

/** Order a library list for display and for the rotary. The input is already
 * newest-first, and the sort is stable, so 'newest' is the identity and every
 * tie keeps newest-first. Pending (composing) rows always pin to the top; an
 * unrated row (rating 0 or absent) sorts below every rated one, and a row with
 * no BPM/key verdict below every row with one. */
export function sortRows<
  R extends { state: string; title: string; rating?: number; bpm?: number; key?: string },
>(rows: R[], sort: MediaSort): R[] {
  return [...rows].sort((a, b) => {
    const pending = Number(b.state === 'pending') - Number(a.state === 'pending')
    if (pending !== 0 || sort === 'newest') return pending
    if (sort === 'rating') return (b.rating ?? 0) - (a.rating ?? 0)
    if (sort === 'bpm') return byNumber(a.bpm ?? Infinity, b.bpm ?? Infinity)
    if (sort === 'key') return byNumber(camelotRank(a.key), camelotRank(b.key))
    return a.title.localeCompare(b.title, undefined, { sensitivity: 'base' })
  })
}

/** Ascending, with Infinity (unknown) ties kept stable rather than NaN. */
function byNumber(a: number, b: number): number {
  return a === b ? 0 : a < b ? -1 : 1
}
