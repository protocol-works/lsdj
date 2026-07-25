import { fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { useState } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { LoraAdapter } from '../audio/nativeEngine'
import { LoraLibraryContext, type LoraLibraryState } from '../models/loraContext'
import type { LoraChoice } from '../models/useLoras'

const invokeMock = vi.fn<
  (command: string, args?: unknown) => Promise<string | null>
>(async () => null)
vi.mock('../audio/nativeEngine', async (importOriginal) => {
  const original = await importOriginal<typeof import('../audio/nativeEngine')>()
  return {
    ...original,
    invoke: (command: string, args?: unknown) => invokeMock(command, args),
  }
})

import { LoraControl, type LoraGenerationKind } from './LoraControl'

const ADAPTERS: LoraAdapter[] = [
  {
    name: 'medium/maqam',
    base: 'medium',
    slug: 'maqam',
    sizeBytes: 200_000_000,
    source: 'owner/maqam',
    adapterType: 'lora',
    rank: 64,
  },
  {
    name: 'medium/breaks',
    base: 'medium',
    slug: 'breaks',
    sizeBytes: 150_000_000,
    source: null,
    adapterType: 'lora',
    rank: 32,
  },
  {
    name: 'small/crackle',
    base: 'small',
    slug: 'crackle',
    sizeBytes: 50_000_000,
    source: null,
    adapterType: 'lora',
    rank: 8,
  },
]

function libraryState(overrides: Partial<LoraLibraryState> = {}): LoraLibraryState {
  return {
    status: null,
    loras: ADAPTERS,
    loading: false,
    progress: null,
    isInstalling: false,
    error: null,
    recentlyInstalled: null,
    install: vi.fn(async () => {}),
    cancelInstall: vi.fn(async () => {}),
    deleteLora: vi.fn(async () => {}),
    openFolder: vi.fn(async () => {}),
    clearError: vi.fn(),
    clearRecentlyInstalled: vi.fn(),
    ...overrides,
  }
}

function Harness({
  adapters = ADAPTERS,
  kind = 'track',
  initial = [],
  library = libraryState(),
  max,
}: {
  adapters?: LoraAdapter[]
  kind?: LoraGenerationKind
  initial?: LoraChoice[]
  library?: LoraLibraryState
  max?: number
}) {
  const [stack, setStack] = useState(initial)
  const [remembered] = useState(() => new Map<string, number>())
  const toggle = (name: string) =>
    setStack((current) =>
      current.some((choice) => choice.name === name)
        ? current.filter((choice) => choice.name !== name)
        : [...current, { name, strength: remembered.get(name) ?? 1 }],
    )
  const setStrength = (name: string, strength: number) => {
    if (strength > 0) remembered.set(name, strength)
    setStack((current) =>
      current.map((choice) => (choice.name === name ? { ...choice, strength } : choice)),
    )
  }
  const toggleBypass = (name: string) =>
    setStack((current) =>
      current.map((choice) => {
        if (choice.name !== name) return choice
        if (choice.strength > 0) remembered.set(name, choice.strength)
        return {
          ...choice,
          strength: choice.strength === 0 ? (remembered.get(name) ?? 1) : 0,
        }
      }),
    )
  return (
    <LoraLibraryContext.Provider value={library}>
      <LoraControl
        adapters={adapters}
        kind={kind}
        value={stack}
        onToggle={toggle}
        onStrength={setStrength}
        onToggleBypass={toggleBypass}
        max={max}
      />
    </LoraLibraryContext.Provider>
  )
}

function openPanel(summary: string | RegExp = 'LoRA: Off') {
  const trigger = screen.getByRole('button', { name: summary })
  fireEvent.click(trigger)
  return { trigger, panel: screen.getByRole('dialog', { name: 'LoRA adapters' }) }
}

function adapterRow(name: string): HTMLElement {
  const row = screen.getByText(name).closest('.ui-lora-panel__adapter')
  if (!(row instanceof HTMLElement)) throw new Error(`No adapter row for ${name}`)
  return row
}

beforeEach(() => {
  vi.clearAllMocks()
})

describe('LoraControl', () => {
  it('always renders a stable summary for none installed and off', () => {
    const { rerender } = render(<Harness adapters={[]} library={libraryState({ loras: [] })} />)
    expect(screen.getByRole('button', { name: 'LoRA: None installed' })).toHaveAttribute(
      'aria-expanded',
      'false',
    )
    rerender(<Harness />)
    expect(screen.getByRole('button', { name: 'LoRA: Off' })).toBeInTheDocument()
  })

  it('applies, steers, bypasses, restores, and removes with explicit states', () => {
    render(<Harness />)
    openPanel()
    fireEvent.click(within(adapterRow('maqam')).getByRole('button', { name: 'Apply' }))
    expect(screen.getByRole('button', { name: 'LoRA: maqam ×1' })).toHaveClass(
      'ui-lora-control__trigger--on',
    )
    expect(screen.getByText('On')).toBeInTheDocument()

    fireEvent.change(screen.getByLabelText('maqam strength'), { target: { value: '1.5' } })
    expect(screen.getByText('Strength ×1.5')).toBeInTheDocument()
    fireEvent.click(within(adapterRow('maqam')).getByRole('button', { name: 'Bypass' }))
    expect(screen.getByRole('button', { name: 'LoRA: 1 bypassed' })).not.toHaveClass(
      'ui-lora-control__trigger--on',
    )
    expect(screen.getByText('Bypassed')).toBeInTheDocument()

    fireEvent.click(within(adapterRow('maqam')).getByRole('button', { name: 'Enable' }))
    expect(screen.getByLabelText('maqam strength')).toHaveValue('1.5')
    fireEvent.click(within(adapterRow('maqam')).getByRole('button', { name: 'Remove' }))
    expect(screen.getByRole('button', { name: 'LoRA: Off' })).toBeInTheDocument()
  })

  it('summarises multiple active and mixed active/bypassed stacks explicitly', () => {
    render(
      <Harness
        initial={[
          { name: 'medium/maqam', strength: 1 },
          { name: 'medium/breaks', strength: 0.5 },
        ]}
      />,
    )
    openPanel('LoRA: 2 active')
    fireEvent.click(within(adapterRow('breaks')).getByRole('button', { name: 'Bypass' }))
    expect(screen.getByRole('button', { name: 'LoRA: 1 active · 1 bypassed' })).toBeInTheDocument()
  })

  it('distinguishes an installed but incompatible library from Off', () => {
    render(
      <Harness
        adapters={[ADAPTERS[0]]}
        kind="sfx"
        library={libraryState({ loras: [ADAPTERS[0]] })}
      />,
    )
    openPanel('LoRA: None compatible')
    expect(screen.getByText('No installed adapters match this generation engine.')).toBeInTheDocument()
  })

  it('explains incompatible and Magenta adapters instead of hiding the feature', () => {
    const { rerender } = render(<Harness />)
    openPanel()
    expect(screen.getByText('Incompatible adapters (1)')).toBeInTheDocument()
    expect(screen.getByText('Small DiT — select SFX or Music to apply')).toBeInTheDocument()

    fireEvent.click(screen.getByLabelText('Close LoRA adapters'))
    rerender(<Harness kind="magenta" />)
    openPanel('LoRA: Unavailable for Magenta')
    expect(
      screen.getByText('LoRA adapters apply to Stable Audio 3 generation, not Magenta.'),
    ).toBeInTheDocument()
    expect(screen.getByText('Incompatible adapters (3)')).toBeInTheDocument()
  })

  it('enforces the stack cap in the available list', () => {
    render(
      <Harness
        initial={[{ name: 'medium/maqam', strength: 1 }]}
        max={1}
      />,
    )
    openPanel('LoRA: maqam ×1')
    expect(within(adapterRow('breaks')).getByRole('button', { name: 'Apply' })).toBeDisabled()
    expect(screen.getByText('Stack full — up to 1 adapters per generation')).toBeInTheDocument()
  })

  it('installs by canonical HF id or local file and keeps advanced base optional', async () => {
    const library = libraryState()
    invokeMock.mockResolvedValue('/downloads/maqam.safetensors')
    render(<Harness library={library} />)
    openPanel()
    fireEvent.click(screen.getByText('Install adapter…'))
    fireEvent.change(screen.getByLabelText('HuggingFace repo'), {
      target: { value: 'https://huggingface.co/owner/maqam/tree/main' },
    })
    fireEvent.click(screen.getByText('Advanced base override'))
    fireEvent.change(screen.getByLabelText('Base'), { target: { value: 'medium' } })
    fireEvent.click(screen.getByRole('button', { name: 'Install' }))
    expect(library.install).toHaveBeenCalledWith(
      { hfRepo: 'owner/maqam' },
      'owner/maqam',
      'medium',
    )

    fireEvent.click(screen.getByRole('button', { name: 'Import file…' }))
    await waitFor(() =>
      expect(library.install).toHaveBeenCalledWith(
        { path: '/downloads/maqam.safetensors' },
        'maqam.safetensors',
        'medium',
      ),
    )
  })

  it('confirms deletion of an applied adapter and exposes registry management', () => {
    const library = libraryState()
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false)
    render(
      <Harness
        library={library}
        initial={[{ name: 'medium/maqam', strength: 1 }]}
      />,
    )
    openPanel('LoRA: maqam ×1')
    fireEvent.click(screen.getByRole('button', { name: 'Open folder' }))
    expect(library.openFolder).toHaveBeenCalledTimes(1)
    fireEvent.click(within(adapterRow('maqam')).getByRole('button', { name: 'Delete adapter maqam' }))
    expect(library.deleteLora).not.toHaveBeenCalled()
    confirm.mockReturnValue(true)
    fireEvent.click(within(adapterRow('maqam')).getByRole('button', { name: 'Delete adapter maqam' }))
    expect(library.deleteLora).toHaveBeenCalledWith('medium/maqam')
    confirm.mockRestore()
  })

  it('closes on Escape and restores focus to its trigger', () => {
    render(<Harness />)
    const { trigger } = openPanel()
    fireEvent.keyDown(document, { key: 'Escape' })
    expect(screen.queryByRole('dialog', { name: 'LoRA adapters' })).toBeNull()
    expect(document.activeElement).toBe(trigger)
  })
})
