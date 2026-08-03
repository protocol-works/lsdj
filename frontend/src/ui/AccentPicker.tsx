import type { AccentTheme } from '../persistence'

type AccentPickerProps = {
  label: string
  value: AccentTheme
  options: { value: AccentTheme; label: string }[]
  onChange: (value: AccentTheme) => void
}

/** Accent chooser as fixed-colour swatches (LSDJ): each square shows its
 * own accent hue (via .ui-swatch--<accent>) regardless of the active theme,
 * so all three read at a glance. A single-select toggle-button group. */
export function AccentPicker({ label, value, options, onChange }: AccentPickerProps) {
  return (
    <div className="ui-picker">
      <span className="ui-field__label">{label}</span>
      <div className="ui-swatches" role="group" aria-label={label}>
        {options.map((option) => (
          <button
            key={option.value}
            type="button"
            aria-pressed={option.value === value}
            aria-label={option.label}
            title={option.label}
            className={`ui-swatch ui-swatch--${option.value}`}
            onClick={() => onChange(option.value)}
          />
        ))}
      </div>
    </div>
  )
}
