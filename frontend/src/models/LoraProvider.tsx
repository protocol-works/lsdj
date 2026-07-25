import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'

import {
  cancelInstall as cancelNativeInstall,
  deleteLora as deleteNativeLora,
  installLora as installNativeLora,
  modelStatus,
  openModelFolder,
  subscribeModelProgress,
  subscribeModelsChanged,
  type LoraBase,
  type ModelProgress,
  type ModelStatus,
} from '../audio/nativeEngine'
import {
  LoraLibraryContext,
  type LoraInstallSource,
  type LoraLibraryState,
} from './loraContext'

const NO_LORAS: LoraLibraryState['loras'] = []

/** One provider above the booth deduplicates `model_status`,
 * `models://changed`, and `model://progress` across Generate, Samples, and both
 * decks. It intentionally does not own any generation stack. */
export function LoraProvider({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<ModelStatus | null>(null)
  const [loading, setLoading] = useState(true)
  const [progress, setProgress] = useState<ModelProgress | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [recentlyInstalled, setRecentlyInstalled] = useState<string | null>(null)
  const knownNamesRef = useRef<Set<string> | null>(null)

  const refresh = useCallback(async () => {
    try {
      const next = await modelStatus()
      const nextNames = new Set(next.loras.map((adapter) => adapter.name))
      const knownNames = knownNamesRef.current
      if (knownNames !== null) {
        const added = next.loras.find((adapter) => !knownNames.has(adapter.name))
        if (added) setRecentlyInstalled(added.name)
      }
      knownNamesRef.current = nextNames
      setStatus(next)
    } catch {
      // A plain-browser dev/test session has no shell registry. Match the
      // existing hook's quiet empty-state fallback rather than surfacing noise.
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    void refresh()
    const unsubscribeChanged = subscribeModelsChanged(() => void refresh())
    const unsubscribeProgress = subscribeModelProgress((event) => {
      if (event.stage === 'done' || event.stage === 'cancelled') {
        setProgress(null)
        if (event.family === 'lora') setError(null)
        return
      }
      if (event.stage === 'error') {
        setProgress(null)
        if (event.family === 'lora') setError(event.message ?? 'Import failed')
        return
      }
      setProgress(event)
      if (event.family === 'lora') setError(null)
    })
    return () => {
      unsubscribeChanged()
      unsubscribeProgress()
    }
  }, [refresh])

  const install = useCallback(
    async (source: LoraInstallSource, displayName: string, base?: LoraBase) => {
      setError(null)
      setProgress({
        family: 'lora',
        name: displayName,
        stage: 'fetch',
        message: null,
        file: null,
      })
      try {
        await installNativeLora(source, base)
      } catch (reason) {
        setProgress(null)
        setError(String(reason))
      }
    },
    [],
  )

  const cancelInstall = useCallback(async () => {
    try {
      await cancelNativeInstall()
    } catch (reason) {
      setError(String(reason))
    }
  }, [])

  const deleteLora = useCallback(async (name: string) => {
    setError(null)
    try {
      await deleteNativeLora(name)
    } catch (reason) {
      setError(String(reason))
    }
  }, [])

  const openFolder = useCallback(async () => {
    try {
      await openModelFolder('lora')
    } catch (reason) {
      setError(String(reason))
    }
  }, [])

  const clearError = useCallback(() => setError(null), [])
  const clearRecentlyInstalled = useCallback(() => setRecentlyInstalled(null), [])
  const loras = status?.loras ?? NO_LORAS
  const isInstalling = progress !== null || status?.installing != null

  const value = useMemo<LoraLibraryState>(
    () => ({
      status,
      loras,
      loading,
      progress,
      isInstalling,
      error,
      recentlyInstalled,
      install,
      cancelInstall,
      deleteLora,
      openFolder,
      clearError,
      clearRecentlyInstalled,
    }),
    [
      cancelInstall,
      clearError,
      clearRecentlyInstalled,
      deleteLora,
      error,
      install,
      isInstalling,
      loading,
      loras,
      openFolder,
      progress,
      recentlyInstalled,
      status,
    ],
  )

  return <LoraLibraryContext.Provider value={value}>{children}</LoraLibraryContext.Provider>
}
