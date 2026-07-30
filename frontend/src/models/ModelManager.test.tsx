import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { ModelProgress, ModelStatus } from '../audio/nativeEngine'

// Capture the subscriber callbacks so tests can fire watcher / progress events.
let changedCb: (() => void) | null = null
let progressCb: ((event: ModelProgress) => void) | null = null
const modelStatus = vi.fn<() => Promise<ModelStatus>>()
const installModel = vi.fn<(family: string, name?: string) => Promise<void>>(async () => {})
const updateModel = vi.fn<(family: string) => Promise<void>>(async () => {})
const cancelInstall = vi.fn(async () => {})
const openModelFolder = vi.fn<(family: string) => Promise<void>>(async () => {})

vi.mock('../audio/nativeEngine', () => ({
  modelStatus: () => modelStatus(),
  installModel: (family: string, name?: string) => installModel(family, name),
  updateModel: (family: string) => updateModel(family),
  cancelInstall: () => cancelInstall(),
  openModelFolder: (family: string) => openModelFolder(family),
  subscribeModelsChanged: (cb: () => void) => {
    changedCb = cb
    return () => {}
  },
  subscribeModelProgress: (cb: (event: ModelProgress) => void) => {
    progressCb = cb
    return () => {}
  },
}))

import { ModelManager } from './ModelManager'

function status(overrides: Partial<ModelStatus> = {}): ModelStatus {
  return {
    magenta: {
      modelsDir: '/models',
      resourcesPresent: true,
      installable: ['mrt2_small', 'mrt2_base'],
      installed: [{ name: 'mrt2_small', sizeBytes: 2_000_000_000, needsResources: false }],
    },
    sa3: {
      state: 'missing',
      sizeBytes: 0,
      checkout: null,
      installedSource: null,
      pinnedSource: { repo: 'https://github.com/brxs/stable-audio-3', commit: 'pinned1' },
      updateAvailable: false,
    },
    loras: [],
    installing: null,
    ...overrides,
  }
}

beforeEach(() => {
  vi.clearAllMocks()
  changedCb = null
  progressCb = null
})

