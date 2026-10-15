/** Pure edits on a playlist's ordered file list. Each returns a new array (the
 * shell takes the whole list on every save), never mutating its input. */

/** Insert `file` at `index` (default: the end); a file already present is left
 * where it is, so a repeated drop is a no-op. */
export function insertFile(files: string[], file: string, index = files.length): string[] {
  if (files.includes(file)) return files
  return [...files.slice(0, index), file, ...files.slice(index)]
}

/** Move the file at `from` so it ends up at index `to`. */
export function moveFile(files: string[], from: number, to: number): string[] {
  const next = [...files]
  const [file] = next.splice(from, 1)
  next.splice(to, 0, file)
  return next
}

export function removeFile(files: string[], file: string): string[] {
  return files.filter((entry) => entry !== file)
}
