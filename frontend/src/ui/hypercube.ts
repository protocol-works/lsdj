/** Cube geometry for the LSDJ hypercube — pure data, no DOM or three.js, so the
 * line topology can be unit-tested. The mark's label is a cube nested inside a
 * cube (the recognisable hypercube read); `cubeSegments(scale)` returns one
 * cube's edges at a given size, and the renderer draws an outer + inner cube in
 * different hues. */

export type Vec3 = readonly [number, number, number]
export type Edge = readonly [number, number]
export type Segment = readonly [Vec3, Vec3]

/** The 8 corners of a cube: every (±1, ±1, ±1). The corner index is a bitmask —
 * bit n set means axis n is +1. */
export function cubeCorners(): Vec3[] {
  const corners: Vec3[] = []
  for (let i = 0; i < 8; i++) {
    corners.push([i & 1 ? 1 : -1, i & 2 ? 1 : -1, i & 4 ? 1 : -1])
  }
  return corners
}

/** The 12 edges of a cube: corner pairs that differ on exactly one axis, i.e.
 * whose indices differ by a single bit. The `i < j` guard yields each once. */
export function cubeEdges(): Edge[] {
  const edges: Edge[] = []
  for (let i = 0; i < 8; i++) {
    for (const bit of [1, 2, 4]) {
      const j = i ^ bit
      if (i < j) edges.push([i, j])
    }
  }
  return edges
}

/** The 12 edges of a cube at the given scale, as line segments. */
export function cubeSegments(scale = 1): Segment[] {
  const corners = cubeCorners()
  const at = (i: number): Vec3 => [
    corners[i][0] * scale,
    corners[i][1] * scale,
    corners[i][2] * scale,
  ]
  return cubeEdges().map(([a, b]) => [at(a), at(b)])
}
