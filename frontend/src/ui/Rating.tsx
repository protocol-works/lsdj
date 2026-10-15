import { useId } from 'react'

const STARS = [1, 2, 3, 4, 5]

export type RatingProps = {
  /** 0–5; 0 = unrated. */
  value: number
  onChange: (value: number) => void
  /** Accessible name of the group, e.g. "Rating for Dub #3". */
  label: string
  /** Per-star accessible name, e.g. "3 stars". */
  starLabel: (count: number) => string
  /** Group tooltip, e.g. "Click the current star to clear". */
  clearHint?: string
}

/** A 0–5 star rating on five visually-hidden native radios, so Tab-into-group
 * and arrow-key selection come from the browser. The only JS: the controlled
 * `checked`, and a click on the already-checked star emitting 0 (a native radio
 * can't un-check). The hover preview is pure CSS. */
export function Rating({ value, onChange, label, starLabel, clearHint }: RatingProps) {
  const name = useId()
  return (
    <div className="ui-rating" role="radiogroup" aria-label={label} title={clearHint}>
      {STARS.map((count) => {
        const id = `${name}-${count}`
        return [
          <input
            key={`input-${count}`}
            id={id}
            className="ui-rating__input"
            type="radio"
            name={name}
            value={count}
            checked={value === count}
            aria-label={starLabel(count)}
            onChange={() => onChange(count)}
            // No change event fires on the checked radio, so a click there clears.
            onClick={() => {
              if (value === count) onChange(0)
            }}
          />,
          <label
            key={`label-${count}`}
            htmlFor={id}
            className={`ui-rating__star${count <= value ? ' ui-rating__star--on' : ''}`}
            aria-hidden="true"
          >
            ★
          </label>,
        ]
      })}
    </div>
  )
}
