/** The Generate/Samples list order (a persisted per-user preference). */
export type MediaSort = 'newest' | 'rating' | 'title'

/** Order a library list for display and for the rotary. The input is already
 * newest-first, and the sort is stable, so 'newest' is the identity and every
 * tie keeps newest-first. Pending (composing) rows always pin to the top; an
 * unrated row (rating 0 or absent) sorts below every rated one. */
export function sortRows<R extends { state: string; title: string; rating?: number }>(
  rows: R[],
  sort: MediaSort,
): R[] {
  return [...rows].sort((a, b) => {
    const pending = Number(b.state === 'pending') - Number(a.state === 'pending')
    if (pending !== 0 || sort === 'newest') return pending
    if (sort === 'rating') return (b.rating ?? 0) - (a.rating ?? 0)
    return a.title.localeCompare(b.title, undefined, { sensitivity: 'base' })
  })
}
