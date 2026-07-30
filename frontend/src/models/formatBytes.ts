/** On-disk size for a model/adapter row: whole GB past 1e9, else whole MB. */
export function formatBytes(bytes: number): string {
  if (bytes <= 0) return '0 MB'
  const gb = bytes / 1e9
  if (gb >= 1) return `${gb.toFixed(1)} GB`
  return `${Math.max(1, Math.round(bytes / 1e6))} MB`
}