describe('ModelManager', () => {
  it('lists installed models with size and missing models', async () => {
    modelStatus.mockResolvedValue(status())
    render(<ModelManager />)
    expect(await screen.findByText('mrt2_small')).toBeInTheDocument()
    expect(screen.getByText('2.0 GB')).toBeInTheDocument()
    expect(screen.getByText('mrt2_base')).toBeInTheDocument() // the missing official model
  })

  it('installs a missing model', async () => {
    modelStatus.mockResolvedValue(status())
    render(<ModelManager />)
    await screen.findByText('mrt2_base')
    fireEvent.click(screen.getAllByText('Install')[0]) // mrt2_base is first in the DOM
    expect(installModel).toHaveBeenCalledWith('magenta', 'mrt2_base')
  })

  it('shows live progress, then re-fetches status on a change event', async () => {
    modelStatus.mockResolvedValue(status())
    render(<ModelManager />)
    await screen.findByText('mrt2_base')
    act(() =>
      progressCb?.({ family: 'magenta', name: 'mrt2_base', stage: 'download', message: null, file: 'state.safetensors' }),
    )
    expect(screen.getByText(/state\.safetensors/)).toBeInTheDocument()
    act(() => progressCb?.({ family: 'magenta', name: 'mrt2_base', stage: 'done', message: null, file: null }))
    act(() => changedCb?.())
    await waitFor(() => expect(modelStatus).toHaveBeenCalledTimes(2))
  })

  it('reflects an in-flight install from the status snapshot (drawer reopened mid-download)', async () => {
    // No live progress event has been seen (fresh mount), but the snapshot says
    // mrt2_base is installing — the row must show Cancel, not Install.
    modelStatus.mockResolvedValue(status({ installing: { family: 'magenta', name: 'mrt2_base' } }))
    render(<ModelManager />)
    await screen.findByText('mrt2_base')
    expect(screen.getByText('Installing…')).toBeInTheDocument()
    expect(screen.getByText('Cancel')).toBeInTheDocument()
    fireEvent.click(screen.getByText('Cancel'))
    expect(cancelInstall).toHaveBeenCalledTimes(1)
  })

  it('offers a repair action for a model missing shared resources', async () => {
    modelStatus.mockResolvedValue(
      status({
        magenta: {
          modelsDir: '/models',
          resourcesPresent: false,
          installable: ['mrt2_small', 'mrt2_base'],
          installed: [{ name: 'mrt2_small', sizeBytes: 2_000_000_000, needsResources: true }],
        },
      }),
    )
    render(<ModelManager />)
    await screen.findByText('mrt2_small')
    fireEvent.click(screen.getByText('Repair'))
    expect(installModel).toHaveBeenCalledWith('magenta', 'mrt2_small')
  })

  it('reveals each family folder for native inspection', async () => {
    modelStatus.mockResolvedValue(
      status({
        sa3: {
          state: 'ready',
          sizeBytes: 5_000_000_000,
          checkout: '/sa3',
          installedSource: { repo: 'https://github.com/brxs/stable-audio-3', commit: 'pinned1' },
          pinnedSource: { repo: 'https://github.com/brxs/stable-audio-3', commit: 'pinned1' },
          updateAvailable: false,
        },
      }),
    )
    render(<ModelManager />)
    await screen.findByText('mrt2_small')
    const buttons = screen.getAllByText('Open folder') // Magenta + SA3 (present)
    expect(buttons).toHaveLength(2)
    fireEvent.click(buttons[0])
    expect(openModelFolder).toHaveBeenCalledWith('magenta')
    fireEvent.click(buttons[1])
    expect(openModelFolder).toHaveBeenCalledWith('sa3')
  })

  it('treats a cancel as a clean stop, not an error', async () => {
    modelStatus.mockResolvedValue(status())
    render(<ModelManager />)
    await screen.findByText('mrt2_base')
    act(() =>
      progressCb?.({ family: 'magenta', name: 'mrt2_base', stage: 'download', message: null, file: 'x' }),
    )
    expect(screen.getByText('Cancel')).toBeInTheDocument() // installing
    act(() =>
      progressCb?.({ family: 'magenta', name: 'mrt2_base', stage: 'cancelled', message: null, file: null }),
    )
    // No error banner, and the in-flight state is cleared (Cancel gone).
    expect(screen.queryByRole('alert')).toBeNull()
    expect(screen.queryByText('Cancel')).toBeNull()
  })

  it('offers an in-place update when the installed SA3 source drifted from the pin', async () => {
    modelStatus.mockResolvedValue(
      status({
        sa3: {
          state: 'ready',
          sizeBytes: 5_000_000_000,
          checkout: '/sa3',
          installedSource: { repo: 'https://github.com/brxs/stable-audio-3', commit: 'oldsha' },
          pinnedSource: { repo: 'https://github.com/brxs/stable-audio-3', commit: 'newsha' },
          updateAvailable: true,
        },
      }),
    )
    render(<ModelManager />)
    await screen.findByText('Update available — a newer version is pinned')
    fireEvent.click(screen.getByText('Update'))
    expect(updateModel).toHaveBeenCalledWith('sa3')
  })

  it('shows no update action when the installed SA3 matches the pin', async () => {
    modelStatus.mockResolvedValue(
      status({
        sa3: {
          state: 'ready',
          sizeBytes: 5_000_000_000,
          checkout: '/sa3',
          installedSource: { repo: 'https://github.com/brxs/stable-audio-3', commit: 'pinned1' },
          pinnedSource: { repo: 'https://github.com/brxs/stable-audio-3', commit: 'pinned1' },
          updateAvailable: false,
        },
      }),
    )
    render(<ModelManager />)
    await screen.findByText('mrt2_small')
    expect(screen.queryByText('Update')).toBeNull()
  })

  it('reports an install error', async () => {
    modelStatus.mockResolvedValue(status())
    render(<ModelManager />)
    await screen.findByText('mrt2_base')
    act(() =>
      progressCb?.({ family: 'magenta', name: 'mrt2_base', stage: 'error', message: 'no weights', file: null }),
    )
    expect(screen.getByRole('alert')).toHaveTextContent('no weights')
  })

})
