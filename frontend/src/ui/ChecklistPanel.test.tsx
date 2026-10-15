import { useRef } from 'react'
import { fireEvent, render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import { ChecklistPanel, type ChecklistPanelProps } from './ChecklistPanel'

const OPTIONS = [
  { value: 'banger', label: 'banger', checked: true },
  { value: 'drone', label: 'drone', checked: false },
]

function Harness(props: Partial<Omit<ChecklistPanelProps, 'anchorRef'>>) {
  const anchorRef = useRef<HTMLButtonElement>(null)
  return (
    <>
      <button ref={anchorRef}>anchor</button>
      <ChecklistPanel
        open
        anchorRef={anchorRef}
        onClose={vi.fn()}
        label="Tags for Dub #3"
        options={OPTIONS}
        onToggle={vi.fn()}
        {...props}
      />
    </>
  )
}

describe('ChecklistPanel', () => {
  it('renders nothing when closed', () => {
    render(<Harness open={false} />)
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
    expect(screen.queryByRole('checkbox')).not.toBeInTheDocument()
  })

  it('renders options as labelled checkboxes with their checked state', () => {
    render(<Harness />)
    expect(screen.getByRole('dialog', { name: 'Tags for Dub #3' })).toBeInTheDocument()
    expect(screen.getByRole('checkbox', { name: 'banger' })).toBeChecked()
    expect(screen.getByRole('checkbox', { name: 'drone' })).not.toBeChecked()
    expect(screen.getByRole('checkbox', { name: 'banger' })).toHaveFocus()
  })

  it('calls onToggle with the option value', () => {
    const onToggle = vi.fn()
    render(<Harness onToggle={onToggle} />)
    fireEvent.click(screen.getByRole('checkbox', { name: 'drone' }))
    expect(onToggle).toHaveBeenCalledWith('drone')
  })

  it('adds trimmed text on Enter, clears the field and stays open', () => {
    const onAdd = vi.fn()
    render(<Harness add={{ label: 'Add a tag', placeholder: 'Add a tag…', onAdd }} />)
    const field = screen.getByRole('textbox', { name: 'Add a tag' })
    fireEvent.change(field, { target: { value: '  deep house  ' } })
    fireEvent.keyDown(field, { key: 'Enter' })
    expect(onAdd).toHaveBeenCalledWith('deep house')
    expect(field).toHaveValue('')
    expect(screen.getByRole('dialog')).toBeInTheDocument()
  })

  it('ignores empty or whitespace-only text', () => {
    const onAdd = vi.fn()
    render(<Harness add={{ label: 'Add a tag', onAdd }} />)
    const field = screen.getByRole('textbox', { name: 'Add a tag' })
    fireEvent.keyDown(field, { key: 'Enter' })
    fireEvent.change(field, { target: { value: '   ' } })
    fireEvent.keyDown(field, { key: 'Enter' })
    expect(onAdd).not.toHaveBeenCalled()
  })

  it('shows emptyText and focuses the add field when there are no options', () => {
    render(<Harness options={[]} emptyText="No tags yet" add={{ label: 'Add a tag', onAdd: vi.fn() }} />)
    expect(screen.getByText('No tags yet')).toBeInTheDocument()
    expect(screen.queryByRole('checkbox')).not.toBeInTheDocument()
    expect(screen.getByRole('textbox', { name: 'Add a tag' })).toHaveFocus()
  })

  it("focuses the add field over the options with initialFocus 'add'", () => {
    render(<Harness initialFocus="add" add={{ label: 'Add a tag', onAdd: vi.fn() }} />)
    expect(screen.getByRole('checkbox', { name: 'banger' })).toBeInTheDocument()
    expect(screen.getByRole('textbox', { name: 'Add a tag' })).toHaveFocus()
  })
})
