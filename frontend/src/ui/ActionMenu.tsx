import { useEffect, useRef, useState } from 'react'
import type { KeyboardEvent, ReactNode } from 'react'

import { AnchoredPanel } from './AnchoredPanel'
import { Button } from './Button'

export type ActionMenuItem = {
  label: string
  onSelect: () => void
  disabled?: boolean
  danger?: boolean
}

export type ActionMenuProps = {
  /** Trigger aria-label (also names the menu). */
  label: string
  items: ActionMenuItem[]
  /** Trigger glyph; defaults to ⋯. */
  children?: ReactNode
}

const enabledItems = (menu: HTMLElement | null) =>
  Array.from(menu?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not(:disabled)') ?? [])

/** An overflow menu: a trigger Button opening an AnchoredPanel with a vertical
 * `role="menu"`. Arrow keys move (wrapping, disabled items skipped), Home/End
 * jump, Enter/Space select. Escape, outside click and focus return to the
 * trigger are AnchoredPanel's. No nested submenus. */
export function ActionMenu({ label, items, children = '⋯' }: ActionMenuProps) {
  const triggerRef = useRef<HTMLButtonElement>(null)
  const menuRef = useRef<HTMLDivElement>(null)
  const [open, setOpen] = useState(false)

  // A passive effect, so it lands after AnchoredPanel's layout effect focuses the panel.
  useEffect(() => {
    if (open) enabledItems(menuRef.current)[0]?.focus()
  }, [open])

  const select = (item: ActionMenuItem) => {
    setOpen(false)
    item.onSelect()
  }

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const enabled = enabledItems(menuRef.current)
    if (enabled.length === 0) return
    const index = enabled.indexOf(document.activeElement as HTMLButtonElement)
    let next: number | null = null
    if (event.key === 'ArrowDown') next = (index + 1) % enabled.length
    else if (event.key === 'ArrowUp') next = index <= 0 ? enabled.length - 1 : index - 1
    else if (event.key === 'Home') next = 0
    else if (event.key === 'End') next = enabled.length - 1
    if (next == null) return
    event.preventDefault()
    enabled[next].focus()
  }

  return (
    <>
      <Button
        ref={triggerRef}
        type="button"
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((current) => !current)}
      >
        {children}
      </Button>
      <AnchoredPanel
        open={open}
        anchorRef={triggerRef}
        onClose={() => setOpen(false)}
        label={label}
        className="ui-action-menu"
      >
        <div ref={menuRef} role="menu" aria-label={label} onKeyDown={onKeyDown}>
          {items.map((item) => (
            <button
              key={item.label}
              type="button"
              role="menuitem"
              tabIndex={-1}
              disabled={item.disabled}
              className={`ui-action-menu__item${item.danger ? ' ui-action-menu__item--danger' : ''}`}
              onClick={() => select(item)}
              onKeyDown={(event) => {
                if (event.key !== 'Enter' && event.key !== ' ') return
                // Handled here so the native button click doesn't select twice.
                event.preventDefault()
                select(item)
              }}
            >
              {item.label}
            </button>
          ))}
        </div>
      </AnchoredPanel>
    </>
  )
}
