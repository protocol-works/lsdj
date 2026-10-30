import { act, fireEvent, render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import { LoomBar, type LoomBarProps } from './LoomBar'

const barProps = (overrides: Partial<LoomBarProps> = {}): LoomBarProps => ({
  name: 'Late dub',
  createdAt: 0,
  currentId: 'loom-a',
  saved: 'saved',
  status: null,
  looms: [
    { id: 'loom-a', name: 'Late dub', updatedAt: 2, nodes: 11 },
    { id: 'loom-b', name: 'Warm-up', updatedAt: 1, nodes: 1 },
  ],
  onRename: vi.fn(),
  onNew: vi.fn(),
  onOpen: vi.fn(),
  onDuplicate: vi.fn(),
  onDelete: vi.fn(),
  explorerOpen: false,
  onToggleExplorer: vi.fn(),
  toolsRef: vi.fn(),
  playing: false,
  bpm: 124,
  threads: 11,
  weaving: 0,
  ...overrides,
})

function renderBar(overrides: Partial<LoomBarProps> = {}) {
  const props = barProps(overrides)
  render(<LoomBar {...props} />)
  return props
}

const menu = (item: string) => {
  fireEvent.click(screen.getByRole('button', { name: 'Loom menu' }))
  fireEvent.click(screen.getByRole('menuitem', { name: item }))
}

describe('LoomBar', () => {
  it('renames on Enter and puts a blank name back on blur', () => {
    const props = renderBar()
    const field = screen.getByRole('textbox', { name: 'Loom name' })
    fireEvent.change(field, { target: { value: 'Sunrise set' } })
    fireEvent.keyDown(field, { key: 'Enter' })
    fireEvent.blur(field)
    expect(props.onRename).toHaveBeenCalledWith('Sunrise set')

    fireEvent.change(field, { target: { value: '   ' } })
    fireEvent.blur(field)
    expect(props.onRename).toHaveBeenCalledTimes(1)
    expect(field).toHaveValue('Late dub')
  })

  it('shows the saved mark and the status line', () => {
    renderBar({ saved: 'saving', status: 'Autosave failed: disk full' })
    expect(screen.getByText('Saving…')).toBeInTheDocument()
    expect(screen.getByRole('status')).toHaveTextContent('Autosave failed: disk full')
  })

  it('opens another loom from the list, not the one already open', () => {
    const props = renderBar()
    menu('Open…')
    expect(screen.queryByRole('menuitem', { name: /Late dub/ })).toBeNull()
    fireEvent.click(screen.getByRole('menuitem', { name: 'Warm-up · 1 node' }))
    expect(props.onOpen).toHaveBeenCalledWith('loom-b')
  })

  it('deletes only after the confirm step', () => {
    const props = renderBar()
    menu('Delete…')
    expect(screen.getByText('Delete “Late dub”? Its songs stay in the library.')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }))
    expect(props.onDelete).not.toHaveBeenCalled()

    menu('Delete…')
    fireEvent.click(screen.getByRole('button', { name: 'Delete' }))
    expect(props.onDelete).toHaveBeenCalledTimes(1)
  })

  it('starts and duplicates looms from the menu', () => {
    const props = renderBar()
    menu('New loom')
    menu('Duplicate')
    expect(props.onNew).toHaveBeenCalledTimes(1)
    expect(props.onDuplicate).toHaveBeenCalledTimes(1)
  })

  it('shows the master tempo and node counts, and toggles the explorer', () => {
    const props = renderBar({ bpm: 128.04, threads: 1, weaving: 2, explorerOpen: true })
    expect(screen.getByText(/master/)).toHaveTextContent('00:00 · master 128.0 bpm · 1 thread · 2 weaving')
    const explorer = screen.getByRole('button', { name: '☰ explorer' })
    expect(explorer).toHaveAttribute('aria-pressed', 'true')
    fireEvent.click(explorer)
    expect(props.onToggleExplorer).toHaveBeenCalledTimes(1)
  })

  it('counts the clock from the first play and on through a pause', () => {
    vi.useFakeTimers()
    try {
      const { rerender } = render(<LoomBar {...barProps()} />)
      act(() => vi.advanceTimersByTime(5_000))
      expect(screen.getByText('00:00')).toBeInTheDocument()

      rerender(<LoomBar {...barProps({ playing: true })} />)
      act(() => vi.advanceTimersByTime(65_000))
      rerender(<LoomBar {...barProps({ playing: false })} />)
      act(() => vi.advanceTimersByTime(2_000))
      expect(screen.getByText('01:07')).toBeInTheDocument()
    } finally {
      vi.useRealTimers()
    }
  })
})
