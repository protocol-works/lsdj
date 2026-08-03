import { Line } from '@react-three/drei'
import { Canvas, useFrame } from '@react-three/fiber'
import { useEffect, useRef, useState } from 'react'
import type { Group } from 'three'

import { cubeSegments, type Segment, type Vec3 } from './hypercube'

/** The LSDJ mark: a vinyl record spinning face-on — its cue marker sweeps so the
 * spin reads — with a hypercube (a cube nested inside a cube) tumbling in 3-D at
 * the centre. A deliberately spare set of lines — record edge, cue marker, and
 * the two nested cubes — so it stays clean at any size (matching the app icon).
 * Built on three.js / react-three-fiber. Two theme-aware hues from the accent
 * triad (<html data-accent>): record body + inner cube = master accent ("blue");
 * cue marker + outer cube = deck-a ("green"). Motion honours
 * prefers-reduced-motion. */

// Normalised units: the record's outer edge is ~0.92 of the scene radius.
const RECORD_EDGE = 0.92
const CUE_INNER = 0.6
const CUE_OUTER = 0.9
const HYPERCUBE_SCALE = 0.26 // half-width of the outer cube
const HYPERCUBE_INNER = 0.5 // inner cube as a fraction of the outer
const DISC_SPIN = 1.2 // rad/s — the record (and its cue marker)
const TUMBLE_Y = 0.7 // rad/s — the hypercube's 3-D tumble
const TUMBLE_X = 0.45
const REST_TUMBLE: [number, number, number] = [0.25, 0.4, 0] // pose when motion is reduced

// The cue marker rests pointing to the lower-right (matching the static mark);
// the spinning record sweeps it clockwise from there.
const CUE_ANGLE = -Math.PI / 4
const CUE_POINTS: [number, number, number][] = [
  [Math.cos(CUE_ANGLE) * CUE_INNER, Math.sin(CUE_ANGLE) * CUE_INNER, 0],
  [Math.cos(CUE_ANGLE) * CUE_OUTER, Math.sin(CUE_ANGLE) * CUE_OUTER, 0],
]

const scaled = (p: Vec3): [number, number, number] => [
  p[0] * HYPERCUBE_SCALE,
  p[1] * HYPERCUBE_SCALE,
  p[2] * HYPERCUBE_SCALE,
]
const flatten = (segments: Segment[]): [number, number, number][] =>
  segments.flatMap(([a, b]) => [scaled(a), scaled(b)])

// Two plain nested cubes (no connecting struts), each in its own hue.
const HYPERCUBE_OUTER_POINTS = flatten(cubeSegments(1))
const HYPERCUBE_INNER_POINTS = flatten(cubeSegments(HYPERCUBE_INNER))

function ring(radius: number, steps = 72): [number, number, number][] {
  const points: [number, number, number][] = []
  for (let i = 0; i <= steps; i++) {
    const angle = (i / steps) * Math.PI * 2
    points.push([Math.cos(angle) * radius, Math.sin(angle) * radius, 0])
  }
  return points
}

/** The record: just the edge in the body colour and the radial cue marker in the
 * trim colour. Lives in a group that spins. */
function Record({ bodyColor, trimColor }: { bodyColor: string; trimColor: string }) {
  return (
    <>
      <Line points={ring(RECORD_EDGE)} color={bodyColor} lineWidth={2.2} />
      <Line points={CUE_POINTS} color={trimColor} lineWidth={1.8} />
    </>
  )
}

/** The hypercube label: a cube nested in a cube, each a single hue — no struts,
 * so it reads clean while it tumbles. */
function Hypercube({
  outerColor,
  innerColor,
}: {
  outerColor: string
  innerColor: string
}) {
  return (
    <>
      <Line points={HYPERCUBE_OUTER_POINTS} segments color={outerColor} lineWidth={1.6} />
      <Line points={HYPERCUBE_INNER_POINTS} segments color={innerColor} lineWidth={1.7} />
    </>
  )
}

/** The record spins around its facing axis (the cue marker sweeps) while the
 * hypercube tumbles independently in 3-D at the centre. Both are driven
 * imperatively in useFrame (not via props) so an accent re-render never resets
 * the motion. */
function Scene({
  bodyColor,
  trimColor,
  spinning,
}: {
  bodyColor: string
  trimColor: string
  spinning: boolean
}) {
  const record = useRef<Group>(null)
  const hypercube = useRef<Group>(null)
  useFrame((_, delta) => {
    if (record.current) {
      record.current.rotation.z = spinning ? record.current.rotation.z - delta * DISC_SPIN : 0
    }
    if (hypercube.current) {
      if (spinning) {
        hypercube.current.rotation.y += delta * TUMBLE_Y
        hypercube.current.rotation.x += delta * TUMBLE_X
      } else {
        hypercube.current.rotation.set(...REST_TUMBLE)
      }
    }
  })
  return (
    <>
      <group ref={record}>
        <Record bodyColor={bodyColor} trimColor={trimColor} />
      </group>
      <group ref={hypercube}>
        {/* Outer cube = deck-a, inner cube = accent. */}
        <Hypercube outerColor={trimColor} innerColor={bodyColor} />
      </group>
    </>
  )
}

type Props = {
  className?: string
}

export function HypercubeMark({ className }: Props) {
  const hostRef = useRef<HTMLDivElement>(null)
  const [bodyColor, setBodyColor] = useState('#22d3ee') // record + inner cube — master accent
  const [trimColor, setTrimColor] = useState('#bef264') // cue + outer cube — deck-a
  const [spinning, setSpinning] = useState(true)

  useEffect(() => {
    const host = hostRef.current
    if (!host) return

    // The host inherits `color: var(--color-accent)` from .logo__mark, so its
    // resolved `color` is the live master accent (var() chains substituted); the
    // trim hue is deck-a. Both re-read on a theme switch.
    const readColors = () => {
      const styles = getComputedStyle(host)
      setBodyColor(styles.color || '#22d3ee')
      setTrimColor(styles.getPropertyValue('--color-deck-a').trim() || '#bef264')
    }
    readColors()
    const accentObserver = new MutationObserver(readColors)
    accentObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-accent'],
    })

    const motion = window.matchMedia('(prefers-reduced-motion: reduce)')
    const applyMotion = () => setSpinning(!motion.matches)
    applyMotion()
    motion.addEventListener('change', applyMotion)

    return () => {
      accentObserver.disconnect()
      motion.removeEventListener('change', applyMotion)
    }
  }, [])

  return (
    <div ref={hostRef} className={className}>
      <Canvas
        frameloop={spinning ? 'always' : 'demand'}
        camera={{ position: [0, 0, 2.6], fov: 50 }}
        dpr={[1, 2]}
        gl={{ alpha: true, antialias: true }}
        style={{ width: '100%', height: '100%' }}
      >
        <Scene bodyColor={bodyColor} trimColor={trimColor} spinning={spinning} />
      </Canvas>
    </div>
  )
}
