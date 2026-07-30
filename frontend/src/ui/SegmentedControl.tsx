import { useRef } from 'react'
import type { KeyboardEvent } from 'react'

export type SegmentedOption<T extends string> = { value: T; label: string }

/** A compact, keyboard-complete single-choice control for dense instrument panels. */
export function SegmentedControl<T extends string>({
  label,
  value,
  options,
  onChange,
  disabled = false,
}: {
  label: string
  value: T
  options: SegmentedOption<T>[]
  onChange: (value: T) => void
  disabled?: boolean
}) {
  const buttons = useRef<(HTMLButtonElement | null)[]>([])

  const move = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    let next: number | null = null
    if (event.key === 'ArrowRight' || event.key === 'ArrowDown') {
      next = (index + 1) % options.length
    } else if (event.key === 'ArrowLeft' || event.key === 'ArrowUp') {
      next = (index - 1 + options.length) % options.length
    } else if (event.key === 'Home') {
      next = 0
    } else if (event.key === 'End') {
      next = options.length - 1
    }
    if (next == null) return
    event.preventDefault()
    onChange(options[next].value)
    buttons.current[next]?.focus()
  }

  return (
    <div className="ui-segmented-field">
      <span className="ui-segmented-field__label">{label}</span>
      <div className="ui-segmented" role="radiogroup" aria-label={label}>
        {options.map((option, index) => {
          const selected = option.value === value
          return (
            <button
              key={option.value}
              ref={(node) => {
                buttons.current[index] = node
              }}
              type="button"
              role="radio"
              aria-checked={selected}
              tabIndex={selected && !disabled ? 0 : -1}
              disabled={disabled}
              className={`ui-segmented__option${selected ? ' ui-segmented__option--selected' : ''}`}
              onClick={() => onChange(option.value)}
              onKeyDown={(event) => move(event, index)}
            >
              {option.label}
            </button>
          )
        })}
      </div>
    </div>
  )
}
