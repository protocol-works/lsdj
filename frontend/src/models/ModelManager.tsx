import { useCallback, useEffect, useState } from 'react'
import type { ReactNode } from 'react'
import { useTranslation } from 'react-i18next'

import { Button } from '../ui/Button'
import {
  cancelInstall,
  installModel,
  modelStatus,
  openModelFolder,
  subscribeModelProgress,
  subscribeModelsChanged,
  updateModel,
  type ModelFamily,
  type ModelProgress,
  type ModelStatus,
} from '../audio/nativeEngine'
import { formatBytes } from './formatBytes'

/** The model manager (issue #43): install / on-disk size for both model
 * families, with live install progress and cancel. Rust owns the lifecycle; this
 * reads `model_status` and drives the install commands. Deletion is done
 * natively via "Open models folder" (the watcher reflects it live). The in-flight
 * install is reflected from the status snapshot too, so closing and reopening the
 * drawer mid-download still shows "Installing… / Cancel". */
export function ModelManager() {
  const { t } = useTranslation()
  const [status, setStatus] = useState<ModelStatus | null>(null)
  const [progress, setProgress] = useState<ModelProgress | null>(null)
  const [error, setError] = useState<string | null>(null)

  const refresh = useCallback(() => {
    modelStatus().then(setStatus).catch(() => {})
  }, [])

  useEffect(() => {
    refresh()
    const unsubChanged = subscribeModelsChanged(refresh)
    const unsubProgress = subscribeModelProgress((event) => {
      // `done` (success) and `cancelled` (user stop) both just clear the in-flight
      // UI; the follow-up `models://changed` reflects the resulting state.
      if (event.stage === 'done' || event.stage === 'cancelled') {
        setProgress(null)
        setError(null)
      } else if (event.stage === 'error') {
        setProgress(null)
        setError(event.message ?? t('modelManager.stage.error'))
      } else {
        setProgress(event)
        setError(null)
      }
    })
    return () => {
      unsubChanged()
      unsubProgress()
    }
  }, [refresh, t])

  const onInstall = useCallback((family: ModelFamily, name?: string) => {
    setError(null)
    // Show "installing" before the first progress event lands.
    setProgress({ family, name: name ?? '', stage: 'download', message: null, file: null })
    installModel(family, name).catch((e: unknown) => {
      setProgress(null)
      setError(String(e))
    })
  }, [])

  const onUpdate = useCallback((family: ModelFamily) => {
    setError(null)
    // An update re-fetches the pinned source; surface progress from the fetch on.
    setProgress({ family, name: '', stage: 'fetch', message: null, file: null })
    updateModel(family).catch((e: unknown) => {
      setProgress(null)
      setError(String(e))
    })
  }, [])

  if (!status) {
    return <p className="modelmgr__empty">{t('modelManager.loading')}</p>
  }

  const progressLabel = (event: ModelProgress) => {
    const stage = t(`modelManager.stage.${event.stage}`, { defaultValue: event.stage })
    // The keyed stage carries the wording; only the file path (data) is appended.
    return event.file ? `${stage} ${event.file}` : stage
  }

  // An install is in flight if a live progress event says so, or the status
  // snapshot does (the latter survives a drawer close/reopen). The live event,
  // when present, gives the more detailed label.
  const snapshot = status.installing
  const isInstalling = progress !== null || snapshot !== null
  const installLabel = (family: ModelFamily, name: string): string | null => {
    if (progress && progress.family === family && progress.name === name) return progressLabel(progress)
    if (snapshot && snapshot.family === family && snapshot.name === name) return t('modelManager.installing')
    return null
  }

  const installedNames = new Set(status.magenta.installed.map((model) => model.name))
  const missing = status.magenta.installable.filter((name) => !installedNames.has(name))
  const sa3 = status.sa3
  const sa3Present = sa3.state !== 'missing'
  const sa3Ready = sa3.state === 'ready'
  const sa3Label = installLabel('sa3', '')

  // Cancel while this row's install is in flight (label set), else its primary
  // action (Install / Repair, or nothing).
  const installAction = (label: string | null, primary: ReactNode) =>
    label ? <Button onClick={() => cancelInstall()}>{t('modelManager.cancel')}</Button> : primary

  return (
    <div>
      {error && (
        <p className="modelmgr__error" role="alert">
          {t('modelManager.errorPrefix', { message: error })}
        </p>
      )}

      <section className="modelmgr__section">
        <div className="modelmgr__section-head">
          <h4 className="modelmgr__subheading">{t('modelManager.magenta')}</h4>
          <Button onClick={() => openModelFolder('magenta')}>{t('modelManager.openFolder')}</Button>
        </div>
        {status.magenta.installed.length === 0 && missing.length === 0 && (
          <p className="modelmgr__empty">{t('modelManager.none')}</p>
        )}
        {status.magenta.installed.map((model) => {
          // A model present but missing shared resources can't load; offer a
          // repair (re-runs install, which fetches the resources). Otherwise it's
          // a plain status row.
          const label = model.needsResources ? installLabel('magenta', model.name) : null
          return (
            <div className="modelmgr__row" key={model.name}>
              <div className="modelmgr__row-main">
                <div className="modelmgr__name">{model.name}</div>
                <div className={`modelmgr__meta${model.needsResources ? ' modelmgr__meta--warn' : ''}`}>
                  {model.needsResources ? t('modelManager.needsResources') : formatBytes(model.sizeBytes)}
                </div>
                {label && <div className="modelmgr__progress">{label}</div>}
              </div>
              {model.needsResources && (
                <div className="modelmgr__actions">
                  {installAction(
                    label,
                    <Button variant="primary" onClick={() => onInstall('magenta', model.name)} disabled={isInstalling}>
                      {t('modelManager.repair')}
                    </Button>,
                  )}
                </div>
              )}
            </div>
          )
        })}
        {missing.map((name) => {
          const label = installLabel('magenta', name)
          return (
            <div className="modelmgr__row" key={name}>
              <div className="modelmgr__row-main">
                <div className="modelmgr__name">{name}</div>
                <div className="modelmgr__meta">{t('modelManager.notInstalled')}</div>
                {label && <div className="modelmgr__progress">{label}</div>}
              </div>
              <div className="modelmgr__actions">
                {installAction(
                  label,
                  <Button variant="primary" onClick={() => onInstall('magenta', name)} disabled={isInstalling}>
                    {t('modelManager.install')}
                  </Button>,
                )}
              </div>
            </div>
          )
        })}
      </section>

      <section className="modelmgr__section">
        <div className="modelmgr__section-head">
          <h4 className="modelmgr__subheading">{t('modelManager.sa3')}</h4>
          {sa3Present && (
            <Button onClick={() => openModelFolder('sa3')}>{t('modelManager.openFolder')}</Button>
          )}
        </div>
        <div className="modelmgr__row">
          <div className="modelmgr__row-main">
            <div className="modelmgr__name">{t('modelManager.sa3')}</div>
            <div className="modelmgr__meta">
              {t(`modelManager.sa3State.${sa3.state}`)}
              {sa3Present && sa3.sizeBytes > 0 ? ` · ${formatBytes(sa3.sizeBytes)}` : ''}
            </div>
            {sa3Ready && sa3.updateAvailable && (
              <div className="modelmgr__meta modelmgr__meta--warn">{t('modelManager.updateAvailable')}</div>
            )}
            {sa3Label && <div className="modelmgr__progress">{sa3Label}</div>}
          </div>
          <div className="modelmgr__actions">
            {installAction(
              sa3Label,
              !sa3Ready ? (
                <Button variant="primary" onClick={() => onInstall('sa3')} disabled={isInstalling}>
                  {t('modelManager.install')}
                </Button>
              ) : sa3.updateAvailable ? (
                <Button variant="primary" onClick={() => onUpdate('sa3')} disabled={isInstalling}>
                  {t('modelManager.update')}
                </Button>
              ) : null,
            )}
          </div>
        </div>
      </section>
    </div>
  )
}
