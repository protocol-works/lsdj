/** Camelot key → hue on the circle of fifths. Camelot numbers already walk
 * the circle (8 → 9 is a fifth up), so each number is a 30° step and a
 * relative minor/major pair (8A/8B) shares a hue; 8 (A minor / C major) sits
 * at 0° as in the mockup. An unknown or missing key is null. */
export function keyHue(key: string | null | undefined): number | null {
  const match = /^(1[0-2]|[1-9])[AB]$/i.exec(key?.trim() ?? '')
  return match ? ((Number(match[1]) + 4) % 12) * 30 : null
}
