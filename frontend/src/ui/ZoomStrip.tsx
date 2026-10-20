import { useEffect, useRef } from 'react'
import type { PointerEvent } from 'react'

import { MAX_HOPS, dragHops, zoomWindowHops } from './zoomGrab'

const WIDTH = 720
const HEIGHT = 88
/** RMS → pixel scaling: sqrt-compressed like the live strip, gained
 * so techno sits around two-thirds height. */
const AMPLITUDE_GAIN = 1.8

type ZoomStripSource = {
  bands: {
    copyWindow: (
      fromHop: number,
      target: { low: Float32Array; mid: Float32Array; high: Float32Array },
    ) => void
  }
  playheadHop: number
  realSecondsPerHop: number
  beat: { periodHops: number; anchorHop: number } | null
  /** Filled hot cues in hop units (M21), in the playhead's domain. */
  cues: number[]
  /** The active loop region in hop units (M21), or null when not
   * looping — washed and bracketed so the wrap is legible up close. */
  loop: { startHop: number; endHop: number } | null
}

/** One pointer move of a grab, in the strip's hop domain: where the
 * playhead sat when the grab began, this move's drag and the drag so
 * far (positive = later audio pulled under the playhead), and the time
 * since the previous move. */
export type ZoomGrab = {
  startHop: number
  deltaHops: number
  totalHops: number
  elapsedSeconds: number
}

type ZoomStripProps = {
  label: string
  /** Which deck this strip draws — selects its CSS palette (--a / --b)
   * and colours the playhead, loop wash and band tint. */
  deck: 'a' | 'b'
  /** Time runs downward instead of rightward (the Serato vertical
   * waveform convention) — same drawing, transposed onto the canvas. */
  vertical?: boolean
  /** Polled per frame; null draws an empty (dimmed) strip — a deck
   * with nothing honest to show shows nothing (M22). */
  getSource: () => ZoomStripSource | null
  /** Makes the strip grabbable: called per pointer move while held.
   * Omitted, the strip is inert (a live deck — ADR-0004). */
  onGrab?: (grab: ZoomGrab) => void
}

/** One deck's scrolling band-coloured close-up (M22): lows, mids and
 * highs as colour, the playhead fixed mid-strip, beat marks from the
 * deck's M20 clock where confident. Canvas-rendered per frame; React
 * never sees the scroll. */
