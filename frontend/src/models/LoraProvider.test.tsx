import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { ModelProgress, ModelStatus } from '../audio/nativeEngine'

let changedCallback: (() => void) | null = null
let progressCallback: ((event: ModelProgress) => void) | null = null
const modelStatusMock = vi.fn<() => Promise<ModelStatus>>()
const installLoraMock = vi.fn<
  (source: object, base?: string) => Promise<void>
>(async () => {})
const deleteLoraMock = vi.fn<(name: string) => Promise<void>>(async () => {})
const cancelInstallMock = vi.fn(async () => {})
const openModelFolderMock = vi.fn<(family: string) => Promise<void>>(async () => {})

vi.mock('../audio/nativeEngine', () => ({
  modelStatus: () => modelStatusMock(),
  installLora: (source: object, base?: string) => installLoraMock(source, base),
  deleteLora: (name: string) => deleteLoraMock(name),
  cancelInstall: () => cancelInstallMock(),
  openModelFolder: (family: string) => openModelFolderMock(family),
  subscribeModelsChanged: (callback: () => void) => {
    changedCallback = callback
    return () => {}
  },
  subscribeModelProgress: (callback: (event: ModelProgress) => void) => {
    progressCallback = callback
    return () => {}
  },
}))

import { useLoraLibrary } from './loraContext'
import { LoraProvider } from './LoraProvider'

function status(overrides: Partial<ModelStatus> = {}): ModelStatus {
  return {
    magenta: {
      modelsDir: '/models',
      resourcesPresent: true,
      installable: [],
      installed: [],
    },
    sa3: {
      state: 'ready',
      backend: 'mlx',
      sizeBytes: 5_000_000_000,
      downloadBytes: 9_154_794_562,
      checkout: '/sa3',
      installedSource: null,
      pinnedSource: { repo: 'https://github.com/Stability-AI/stable-audio-3', commit: 'pin' },
      updateAvailable: false,
    },
    loras: [],
    installing: null,
    ...overrides,
  }
}

const maqam = {
  name: 'medium/maqam',
  base: 'medium' as const,
  slug: 'maqam',
  sizeBytes: 200_000_000,
  source: 'owner/maqam',
  adapterType: 'lora',
  rank: 64,
}

function Probe({ suffix = '' }: { suffix?: string }) {
  const library = useLoraLibrary()
  return (
    <div>
      <span>{`names${suffix}:${library.loras.map((adapter) => adapter.name).join(',')}`}</span>
      <span>{`loading${suffix}:${library.loading}`}</span>
      <span>{`progress${suffix}:${library.progress?.stage ?? 'none'}`}</span>
      <span>{`error${suffix}:${library.error ?? 'none'}`}</span>
      <span>{`recent${suffix}:${library.recentlyInstalled ?? 'none'}`}</span>
      <button onClick={() => void library.install({ hfRepo: 'owner/maqam' }, 'owner/maqam', 'medium')}>
        install{suffix}
      </button>
      <button onClick={() => void library.cancelInstall()}>cancel{suffix}</button>
      <button onClick={() => void library.deleteLora('medium/maqam')}>delete{suffix}</button>
      <button onClick={() => void library.openFolder()}>folder{suffix}</button>
    </div>
  )
}

beforeEach(() => {
  vi.clearAllMocks()
  changedCallback = null
  progressCallback = null
})

describe('LoraProvider', () => {
  it('shares one registry fetch and event subscriptions across all consumers', async () => {
    modelStatusMock.mockResolvedValue(status({ loras: [maqam] }))
    render(
      <LoraProvider>
        <Probe suffix="a" />
        <Probe suffix="b" />
      </LoraProvider>,
    )
    expect(await screen.findByText('namesa:medium/maqam')).toBeInTheDocument()
    expect(screen.getByText('namesb:medium/maqam')).toBeInTheDocument()
    expect(modelStatusMock).toHaveBeenCalledTimes(1)
    expect(changedCallback).not.toBeNull()
    expect(progressCallback).not.toBeNull()
  })

  it('drives install, cancel, delete, folder, progress, and LoRA-only errors', async () => {
    modelStatusMock.mockResolvedValue(status())
    render(
      <LoraProvider>
        <Probe />
      </LoraProvider>,
    )
    await screen.findByText('names:')

    fireEvent.click(screen.getByText('install'))
    expect(installLoraMock).toHaveBeenCalledWith({ hfRepo: 'owner/maqam' }, 'medium')
    expect(screen.getByText('progress:fetch')).toBeInTheDocument()

    act(() =>
      progressCallback?.({
        family: 'lora',
        name: 'owner/maqam',
        stage: 'download',
        message: null,
        file: 'adapter.safetensors',
      }),
    )
    expect(screen.getByText('progress:download')).toBeInTheDocument()
    fireEvent.click(screen.getByText('cancel'))
    fireEvent.click(screen.getByText('delete'))
    fireEvent.click(screen.getByText('folder'))
    expect(cancelInstallMock).toHaveBeenCalledTimes(1)
    expect(deleteLoraMock).toHaveBeenCalledWith('medium/maqam')
    expect(openModelFolderMock).toHaveBeenCalledWith('lora')

    act(() =>
      progressCallback?.({
        family: 'magenta',
        name: 'mrt2',
        stage: 'error',
        message: 'other failure',
        file: null,
      }),
    )
    expect(screen.getByText('error:none')).toBeInTheDocument()
    act(() =>
      progressCallback?.({
        family: 'lora',
        name: 'owner/maqam',
        stage: 'error',
        message: 'not a recognised SA3 LoRA',
        file: null,
      }),
    )
    expect(screen.getByText('error:not a recognised SA3 LoRA')).toBeInTheDocument()
  })

  it('refreshes on registry changes and identifies a newly installed adapter', async () => {
    modelStatusMock
      .mockResolvedValueOnce(status())
      .mockResolvedValueOnce(status({ loras: [maqam] }))
    render(
      <LoraProvider>
        <Probe />
      </LoraProvider>,
    )
    await screen.findByText('names:')
    act(() => changedCallback?.())
    await waitFor(() => expect(screen.getByText('names:medium/maqam')).toBeInTheDocument())
    expect(screen.getByText('recent:medium/maqam')).toBeInTheDocument()
  })

  it('settles into the empty state when the native registry is unavailable', async () => {
    modelStatusMock.mockRejectedValue(new Error('native shell unavailable'))
    render(
      <LoraProvider>
        <Probe />
      </LoraProvider>,
    )
    expect(await screen.findByText('loading:false')).toBeInTheDocument()
    expect(screen.getByText('names:')).toBeInTheDocument()
  })
})
