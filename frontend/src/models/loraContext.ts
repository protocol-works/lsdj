import { createContext, useContext } from 'react'

import type {
  LoraAdapter,
  LoraBase,
  ModelProgress,
  ModelStatus,
} from '../audio/nativeEngine'

export type LoraInstallSource = { hfRepo: string } | { path: string }

/** Shared LoRA registry/lifecycle state. Selection remains local to each
 * generation form; only the installed-library truth and the shell's one global
 * install pipeline live here. */
export type LoraLibraryState = {
  status: ModelStatus | null
  loras: LoraAdapter[]
  loading: boolean
  progress: ModelProgress | null
  isInstalling: boolean
  error: string | null
  recentlyInstalled: string | null
  install: (
    source: LoraInstallSource,
    displayName: string,
    base?: LoraBase,
  ) => Promise<void>
  cancelInstall: () => Promise<void>
  deleteLora: (name: string) => Promise<void>
  openFolder: () => Promise<void>
  clearError: () => void
  clearRecentlyInstalled: () => void
}

const EMPTY_LIBRARY: LoraLibraryState = {
  status: null,
  loras: [],
  loading: true,
  progress: null,
  isInstalling: false,
  error: null,
  recentlyInstalled: null,
  install: async () => {},
  cancelInstall: async () => {},
  deleteLora: async () => {},
  openFolder: async () => {},
  clearError: () => {},
  clearRecentlyInstalled: () => {},
}

export const LoraLibraryContext = createContext<LoraLibraryState>(EMPTY_LIBRARY)

export function useLoraLibrary(): LoraLibraryState {
  return useContext(LoraLibraryContext)
}