export function ZoomStrip({
  label,
  deck,
  vertical,
  getSource,
  onGrab,
}: ZoomStripProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null)
  const grabRef = useRef<{
    pointerId: number
    startHop: number
    windowHops: number
    axisPx: number
    lastPx: number
    lastTime: number
    totalPx: number
  } | null>(null)

  const axisOf = (event: PointerEvent<HTMLCanvasElement>) =>
    vertical ? event.clientY : event.clientX

  const handlePointerDown = (event: PointerEvent<HTMLCanvasElement>) => {
    if (!onGrab || event.button !== 0) return
    const source = getSource()
    if (!source) return
    const rect = event.currentTarget.getBoundingClientRect()
    event.currentTarget.setPointerCapture(event.pointerId)
    grabRef.current = {
      pointerId: event.pointerId,
      startHop: source.playheadHop,
      windowHops: zoomWindowHops(source.realSecondsPerHop),
      axisPx: vertical ? rect.height : rect.width,
      lastPx: axisOf(event),
      lastTime: event.timeStamp,
      totalPx: 0,
    }
  }

  const handlePointerMove = (event: PointerEvent<HTMLCanvasElement>) => {
    const grab = grabRef.current
    if (!grab || grab.pointerId !== event.pointerId || !onGrab) return
    const px = axisOf(event)
    const deltaPx = px - grab.lastPx
    if (deltaPx === 0) return
    const elapsedSeconds = (event.timeStamp - grab.lastTime) / 1000
    grab.lastPx = px
    grab.lastTime = event.timeStamp
    grab.totalPx += deltaPx
    onGrab({
      startHop: grab.startHop,
      deltaHops: dragHops(deltaPx, grab.axisPx, grab.windowHops),
      totalHops: dragHops(grab.totalPx, grab.axisPx, grab.windowHops),
      elapsedSeconds,
    })
  }

  const handlePointerEnd = (event: PointerEvent<HTMLCanvasElement>) => {
    if (grabRef.current?.pointerId === event.pointerId) grabRef.current = null
  }

  useEffect(() => {
    const canvas = canvasRef.current
    const context = canvas?.getContext('2d')
    if (!canvas || !context) return
    const styles = getComputedStyle(canvas)
    const colour = (token: string, fallback: string) =>
      styles.getPropertyValue(token).trim() || fallback
    // Each strip's palette is owned by CSS (see .ui-zoomstrip--a / --b),
    // themed via <html data-accent>, so the two decks are styleable
    // independently. Bands tint to the deck's accent: low = the dim shade,
    // mid = the full colour, high = near-white for sparkle.
    const readPalette = () => ({
      low: colour('--wave-low', '#6b3fa8'),
      mid: colour('--wave-mid', '#a855f7'),
      high: colour('--wave-high', '#e8edf2'),
      marks: colour('--wave-beat', '#ff4757'),
      cue: colour('--wave-cue', '#26de81'),
      playhead: colour('--wave-accent', '#ffffff'),
    })
    // Re-read on an accent switch so a live change recolours the strip and
    // it stays matched to its deck; the cheap dataset check keeps the
    // steady state off the style-recalc path.
    let themeKey = document.documentElement.dataset.accent ?? ''
    let palette = readPalette()
    const scratch = {
      low: new Float32Array(MAX_HOPS),
      mid: new Float32Array(MAX_HOPS),
      high: new Float32Array(MAX_HOPS),
    }

    let frame = 0
    const draw = () => {
      frame = requestAnimationFrame(draw)
      const theme = document.documentElement.dataset.accent ?? ''
      if (theme !== themeKey) {
        themeKey = theme
        palette = readPalette()
      }
      context.setTransform(1, 0, 0, 1, 0, 0)
      context.clearRect(0, 0, canvas.width, canvas.height)
      // Vertical strips transpose the same drawing: the horizontal
      // time axis maps downward, amplitude maps across.
      if (vertical) context.setTransform(0, 1, 1, 0, 0, 0)
      const source = getSource()
      if (!source) return
      const hops = zoomWindowHops(source.realSecondsPerHop)
      const fromHop = source.playheadHop - hops / 2
      // The band window quantises to whole hops but the playhead and
      // beat marks do not: draw bands from the floored origin (one
      // extra hop covers the right edge) so the waveform glides in
      // step with the marks instead of stepping a fraction ahead.
      const flooredFrom = Math.floor(fromHop)
      const window = {
        low: scratch.low.subarray(0, hops + 1),
        mid: scratch.mid.subarray(0, hops + 1),
        high: scratch.high.subarray(0, hops + 1),
      }
      source.bands.copyWindow(flooredFrom, window)
      const pxPerHop = WIDTH / hops
      // The loop region (M21) washes behind the bands — the same accent
      // tint the overview uses — so the looped audio reads inside it.
      // Only the visible slice shades; a region wider than the window
      // still fills edge to edge.
      if (source.loop) {
        const left = Math.max(0, (source.loop.startHop - fromHop) * pxPerHop)
        const right = Math.min(WIDTH, (source.loop.endHop - fromHop) * pxPerHop)
        if (right > left) {
          context.fillStyle = palette.playhead
          context.globalAlpha = 0.18
          context.fillRect(left, 0, right - left, HEIGHT)
          context.globalAlpha = 1
        }
      }
      const centre = HEIGHT / 2
      const height = (value: number) =>
        Math.min(1, Math.sqrt(value) * AMPLITUDE_GAIN) * (HEIGHT * 0.94)
      for (let i = 0; i <= hops; i++) {
        const x = (flooredFrom + i - fromHop) * pxPerHop
        const width = Math.max(1, pxPerHop)
        const bands = [
          [window.low[i], palette.low],
          [window.mid[i], palette.mid],
          [window.high[i], palette.high],
        ] as const
        for (const [value, fill] of bands) {
          if (value <= 0) continue
          const h = height(value)
          context.fillStyle = fill
          context.fillRect(x, centre - h / 2, width, h)
        }
      }
      // Beat marks where the deck's clock is confident: full-height
      // lines, every fourth heavier — the lattice the eyes match.
      // The heavy marks count bars within a strip only: each deck's
      // anchor is an arbitrary beat, so two locked decks coincide on
      // the per-beat lattice, not necessarily on the heavy ones.
      if (source.beat) {
        const { periodHops, anchorHop } = source.beat
        context.fillStyle = palette.marks
        const firstIndex = Math.ceil((fromHop - anchorHop) / periodHops)
        for (let k = firstIndex; ; k++) {
          const hop = anchorHop + k * periodHops
          if (hop >= fromHop + hops) break
          const x = (hop - fromHop) * pxPerHop
          const heavy = ((k % 4) + 4) % 4 === 0
          // Grid-red over the band colours; the muted gray that
          // shipped first was invisible on the device.
          context.globalAlpha = heavy ? 1 : 0.7
          context.fillRect(Math.round(x), 0, heavy ? 3 : 2, HEIGHT)
        }
        context.globalAlpha = 1
      }
      // Hot cues (M21): a mint flag tab over a faint full-height stem —
      // the tab names it a cue (never the red beat marks), the stem lets
      // you line a cue up against the centre playhead as you scrub onto it.
      for (const cueHop of source.cues) {
        if (cueHop < fromHop || cueHop >= fromHop + hops) continue
        const x = Math.round((cueHop - fromHop) * pxPerHop)
        context.fillStyle = palette.cue
        context.globalAlpha = 0.5
        context.fillRect(x, 0, 2, HEIGHT)
        context.globalAlpha = 1
        context.fillRect(x, 0, 8, 6)
      }
      // Loop entry/exit (M21): top and bottom caps frame each boundary
      // that's in view, so what the region wraps on is unmistakable —
      // and they never read as the full-height centre playhead. An
      // off-screen edge draws nothing, never a false boundary.
      if (source.loop) {
        const capHeight = 14
        context.fillStyle = palette.playhead
        for (const edgeHop of [source.loop.startHop, source.loop.endHop]) {
          if (edgeHop < fromHop || edgeHop > fromHop + hops) continue
          const x = Math.round((edgeHop - fromHop) * pxPerHop)
          context.fillRect(x, 0, 2, capHeight)
          context.fillRect(x, HEIGHT - capHeight, 2, capHeight)
        }
      }
      // The playhead sits mid-strip; the audio scrolls beneath it.
      context.fillStyle = palette.playhead
      context.fillRect(WIDTH / 2 - 1, 0, 2, HEIGHT)
    }
    frame = requestAnimationFrame(draw)
    return () => cancelAnimationFrame(frame)
  }, [getSource, deck, vertical])

  return (
    <canvas
      ref={canvasRef}
      className={`ui-zoomstrip ui-zoomstrip--${deck}${vertical ? ' ui-zoomstrip--vertical' : ''}${onGrab ? ' ui-zoomstrip--grabbable' : ''}`}
      width={vertical ? HEIGHT : WIDTH}
      height={vertical ? WIDTH : HEIGHT}
      role="img"
      aria-label={label}
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={handlePointerEnd}
      onPointerCancel={handlePointerEnd}
    />
  )
}
