import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { createControlBus, type ControlBus } from '../control/bus'
import { ControlBusProvider } from '../control/ControlBusProvider'
import { serialisePresets, type StylePreset } from '../presets'
import { CrateBrowser } from './CrateBrowser'

const FUNK: StylePreset = {
  name: 'Warm funk',
  targets: [{ text: 'warm disco funk', x: 0.5, y: 0.5 }],
  cursor: { x: 0.5, y: 0.5 },
  fx: { kind: null, amount: 0 },
}
const DUB: StylePreset = { ...FUNK, name: 'Dub session' }

function renderBrowser(
  presets: StylePreset[],
  handlers: {
    onLoad?: ReturnType<typeof vi.fn>
    onDelete?: ReturnType<typeof vi.fn>
    onImport?: ReturnType<typeof vi.fn>
  } = {},
  bus: ControlBus = createControlBus(),
  filter = '',
) {
  return render(
    <ControlBusProvider bus={bus}>
      <CrateBrowser
        presets={presets}
        onLoad={(handlers.onLoad ?? vi.fn()) as (deck: 'a' | 'b', preset: StylePreset) => void}
        onDelete={(handlers.onDelete ?? vi.fn()) as (name: string) => void}
        onImport={(handlers.onImport ?? vi.fn()) as (presets: StylePreset[]) => void}
        filter={filter}
      />
    </ControlBusProvider>,
  )
}

afterEach(() => vi.unstubAllGlobals())

describe('CrateBrowser', () => {
  it('shows the empty state and disables export', () => {
    renderBrowser([])
    expect(screen.getByText(/No presets yet/)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Export' })).toBeDisabled()
  })

  it('loads a preset to a deck from its row', () => {
    const onLoad = vi.fn()
    renderBrowser([FUNK, DUB], { onLoad })
    fireEvent.click(
      screen.getByRole('button', { name: 'Load Dub session to deck B' }),
    )
    expect(onLoad).toHaveBeenCalledWith('b', DUB)
  })

  it('filters presets by name and target text, including hardware loading', () => {
    const onLoad = vi.fn()
    const bus = createControlBus()
    renderBrowser([FUNK, DUB], { onLoad }, bus, 'DUB warm disco')

    expect(screen.queryByText('Warm funk')).toBeNull()
    expect(screen.getByText('Dub session')).toBeInTheDocument()
    act(() => bus.publish({ kind: 'browse_load', deck: 'b' }))
    expect(onLoad).toHaveBeenCalledWith('b', DUB)
  })

  it('distinguishes an empty filter result from an empty preset library', () => {
    renderBrowser([FUNK], {}, createControlBus(), 'techno')
    expect(screen.getByRole('status')).toHaveTextContent('No results for “techno”.')
    expect(screen.queryByText(/No presets yet/)).toBeNull()
    expect(screen.getByRole('button', { name: 'Export' })).toBeEnabled()
  })

  it('deletes a preset from its row', () => {
    const onDelete = vi.fn()
    renderBrowser([FUNK], { onDelete })
    fireEvent.click(
      screen.getByRole('button', { name: 'Remove preset Warm funk' }),
    )
    expect(onDelete).toHaveBeenCalledWith('Warm funk')
  })

  it('moves the highlight with the browse rotary and loads with LOAD', () => {
    const onLoad = vi.fn()
    const bus = createControlBus()
    renderBrowser([FUNK, DUB], { onLoad }, bus)
    expect(
      screen.getByRole('button', { name: 'Select preset Warm funk' }),
    ).toHaveAttribute('aria-current', 'true')

    act(() => bus.publish({ kind: 'browse_scroll', steps: 1 }))
    expect(
      screen.getByRole('button', { name: 'Select preset Dub session' }),
    ).toHaveAttribute('aria-current', 'true')
    // The end of the list clamps rather than wrapping.
    act(() => bus.publish({ kind: 'browse_scroll', steps: 1 }))
    expect(
      screen.getByRole('button', { name: 'Select preset Dub session' }),
    ).toHaveAttribute('aria-current', 'true')

    act(() => bus.publish({ kind: 'browse_load', deck: 'a' }))
    expect(onLoad).toHaveBeenCalledWith('a', DUB)
  })

  it('a fast multi-click tick moves the highlight by its magnitude', () => {
    const bus = createControlBus()
    renderBrowser([FUNK, DUB, { ...FUNK, name: 'Third' }], {}, bus)
    act(() => bus.publish({ kind: 'browse_scroll', steps: 2 }))
    expect(
      screen.getByRole('button', { name: 'Select preset Third' }),
    ).toHaveAttribute('aria-current', 'true')
    act(() => bus.publish({ kind: 'browse_scroll', steps: -2 }))
    expect(
      screen.getByRole('button', { name: 'Select preset Warm funk' }),
    ).toHaveAttribute('aria-current', 'true')
  })

  it('ignores hardware loads while the crate is empty', () => {
    const onLoad = vi.fn()
    const bus = createControlBus()
    renderBrowser([], { onLoad }, bus)
    act(() => bus.publish({ kind: 'browse_scroll', steps: 1 }))
    act(() => bus.publish({ kind: 'browse_load', deck: 'a' }))
    expect(onLoad).not.toHaveBeenCalled()
  })

  it('keeps a valid highlight after the list shrinks', () => {
    const onLoad = vi.fn()
    const bus = createControlBus()
    const { rerender } = render(
      <ControlBusProvider bus={bus}>
        <CrateBrowser presets={[FUNK, DUB]} onLoad={onLoad} onDelete={vi.fn()} onImport={vi.fn()} />
      </ControlBusProvider>,
    )
    act(() => bus.publish({ kind: 'browse_scroll', steps: 1 })) // → DUB
    rerender(
      <ControlBusProvider bus={bus}>
        <CrateBrowser presets={[FUNK]} onLoad={onLoad} onDelete={vi.fn()} onImport={vi.fn()} />
      </ControlBusProvider>,
    )
    act(() => bus.publish({ kind: 'browse_load', deck: 'b' }))
    expect(onLoad).toHaveBeenCalledWith('b', FUNK)
  })

  it('exports the crate as a JSON download', () => {
    const createObjectURL = vi.fn(() => 'blob:crates')
    vi.stubGlobal('URL', {
      ...URL,
      createObjectURL,
      revokeObjectURL: vi.fn(),
    })
    renderBrowser([FUNK])
    fireEvent.click(screen.getByRole('button', { name: 'Export' }))
    expect(createObjectURL).toHaveBeenCalledTimes(1)
  })

  it('imports a crates file and surfaces a bad one', async () => {
    const onImport = vi.fn()
    renderBrowser([], { onImport })
    const input = screen.getByLabelText('Presets file')

    const good = new File([serialisePresets([FUNK])], 'crates.json')
    fireEvent.change(input, { target: { files: [good] } })
    await waitFor(() => expect(onImport).toHaveBeenCalledWith([FUNK]))

    const bad = new File(['{nope'], 'crates.json')
    fireEvent.change(input, { target: { files: [bad] } })
    expect(
      await screen.findByText('Import failed: not a JSON file'),
    ).toBeInTheDocument()
  })
})
