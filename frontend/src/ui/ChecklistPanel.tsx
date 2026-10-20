import { useEffect, useRef, useState } from 'react'
import type { RefObject } from 'react'

import { AnchoredPanel } from './AnchoredPanel'
import { TextField } from './TextField'

export type ChecklistOption = { value: string; label: string; checked: boolean }

export type ChecklistPanelProps = {
  open: boolean
  anchorRef: RefObject<HTMLElement | null>
  onClose: () => void
  label: string
  options: ChecklistOption[]
  onToggle: (value: string) => void
  /** Optional add-a-new-option field; commits the trimmed text on Enter. */
  add?: { label: string; placeholder?: string; onAdd: (text: string) => void }
  /** Shown when `options` is empty. */
  emptyText?: string
  /** 'add' focuses the add field on open even when there are options. */
  initialFocus?: 'add'
}

/** An AnchoredPanel holding a labelled list of native checkboxes, plus an
 * optional add field that stays open after each add (tags, playlists). */
export function ChecklistPanel({
  open,
  anchorRef,
  onClose,
  label,
  options,
  onToggle,
  add,
  emptyText,
  initialFocus,
}: ChecklistPanelProps) {
  const bodyRef = useRef<HTMLDivElement>(null)
  const [draft, setDraft] = useState('')

  // The first input is the first checkbox, or the add field when there are no
  // options; the add field is always the last. Passive, so it lands after
  // AnchoredPanel focuses the panel itself.
  useEffect(() => {
    if (!open) return
    const inputs = bodyRef.current?.querySelectorAll('input')
    inputs?.[initialFocus === 'add' ? inputs.length - 1 : 0]?.focus()
  }, [open, initialFocus])

  return (
    <AnchoredPanel
      open={open}
      anchorRef={anchorRef}
      onClose={onClose}
      label={label}
      className="ui-checklist-panel"
    >
      <div ref={bodyRef} className="ui-checklist-panel__body">
        {options.length > 0 ? (
          <ul className="ui-checklist-panel__list" aria-label={label}>
            {options.map((option) => (
              <li key={option.value}>
                <label className="ui-checklist-panel__option">
                  <input
                    type="checkbox"
                    checked={option.checked}
                    onChange={() => onToggle(option.value)}
                  />
                  {option.label}
                </label>
              </li>
            ))}
          </ul>
        ) : (
          emptyText && <p className="ui-checklist-panel__empty">{emptyText}</p>
        )}
        {add && (
          <TextField
            label={add.label}
            labelHidden
            placeholder={add.placeholder}
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={(event) => {
              if (event.key !== 'Enter') return
              event.preventDefault()
              const text = draft.trim()
              if (text) add.onAdd(text)
              setDraft('')
            }}
          />
        )}
      </div>
    </AnchoredPanel>
  )
}
