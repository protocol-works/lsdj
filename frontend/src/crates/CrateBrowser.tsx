import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'

import type { DeckId } from '../audio/types'
import { useControlBus } from '../control/busContext'
import { parsePresetsExport, serialisePresets, type StylePreset } from '../presets'
import { matchesSearch } from '../search'
import { Button } from '../ui/Button'
import { Panel } from '../ui/Panel'
import './crates.css'

type CrateBrowserProps = {
  presets: StylePreset[]
  onLoad: (deck: DeckId, preset: StylePreset) => void
  onDelete: (name: string) => void
  onImport: (presets: StylePreset[]) => void
  filter?: string
}

function downloadCrates(presets: StylePreset[]) {
  const url = URL.createObjectURL(
    new Blob([serialisePresets(presets)], { type: 'application/json' }),
  )
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = 'lsdj-crates.json'
  anchor.click()
  setTimeout(() => URL.revokeObjectURL(url), 0)
}

/** The crate browser (M16): saved style presets, loadable onto either
 * deck. The FLX4 browse rotary moves the highlight and the LOAD
 * buttons load it (intents on the ControlBus); mouse does the same
 * per row. */
export function CrateBrowser({
  presets,
  onLoad,
  onDelete,
  onImport,
  filter = '',
}: CrateBrowserProps) {
  const { t } = useTranslation()
  const [index, setIndex] = useState(0)
  const [importError, setImportError] = useState<string | null>(null)
  const fileInput = useRef<HTMLInputElement>(null)
  const highlightedRow = useRef<HTMLLIElement>(null)
  // The stored index may point past the end after a delete; the
  // clamped value is the single truth everywhere below.
  const filteredPresets = presets.filter((preset) =>
    matchesSearch(
      filter,
      preset.name,
      ...preset.targets.map((target) => target.text),
      preset.fx.kind,
    ),
  )
  const highlighted =
    filteredPresets.length === 0 ? -1 : Math.min(index, filteredPresets.length - 1)

  // The list scrolls past ~8 presets; the rotary's highlight must stay
  // visible. (Optional call: jsdom has no scrollIntoView.)
  useLayoutEffect(() => {
    highlightedRow.current?.scrollIntoView?.({ block: 'nearest' })
  }, [highlighted, filter])

  // Hardware intents (M16): rotary turn = highlight, LOAD = load the
  // highlighted preset. Resubscribes per render to read fresh state;
  // the functional update keeps back-to-back ticks lossless.
  const bus = useControlBus()
  useEffect(() =>
    bus.subscribe((intent) => {
      if (intent.kind === 'browse_scroll') {
        if (filteredPresets.length === 0) return
        setIndex((current) => {
          const from = Math.min(current, filteredPresets.length - 1)
          return Math.max(0, Math.min(filteredPresets.length - 1, from + intent.steps))
        })
      } else if (intent.kind === 'browse_load') {
        const preset = filteredPresets[highlighted]
        if (preset) onLoad(intent.deck, preset)
      }
    }),
  )

  async function importFile(file: File) {
    setImportError(null)
    try {
      onImport(parsePresetsExport(await file.text()))
    } catch (error) {
      setImportError(error instanceof Error ? error.message : String(error))
    }
  }

  return (
    <Panel className="crates" aria-label={t('crates.title')}>
      <h2 className="crates__title">{t('crates.title')}</h2>
      {presets.length === 0 ? (
        <p className="crates__empty">{t('crates.empty')}</p>
      ) : filteredPresets.length === 0 ? (
        <p className="crates__empty" role="status">
          {t('media.search.noResults', { query: filter.trim() })}
        </p>
      ) : (
        <ul className="crates__list">
          {filteredPresets.map((preset, presetIndex) => (
            <li
              key={preset.name}
              ref={presetIndex === highlighted ? highlightedRow : null}
              className={`crates__item${
                presetIndex === highlighted ? ' crates__item--highlighted' : ''
              }`}
            >
              <button
                className="crates__name"
                aria-label={t('crates.highlight', { name: preset.name })}
                aria-current={presetIndex === highlighted}
                onClick={() => setIndex(presetIndex)}
              >
                {preset.name}
              </button>
              {(['a', 'b'] as const).map((deck) => (
                <Button
                  key={deck}
                  aria-label={t('crates.loadTo', {
                    name: preset.name,
                    deck: deck.toUpperCase(),
                  })}
                  onClick={() => onLoad(deck, preset)}
                >
                  {t('crates.loadShort', { deck: deck.toUpperCase() })}
                </Button>
              ))}
              <Button
                aria-label={t('crates.delete', { name: preset.name })}
                onClick={() => onDelete(preset.name)}
              >
                ✕
              </Button>
            </li>
          ))}
        </ul>
      )}
      <div className="crates__io">
        <Button onClick={() => downloadCrates(presets)} disabled={presets.length === 0}>
          {t('crates.export')}
        </Button>
        <Button onClick={() => fileInput.current?.click()}>
          {t('crates.import')}
        </Button>
        <input
          ref={fileInput}
          className="crates__file"
          type="file"
          accept="application/json,.json"
          aria-label={t('crates.importFile')}
          onChange={(event) => {
            const file = event.target.files?.[0]
            if (file) void importFile(file)
            event.target.value = ''
          }}
        />
      </div>
      {importError && (
        <p className="crates__error" role="alert">
          {t('crates.importFailed', { message: importError })}
        </p>
      )}
    </Panel>
  )
}
