import { useId, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'

import {
  invoke,
  type LoraAdapter,
  type LoraBase,
} from '../audio/nativeEngine'
import { formatBytes } from '../models/formatBytes'
import { useLoraLibrary } from '../models/loraContext'
import {
  KIND_BASES,
  MAX_LORA_STACK,
  type LoraChoice,
} from '../models/useLoras'
import { AnchoredPanel } from './AnchoredPanel'
import { Button } from './Button'
import { Knob, type KnobAccent } from './Knob'
import { Select } from './Select'
import { TextField } from './TextField'

const STRENGTH_MIN = 0
const STRENGTH_MAX = 2
const STRENGTH_STEP = 0.25
const STRENGTH_REST = 1

export type LoraGenerationKind = 'sfx' | 'music' | 'track' | 'magenta'

/** Accept a bare HuggingFace repo id or the common pasted URL shapes. The Rust
 * importer repeats validation at the trust boundary. */
function normalizeHfRepo(input: string): string {
  const rest = input
    .trim()
    .replace(/^https?:\/\//, '')
    .replace(/^(www\.)?(huggingface\.co|hf\.co)\//, '')
    .split(/[?#]/)[0]
  const segments = rest.split('/').filter(Boolean)
  return segments.length >= 2 ? `${segments[0]}/${segments[1]}` : input.trim()
}

function strengthText(value: number): string {
  return Number.isInteger(value) ? value.toFixed(0) : String(value)
}

/** One stable-height generation control. Its portalled panel progressively
 * reveals applied/available/incompatible adapters and lifecycle management. */
export function LoraControl({
  adapters,
  kind,
  value,
  onToggle,
  onStrength,
  onToggleBypass,
  accent = 'master',
  max = MAX_LORA_STACK,
}: {
  adapters: LoraAdapter[]
  kind: LoraGenerationKind
  value: LoraChoice[]
  onToggle: (name: string) => void
  onStrength: (name: string, strength: number) => void
  onToggleBypass: (name: string) => void
  accent?: KnobAccent
  max?: number
}) {
  const { t } = useTranslation()
  const library = useLoraLibrary()
  const triggerRef = useRef<HTMLButtonElement>(null)
  const appliedHeadingId = useId()
  const availableHeadingId = useId()
  const [open, setOpen] = useState(false)
  const [repo, setRepo] = useState('')
  const [base, setBase] = useState<'auto' | LoraBase>('auto')

  const expectedBase = kind === 'magenta' ? null : KIND_BASES[kind]
  const choices = useMemo(
    () => new Map(value.map((choice) => [choice.name, choice])),
    [value],
  )
  const compatible = useMemo(
    () =>
      expectedBase === null
        ? []
        : adapters.filter((adapter) => adapter.base === expectedBase),
    [adapters, expectedBase],
  )
  const incompatible = useMemo(
    () =>
      expectedBase === null
        ? adapters
        : adapters.filter((adapter) => adapter.base !== expectedBase),
    [adapters, expectedBase],
  )
  const applied = compatible.flatMap((adapter) => {
    const choice = choices.get(adapter.name)
    return choice ? [{ adapter, choice }] : []
  })
  const available = compatible.filter((adapter) => !choices.has(adapter.name))
  const active = applied.filter(({ choice }) => choice.strength > 0)
  const bypassed = applied.filter(({ choice }) => choice.strength === 0)
  const full = applied.length >= max

  let summary: string
  if (adapters.length === 0) {
    summary = t('lora.summary.noneInstalled')
  } else if (kind === 'magenta') {
    summary = t('lora.summary.unavailable')
  } else if (compatible.length === 0) {
    summary = t('lora.summary.noneCompatible')
  } else if (active.length === 1 && bypassed.length === 0) {
    const [{ adapter, choice }] = active
    summary = t('lora.summary.single', {
      name: adapter.slug,
      value: strengthText(choice.strength),
    })
  } else if (active.length > 0 && bypassed.length > 0) {
    summary = t('lora.summary.activeAndBypassed', {
      active: active.length,
      bypassed: bypassed.length,
    })
  } else if (active.length > 0) {
    summary = t('lora.summary.active', { count: active.length })
  } else if (bypassed.length > 0) {
    summary = t('lora.summary.bypassed', { count: bypassed.length })
  } else {
    summary = t('lora.summary.off')
  }

  const repoDraft = normalizeHfRepo(repo)
  const loraProgress = library.progress?.family === 'lora' ? library.progress : null
  const snapshotInstalling = library.status?.installing?.family === 'lora'
  const progressLabel = loraProgress
    ? (() => {
        const stage = t(`modelManager.stage.${loraProgress.stage}`, {
          defaultValue: loraProgress.stage,
        })
        return loraProgress.file ? `${stage} ${loraProgress.file}` : stage
      })()
    : snapshotInstalling
      ? t('modelManager.installing')
      : null

  const install = (source: { hfRepo: string } | { path: string }, name: string) => {
    void library.install(source, name, base === 'auto' ? undefined : base)
  }

  const importFile = () => {
    void (async () => {
      const path = await invoke<string | null>('plugin:dialog|open', {
        options: {
          multiple: false,
          filters: [{ name: t('modelManager.loraFileFilter'), extensions: ['safetensors'] }],
        },
      }).catch(() => null)
      if (!path) return
      const name = path.replace(/\/+$/, '').split('/').pop() || path
      install({ path }, name)
    })()
  }

  const apply = (name: string) => {
    onToggle(name)
    if (library.recentlyInstalled === name) library.clearRecentlyInstalled()
  }

  const removeAdapter = (adapter: LoraAdapter, selected: boolean) => {
    if (selected && !window.confirm(t('lora.confirmDelete', { name: adapter.slug }))) return
    void library.deleteLora(adapter.name)
  }

  const adapterMeta = (adapter: LoraAdapter) =>
    `${t(`modelManager.loraBase.${adapter.base}`)} · ${formatBytes(adapter.sizeBytes)}`

  return (
    <div className={`ui-lora-control ui-lora-control--${accent}`}>
      <span className="ui-lora-control__label">{t('lora.label')}</span>
      <button
        ref={triggerRef}
        type="button"
        className={`ui-lora-control__trigger${active.length > 0 ? ' ui-lora-control__trigger--on' : ''}`}
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={t('lora.triggerLabel', { summary })}
        onClick={() => setOpen((current) => !current)}
      >
        <span className="ui-lora-control__led" aria-hidden="true" />
        <span className="ui-lora-control__summary">{summary}</span>
        <span className="ui-lora-control__chevron" aria-hidden="true">▾</span>
      </button>

      <AnchoredPanel
        open={open}
        anchorRef={triggerRef}
        onClose={() => setOpen(false)}
        label={t('lora.panelTitle')}
        className={`ui-lora-panel ui-lora-panel--${accent}`}
      >
        <header className="ui-lora-panel__header">
          <div>
            <h3 className="ui-lora-panel__title">{t('lora.panelTitle')}</h3>
            <p className="ui-lora-panel__context">
              {kind === 'magenta'
                ? t('lora.unavailableDetail')
                : t('lora.compatibleWith', {
                    base: t(`modelManager.loraBase.${expectedBase}`),
                  })}
            </p>
          </div>
          <div className="ui-lora-panel__header-actions">
            <Button onClick={() => void library.openFolder()}>{t('modelManager.openFolder')}</Button>
            <button
              type="button"
              className="ui-lora-panel__close"
              aria-label={t('lora.close')}
              onClick={() => setOpen(false)}
            >
              ×
            </button>
          </div>
        </header>

        <div className="ui-lora-panel__body">
          {library.error ? (
            <p className="ui-lora-panel__error" role="alert">
              {t('modelManager.errorPrefix', { message: library.error })}
            </p>
          ) : null}

          <section className="ui-lora-panel__section" aria-labelledby={appliedHeadingId}>
            <h4 id={appliedHeadingId} className="ui-lora-panel__heading">
              {t('lora.applied')}
            </h4>
            {applied.length === 0 ? (
              <p className="ui-lora-panel__empty">{t('lora.noneApplied')}</p>
            ) : (
              <div className="ui-lora-panel__list">
                {applied.map(({ adapter, choice }) => (
                  <div
                    className={`ui-lora-panel__adapter ui-lora-panel__adapter--${choice.strength === 0 ? 'bypassed' : 'on'}`}
                    key={adapter.name}
                  >
                    <div className="ui-lora-panel__adapter-main">
                      <div className="ui-lora-panel__adapter-title">
                        <span className="ui-lora-panel__adapter-name">{adapter.slug}</span>
                        <span className="ui-lora-panel__state">
                          {t(choice.strength === 0 ? 'lora.bypassed' : 'lora.on')}
                        </span>
                      </div>
                      <span className="ui-lora-panel__meta">{adapterMeta(adapter)}</span>
                    </div>
                    <Knob
                      label={t('lora.strengthValue', { value: strengthText(choice.strength) })}
                      ariaLabel={t('lora.strength', { name: adapter.slug })}
                      size="s"
                      accent={accent}
                      value={choice.strength}
                      min={STRENGTH_MIN}
                      max={STRENGTH_MAX}
                      step={STRENGTH_STEP}
                      resetValue={STRENGTH_REST}
                      onChange={(strength) => onStrength(adapter.name, strength)}
                    />
                    <div className="ui-lora-panel__actions">
                      <Button
                        lit={choice.strength === 0}
                        aria-pressed={choice.strength === 0}
                        onClick={() => onToggleBypass(adapter.name)}
                      >
                        {t(choice.strength === 0 ? 'lora.enable' : 'lora.bypass')}
                      </Button>
                      <Button onClick={() => onToggle(adapter.name)}>{t('lora.remove')}</Button>
                      <Button
                        aria-label={t('modelManager.loraDelete', { name: adapter.slug })}
                        onClick={() => removeAdapter(adapter, true)}
                      >
                        ×
                      </Button>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </section>

          <section className="ui-lora-panel__section" aria-labelledby={availableHeadingId}>
            <h4 id={availableHeadingId} className="ui-lora-panel__heading">
              {t('lora.available')}
            </h4>
            {library.loading && adapters.length === 0 ? (
              <p className="ui-lora-panel__empty">{t('modelManager.loading')}</p>
            ) : available.length === 0 ? (
              <p className="ui-lora-panel__empty">
                {adapters.length === 0
                  ? t('modelManager.loraNone')
                  : compatible.length === 0
                    ? t('lora.noneCompatible')
                    : t('lora.noneAvailable')}
              </p>
            ) : (
              <div className="ui-lora-panel__list">
                {available.map((adapter) => (
                  <div
                    className={`ui-lora-panel__adapter${library.recentlyInstalled === adapter.name ? ' ui-lora-panel__adapter--new' : ''}`}
                    key={adapter.name}
                  >
                    <div className="ui-lora-panel__adapter-main">
                      <div className="ui-lora-panel__adapter-title">
                        <span className="ui-lora-panel__adapter-name">{adapter.slug}</span>
                        {library.recentlyInstalled === adapter.name ? (
                          <span className="ui-lora-panel__new">{t('lora.new')}</span>
                        ) : null}
                      </div>
                      <span className="ui-lora-panel__meta">{adapterMeta(adapter)}</span>
                    </div>
                    <div className="ui-lora-panel__actions">
                      <Button
                        variant="primary"
                        disabled={full || kind === 'magenta'}
                        title={full ? t('lora.stackFull', { max }) : undefined}
                        onClick={() => apply(adapter.name)}
                      >
                        {t('lora.apply')}
                      </Button>
                      <Button
                        aria-label={t('modelManager.loraDelete', { name: adapter.slug })}
                        onClick={() => removeAdapter(adapter, false)}
                      >
                        ×
                      </Button>
                    </div>
                  </div>
                ))}
              </div>
            )}
            {full ? <p className="ui-lora-panel__hint">{t('lora.stackFull', { max })}</p> : null}
          </section>

          {incompatible.length > 0 ? (
            <details className="ui-lora-panel__disclosure">
              <summary>{t('lora.incompatible', { count: incompatible.length })}</summary>
              <div className="ui-lora-panel__list">
                {incompatible.map((adapter) => (
                  <div className="ui-lora-panel__adapter ui-lora-panel__adapter--incompatible" key={adapter.name}>
                    <div className="ui-lora-panel__adapter-main">
                      <span className="ui-lora-panel__adapter-name">{adapter.slug}</span>
                      <span className="ui-lora-panel__meta">
                        {t(adapter.base === 'medium' ? 'lora.requiresTrack' : 'lora.requiresPads')}
                      </span>
                    </div>
                    <Button
                      aria-label={t('modelManager.loraDelete', { name: adapter.slug })}
                      onClick={() => removeAdapter(adapter, choices.has(adapter.name))}
                    >
                      ×
                    </Button>
                  </div>
                ))}
              </div>
            </details>
          ) : null}

          <details className="ui-lora-panel__disclosure ui-lora-panel__install">
            <summary>{t('lora.installAdapter')}</summary>
            <div className="ui-lora-panel__install-body">
              <TextField
                label={t('modelManager.loraRepo')}
                value={repo}
                placeholder={t('modelManager.loraRepoPlaceholder')}
                onChange={(event) => setRepo(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === 'Enter' && repoDraft && !library.isInstalling) {
                    install({ hfRepo: repoDraft }, repoDraft)
                  }
                }}
              />
              <details className="ui-lora-panel__advanced">
                <summary>{t('lora.advanced')}</summary>
                <Select
                  label={t('modelManager.loraBaseLabel')}
                  value={base}
                  options={[
                    { value: 'auto', label: t('modelManager.loraBaseAuto') },
                    { value: 'small', label: t('modelManager.loraBase.small') },
                    { value: 'medium', label: t('modelManager.loraBase.medium') },
                  ]}
                  onChange={(next) => setBase(next as 'auto' | LoraBase)}
                />
              </details>
              <div className="ui-lora-panel__install-actions">
                {progressLabel ? (
                  <Button onClick={() => void library.cancelInstall()}>
                    {t('modelManager.cancel')}
                  </Button>
                ) : (
                  <>
                    <Button
                      variant="primary"
                      disabled={!repoDraft || library.isInstalling}
                      onClick={() => install({ hfRepo: repoDraft }, repoDraft)}
                    >
                      {t('modelManager.install')}
                    </Button>
                    <Button disabled={library.isInstalling} onClick={importFile}>
                      {t('modelManager.loraImportFile')}
                    </Button>
                  </>
                )}
              </div>
              {progressLabel ? <p className="ui-lora-panel__progress">{progressLabel}</p> : null}
              {library.isInstalling && !progressLabel ? (
                <p className="ui-lora-panel__hint">{t('lora.anotherInstall')}</p>
              ) : null}
            </div>
          </details>
        </div>
      </AnchoredPanel>
    </div>
  )
}
