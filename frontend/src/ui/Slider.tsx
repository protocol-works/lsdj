import { useId } from 'react'

/** A reset (↺) control in the label row is opt-in, but if present it MUST carry
 * an accessible name — so `onReset` and `resetLabel` travel together or not at
 * all (an unlabelled icon button is a11y-invalid). */
type SliderResetProps =
  | { onReset: () => void; resetLabel: string }
  | { onReset?: never; resetLabel?: never }

type SliderProps = {
  label: string
  help?: { label: string; text: string }
  min: number
  max: number
  step: number
  value: number
  disabled?: boolean
  'data-shortcut'?: string
  onChange: (value: number) => void
} & SliderResetProps

export function Slider({
  label,
  help,
  min,
  max,
  step,
  value,
  disabled,
  'data-shortcut': dataShortcut,
  onChange,
  onReset,
  resetLabel,
}: SliderProps) {
  const id = useId()
  const helpId = useId()
  return (
    <div className={`ui-slider${disabled ? ' ui-slider--disabled' : ''}`}>
      <div className="ui-slider__head">
        <div className="ui-slider__label-group">
          <label className="ui-slider__label" htmlFor={id}>
            {label}
          </label>
          {help ? (
            <span className="ui-slider__help">
              <button
                type="button"
                className="ui-slider__help-trigger"
                aria-label={help.label}
                aria-describedby={helpId}
              >
                ?
              </button>
              <span id={helpId} className="ui-slider__tooltip" role="tooltip">
                {help.text}
              </span>
            </span>
          ) : null}
        </div>
        {onReset && (
          <button
            type="button"
            className="ui-slider__reset"
            onClick={onReset}
            disabled={disabled}
            aria-label={resetLabel}
            title={resetLabel}
          >
            ↺
          </button>
        )}
      </div>
      <input
        className="ui-slider__input"
        id={id}
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        disabled={disabled}
        data-shortcut={dataShortcut}
        onChange={(event) => onChange(Number(event.target.value))}
      />
    </div>
  )
}
