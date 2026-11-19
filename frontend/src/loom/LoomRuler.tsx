import { useRef, type PointerEvent } from 'react'
import { useTranslation } from 'react-i18next'

import { LAYOUT } from './layout'

// Pixels of vertical drag per e-fold of zoom: the mockup's feel.
const ZOOM_DRAG_PX = 90

/** Bar numbers along the top of the canvas, at a density that fits the zoom.
 * Drag it like a DAW ruler: up/down zooms around the grab point, sideways
 * scrolls. */
export function LoomRuler({
  width,
  pxPerBar,
  onDrag,
}: {
  width: number
  pxPerBar: number
  /** `px` is the wanted zoom, `anchorX` the grab's client x, `scrollBy` the
   * horizontal scroll since the last move. */
  onDrag: (px: number, anchorX: number, scrollBy: number) => void
}) {
  const { t } = useTranslation()
  const drag = useRef<{ x: number; lastX: number; y: number; px: number } | null>(null)
  const step = pxPerBar >= 32 ? 4 : pxPerBar >= 12 ? 8 : 16
  const marks: number[] = []
  for (let bar = 0; LAYOUT.padX + bar * pxPerBar < width; bar += step) marks.push(bar)

  const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    event.stopPropagation()
    event.currentTarget.setPointerCapture?.(event.pointerId)
    drag.current = { x: event.clientX, lastX: event.clientX, y: event.clientY, px: pxPerBar }
  }
  const onPointerMove = (event: PointerEvent<HTMLDivElement>) => {
    const current = drag.current
    if (!current) return
    onDrag(current.px * Math.exp((current.y - event.clientY) / ZOOM_DRAG_PX), current.x, current.lastX - event.clientX)
    current.lastX = event.clientX
  }
  const onPointerUp = () => {
    drag.current = null
  }

  return (
    <div
      className="loom__ruler"
      style={{ width }}
      title={t('loom.ruler')}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerUp}
    >
      {marks.map((bar) => (
        <span key={bar} className="loom__ruler-mark" style={{ left: LAYOUT.padX + bar * pxPerBar }}>
          {bar}
        </span>
      ))}
    </div>
  )
}
