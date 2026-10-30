import { useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { ActionMenu } from '../ui/ActionMenu'
import { AnchoredPanel } from '../ui/AnchoredPanel'
import { Button } from '../ui/Button'
import { TextField } from '../ui/TextField'
import type { LoomSummary } from './loomLibrary'
import './loom.css'

export type LoomBarProps = {
  name: string
  currentId: string
  saved: 'saved' | 'saving'
  /** A refusal or failure line; the owner clears it. */
  status: string | null
  /** Every saved loom, newest first. */
  looms: LoomSummary[]
  onRename: (name: string) => void
  onNew: () => void
  onOpen: (id: string) => void
  onDuplicate: () => void
  onDelete: () => void
}

/** The loom chrome in the top bar (plan decision 13): the name (commits on
 * blur or Enter), a quiet saved mark, the loom menu, and the status line. The
 * loom saves itself, so there is no save button. */
export function LoomBar({
  name,
  currentId,
  saved,
  status,
  looms,
  onRename,
  onNew,
  onOpen,
  onDuplicate,
  onDelete,
}: LoomBarProps) {
  const { t } = useTranslation()
  const rootRef = useRef<HTMLDivElement>(null)
  const [draft, setDraft] = useState(name)
  const [seededName, setSeededName] = useState(name)
  const [panel, setPanel] = useState<'open' | 'delete' | null>(null)
  // Re-seed the field when the loom changes under it (open, new, rename).
  if (name !== seededName) {
    setSeededName(name)
    setDraft(name)
  }

  const commit = () => {
    if (draft.trim() && draft.trim() !== name) onRename(draft)
    else setDraft(name)
  }
  const others = looms.filter((loom) => loom.id !== currentId)

  return (
    <div ref={rootRef} className="loom-bar">
      <div className="loom-bar__name">
        <TextField
          label={t('loom.bar.name')}
          labelHidden
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          onBlur={commit}
          onKeyDown={(event) => {
            if (event.key === 'Escape') setDraft(name)
            if (event.key === 'Enter' || event.key === 'Escape') event.currentTarget.blur()
          }}
        />
      </div>
      <span className="loom-bar__saved">{t(saved === 'saved' ? 'loom.bar.saved' : 'loom.bar.saving')}</span>
      <ActionMenu
        label={t('loom.bar.menu')}
        items={[
          { label: t('loom.bar.new'), onSelect: onNew },
          { label: t('loom.bar.open'), onSelect: () => setPanel('open'), disabled: others.length === 0 },
          { label: t('loom.bar.duplicate'), onSelect: onDuplicate },
          { label: t('loom.bar.delete'), onSelect: () => setPanel('delete'), danger: true },
        ]}
      />
      <span className="loom-bar__status" role="status">
        {status}
      </span>
      <AnchoredPanel
        open={panel === 'open'}
        anchorRef={rootRef}
        onClose={() => setPanel(null)}
        label={t('loom.bar.openLabel')}
        className="ui-action-menu"
      >
        <div role="menu" aria-label={t('loom.bar.openLabel')}>
          {others.map((loom) => (
            <button
              key={loom.id}
              type="button"
              role="menuitem"
              className="ui-action-menu__item"
              onClick={() => {
                setPanel(null)
                onOpen(loom.id)
              }}
            >
              {t('loom.bar.openItem', { name: loom.name, count: loom.nodes })}
            </button>
          ))}
        </div>
      </AnchoredPanel>
      <AnchoredPanel
        open={panel === 'delete'}
        anchorRef={rootRef}
        onClose={() => setPanel(null)}
        label={t('loom.bar.delete')}
        className="loom-bar__confirm"
      >
        <p>{t('loom.bar.confirmDelete', { name })}</p>
        <div className="loom-bar__confirm-actions">
          <Button
            variant="primary"
            onClick={() => {
              setPanel(null)
              onDelete()
            }}
          >
            {t('loom.bar.confirm')}
          </Button>
          <Button onClick={() => setPanel(null)}>{t('loom.bar.cancel')}</Button>
        </div>
      </AnchoredPanel>
    </div>
  )
}
