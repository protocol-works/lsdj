import type { ComponentProps } from 'react'

// ComponentProps (not ButtonHTMLAttributes) so callers can pass a `ref` — React 19
// forwards it as an ordinary prop (e.g. an ActionMenu trigger anchoring its panel).
type ButtonProps = Omit<ComponentProps<'button'>, 'className'> & {
  variant?: 'primary' | 'default'
  /** LED-style active state for toggle buttons (e.g. channel CUE). */
  lit?: boolean
}

export function Button({ variant = 'default', lit = false, ...props }: ButtonProps) {
  const variantClass = variant === 'primary' ? ' ui-button--primary' : ''
  const litClass = lit ? ' ui-button--lit' : ''
  return <button className={`ui-button${variantClass}${litClass}`} {...props} />
}
