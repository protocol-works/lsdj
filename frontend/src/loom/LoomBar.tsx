import { useEffect, useRef, useState } from 'react'
import { Trans, useTranslation } from 'react-i18next'

import { ActionMenu } from '../ui/ActionMenu'
import { AnchoredPanel } from '../ui/AnchoredPanel'
import { Button } from '../ui/Button'
import { TextField } from '../ui/TextField'
import type { LoomSummary } from './loomLibrary'
import './loom.css'

export type LoomBarProps = {
  name: string
  /** When the loom was made (epoch ms), shown after its name. */
  createdAt: number
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
  explorerOpen: boolean
  onToggleExplorer: () => void
  /** Receives the bar's slot for the view's zoom and new-thread buttons. */
  toolsRef: (element: HTMLElement | null) => void
  /** The clock line: a deck is playing (the clock starts at the first play
   * and counts on through pauses), the master tempo, and the ready and
   * rendering node counts. */
  playing: boolean
  bpm: number
  threads: number
  weaving: number
}

const twoDigits = (value: number) => String(value).padStart(2, '0')

/** The loom chrome in the top bar (plan decision 13, the mockup's slim
 * line): the name (commits on blur or Enter) and its date, a quiet saved
 * mark, the loom menu, the explorer toggle, the view's tools, the status and
 * the clock line. The loom saves itself, so there is no save button. */
export function LoomBar({
  name,
  createdAt,
  currentId,
  saved,
  status,
  looms,
  onRename,
  onNew,
  onOpen,
  onDuplicate,
  onDelete,
  explorerOpen,
  onToggleExplorer,
  toolsRef,
  playing,
  bpm,
  threads,
  weaving,
}: LoomBarProps) {
  const { t, i18n } = useTranslation()
  const rootRef = useRef<HTMLDivElement>(null)
  const [draft, setDraft] = useState(name)
  const [seededName, setSeededName] = useState(name)
  const [panel, setPanel] = useState<'open' | 'delete' | null>(null)
  // Re-seed the field when the loom changes under it (open, new, rename).
  if (name !== seededName) {
    setSeededName(name)
    setDraft(name)
  }

  // The set clock: from the first play, ticking once a second.
  const startedAt = useRef<number | null>(null)
  const [elapsed, setElapsed] = useState(0)
  useEffect(() => {
    if (!playing && startedAt.current === null) return
    startedAt.current ??= Date.now()
    const timer = setInterval(
      () => setElapsed(Math.floor((Date.now() - (startedAt.current ?? Date.now())) / 1000)),
      1000,
    )
    return () => clearInterval(timer)
  }, [playing])

  const commit = () => {
    if (draft.trim() && draft.trim() !== name) onRename(draft)
    else setDraft(name)
  }
  const others = looms.filter((loom) => loom.id !== currentId)
  const created = new Date(createdAt)

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
        <span className="loom-bar__created">
          {t('loom.bar.created', {
            day: created.getDate(),
            month: created.toLocaleDateString(i18n.language, { month: 'short' }),
          })}
        </span>
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
      <Button aria-pressed={explorerOpen} onClick={onToggleExplorer}>
        {t('loom.bar.explorer')}
      </Button>
      <span ref={toolsRef} className="loom-bar__tools" />
      <span className="loom-bar__status" role="status">
        {status}
      </span>
      <span className="loom-bar__clock">
        <Trans
          i18nKey="loom.bar.clock"
          count={threads}
          values={{ time: `${twoDigits(Math.floor(elapsed / 60))}:${twoDigits(elapsed % 60)}`, bpm: bpm.toFixed(1), weaving }}
          components={{ b: <b /> }}
        />
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
