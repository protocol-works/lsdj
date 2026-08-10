import { useId, useRef, type PointerEvent } from 'react'

export type KnobAccent = 'a' | 'b' | 'master'
export type KnobSize = 'm' | 's'

type KnobProps = {
  label: string
  /** Accessible name for the range input when the visible label is a live
   * readout (e.g. the LoRA rack's ×strength) rather than a stable name. */
  ariaLabel?: string
  value: number
  min?: number
  max?: number
  step?: number
  accent?: KnobAccent
  /** Dial size: 'm' is the mixer/FX dial, 's' the compact in-row trim
   * (LoRA rack). Same geometry, scaled. */
  size?: KnobSize
  disabled?: boolean
  /** Where double-click parks the knob; defaults to the range centre
   * (the EQ-flat convention). Effects rest elsewhere (ADR-0008). */
  resetValue?: number
  onChange: (value: number) => void
}

// capInset keeps the pointer inside the cap at both scales (a score mark,
// not a needle).
const GEOMETRY: Record<
  KnobSize,
  { size: number; radius: number; pointer: number; capInset: number }
> = {
  m: { size: 44, radius: 17, pointer: 10, capInset: 5 },
  s: { size: 28, radius: 11, pointer: 6.5, capInset: 3.5 },
}
const SWEEP_DEGREES = 270
// A knob is much smaller than a useful slider track (the compact LoRA trim is
// only 28 px wide). Map a full-range vertical drag to a stable distance so the
// pointer interaction stays precise regardless of the rendered dial size.
const DRAG_RANGE_PIXELS = 160
// Min points to 7 o'clock and the dial sweeps clockwise over the top to
// 5 o'clock, leaving the conventional gap at the bottom. Angles are
// y-up math convention, so clockwise on screen = decreasing angle.
const START_DEGREES = -135

type Drag = {
  pointerId: number
  startY: number
  startValue: number
  lastValue: number
  captured: boolean
}

function angleFor(fraction: number) {
  return START_DEGREES - SWEEP_DEGREES * fraction
}

function polar(size: KnobSize, angleDegrees: number, radius: number) {
  const radians = (angleDegrees * Math.PI) / 180
  const centre = GEOMETRY[size].size / 2
  return {
    x: centre + radius * Math.cos(radians),
    y: centre - radius * Math.sin(radians),
  }
}

function arcPath(size: KnobSize, fromDegrees: number, toDegrees: number) {
  const radius = GEOMETRY[size].radius
  const start = polar(size, fromDegrees, radius)
  const end = polar(size, toDegrees, radius)
  const largeArc = Math.abs(fromDegrees - toDegrees) > 180 ? 1 : 0
  return `M ${start.x} ${start.y} A ${radius} ${radius} 0 ${largeArc} 1 ${end.x} ${end.y}`
}

function snapToStep(value: number, min: number, max: number, step: number) {
  const clamped = Math.min(max, Math.max(min, value))
  if (!Number.isFinite(step) || step <= 0) return clamped
  const snapped = min + Math.round((clamped - min) / step) * step
  // Multiplication by a decimal step can expose floating-point tails to the
  // controlled input (for example 0.30000000000000004).
  return Math.min(max, Math.max(min, Number(snapped.toPrecision(12))))
}

/** Rotary control: an SVG arc dial over a real (invisible) range input, so
 * keyboard, labels, and test tooling keep native input semantics.
 * Double-click resets to `resetValue` (range centre by default). */
export function Knob({
  label,
  ariaLabel,
  value,
  min = 0,
  max = 1,
  step = 0.01,
  accent = 'master',
  size = 'm',
  disabled,
  resetValue,
  onChange,
}: KnobProps) {
  const id = useId()
  const inputRef = useRef<HTMLInputElement>(null)
  const dragRef = useRef<Drag | null>(null)
  const { size: box, radius, pointer: pointerRadius, capInset } = GEOMETRY[size]
  const fraction = max === min ? 0 : (value - min) / (max - min)
  const valueAngle = angleFor(fraction)
  const pointer = polar(size, valueAngle, pointerRadius)

  function handlePointerDown(event: PointerEvent<HTMLDivElement>) {
    if (disabled || (event.pointerType === 'mouse' && event.button !== 0)) return
    event.preventDefault()
    inputRef.current?.focus()
    dragRef.current = {
      pointerId: event.pointerId,
      startY: event.clientY,
      startValue: value,
      lastValue: value,
      captured: false,
    }
  }

  function handlePointerMove(event: PointerEvent<HTMLDivElement>) {
    const drag = dragRef.current
    if (disabled || !drag || event.pointerId !== drag.pointerId) return
    event.preventDefault()
    if (!drag.captured) {
      // jsdom has no pointer capture; browsers use it to keep the drag alive
      // after the pointer leaves the small dial.
      event.currentTarget.setPointerCapture?.(event.pointerId)
      drag.captured = true
    }
    const range = max - min
    const next = snapToStep(
      drag.startValue + ((drag.startY - event.clientY) / DRAG_RANGE_PIXELS) * range,
      min,
      max,
      step,
    )
    if (next === drag.lastValue) return
    drag.lastValue = next
    onChange(next)
  }

  function handlePointerEnd(event: PointerEvent<HTMLDivElement>) {
    if (event.pointerId === dragRef.current?.pointerId) dragRef.current = null
  }

  function handleDoubleClick() {
    if (!disabled) onChange(resetValue ?? (min + max) / 2)
  }

  return (
    <div
      className={`ui-knob ui-knob--${accent}${size === 's' ? ' ui-knob--s' : ''}${
        disabled ? ' ui-knob--disabled' : ''
      }`}
    >
      <div
        className="ui-knob__dial"
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerEnd}
        onPointerCancel={handlePointerEnd}
        onDoubleClick={handleDoubleClick}
      >
        <svg viewBox={`0 0 ${box} ${box}`} aria-hidden="true">
          <path className="ui-knob__track" d={arcPath(size, START_DEGREES, angleFor(1))} />
          <path className="ui-knob__value" d={arcPath(size, START_DEGREES, valueAngle)} />
          <circle className="ui-knob__cap" cx={box / 2} cy={box / 2} r={radius - capInset} />
          <line
            className="ui-knob__pointer"
            x1={box / 2}
            y1={box / 2}
            x2={pointer.x}
            y2={pointer.y}
          />
        </svg>
        <input
          ref={inputRef}
          className="ui-knob__input"
          id={id}
          type="range"
          aria-label={ariaLabel}
          min={min}
          max={max}
          step={step}
          value={value}
          disabled={disabled}
          onChange={(event) => onChange(Number(event.target.value))}
        />
      </div>
      <label className="ui-knob__label" htmlFor={id}>
        {label}
      </label>
    </div>
  )
}
