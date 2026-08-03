import type { ReactNode } from 'react'

import type { BeatViewLayout } from '../persistence'

/** Layout glyphs (24×24, currentColor): center = two stacked horizontal
 * strips, vertical = two side-by-side strips, top = one full-width bar,
 * off = a hollow square struck through. */
const ICONS: Record<BeatViewLayout, ReactNode> = {
  center: (
    <>
      <rect x="4" y="5" width="16" height="6" />
      <rect x="4" y="13" width="16" height="6" />
    </>
  ),
  vertical: (
    <>
      <rect x="5" y="4" width="6" height="16" />
      <rect x="13" y="4" width="6" height="16" />
    </>
  ),
  top: <rect x="4" y="4" width="16" height="6" />,
  off: (
    <>
      <rect
        x="4"
        y="4"
        width="16"
        height="16"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
      />
      <line
        x1="5"
        y1="19"
        x2="19"
        y2="5"
        stroke="currentColor"
        strokeWidth="2"
      />
    </>
  ),
}

type BeatViewPickerProps = {
  label: string
  value: BeatViewLayout
  options: { value: BeatViewLayout; label: string }[]
  onChange: (value: BeatViewLayout) => void
}

/** Beat-view layout chooser as an icon toggle group (LSDJ): replaces the
 * dropdown with a glyph per layout, the active one lit in the accent. */
export function BeatViewPicker({
  label,
  value,
  options,
  onChange,
}: BeatViewPickerProps) {
  return (
    <div className="ui-picker">
      <span className="ui-field__label">{label}</span>
      <div className="ui-iconpicker" role="group" aria-label={label}>
        {options.map((option) => (
          <button
            key={option.value}
            type="button"
            aria-pressed={option.value === value}
            aria-label={option.label}
            title={option.label}
            className={`ui-iconpicker__btn${option.value === value ? ' ui-iconpicker__btn--active' : ''}`}
            onClick={() => onChange(option.value)}
          >
            <svg className="ui-iconpicker__icon" viewBox="0 0 24 24" aria-hidden="true">
              {ICONS[option.value]}
            </svg>
          </button>
        ))}
      </div>
    </div>
  )
}
