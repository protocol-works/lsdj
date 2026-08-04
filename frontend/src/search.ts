/** Case-insensitive AND search across a row's user-facing metadata. Each query
 * term may match a different field, so "dub magenta" can match a title + model. */
export function matchesSearch(
  query: string,
  ...values: Array<string | null | undefined>
): boolean {
  const terms = query.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean)
  if (terms.length === 0) return true
  const searchable = values.filter(Boolean).join(' ').toLocaleLowerCase()
  return terms.every((term) => searchable.includes(term))
}
