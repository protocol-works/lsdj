import { fireEvent, render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import { ActionMenu } from './ActionMenu'

function renderMenu() {
  const rename = vi.fn()
  const tags = vi.fn()
  const remove = vi.fn()
  render(
    <ActionMenu
      label="Actions for Dub #3"
      items={[
        { label: 'Rename', onSelect: rename },
        { label: 'Tags', onSelect: tags, disabled: true },
        { label: 'Delete', onSelect: remove, danger: true },
      ]}
    />,
  )
  return { rename, tags, remove, trigger: screen.getByRole('button', { name: 'Actions for Dub #3' }) }
}

describe('ActionMenu', () => {
  it('is closed by default', () => {
    const { trigger } = renderMenu()
    expect(trigger).toHaveAttribute('aria-haspopup', 'menu')
    expect(trigger).toHaveAttribute('aria-expanded', 'false')
    expect(screen.queryByRole('menu')).not.toBeInTheDocument()
  })

  it('opens on click with focus on the first enabled item', () => {
    const { trigger } = renderMenu()
    fireEvent.click(trigger)
    expect(trigger).toHaveAttribute('aria-expanded', 'true')
    expect(screen.getByRole('menu', { name: 'Actions for Dub #3' })).toBeInTheDocument()
    expect(screen.getByRole('menuitem', { name: 'Rename' })).toHaveFocus()
  })

  it('wraps ArrowDown/ArrowUp and skips the disabled item', () => {
    const { trigger } = renderMenu()
    fireEvent.click(trigger)
    const menu = screen.getByRole('menu')
    const rename = screen.getByRole('menuitem', { name: 'Rename' })
    const remove = screen.getByRole('menuitem', { name: 'Delete' })
    fireEvent.keyDown(menu, { key: 'ArrowDown' })
    expect(remove).toHaveFocus()
    fireEvent.keyDown(menu, { key: 'ArrowDown' })
    expect(rename).toHaveFocus()
    fireEvent.keyDown(menu, { key: 'ArrowUp' })
    expect(remove).toHaveFocus()
    fireEvent.keyDown(menu, { key: 'Home' })
    expect(rename).toHaveFocus()
    fireEvent.keyDown(menu, { key: 'End' })
    expect(remove).toHaveFocus()
  })

  it('selects on Enter: closes, then calls onSelect', () => {
    const { trigger, rename } = renderMenu()
    fireEvent.click(trigger)
    fireEvent.keyDown(screen.getByRole('menuitem', { name: 'Rename' }), { key: 'Enter' })
    expect(rename).toHaveBeenCalledTimes(1)
    expect(screen.queryByRole('menu')).not.toBeInTheDocument()
    expect(trigger).toHaveAttribute('aria-expanded', 'false')
  })

  it('closes on Escape and returns focus to the trigger', () => {
    const { trigger, rename } = renderMenu()
    fireEvent.click(trigger)
    fireEvent.keyDown(document.activeElement!, { key: 'Escape' })
    expect(screen.queryByRole('menu')).not.toBeInTheDocument()
    expect(trigger).toHaveFocus()
    expect(rename).not.toHaveBeenCalled()
  })

  it('does not select a disabled item', () => {
    const { trigger, tags } = renderMenu()
    fireEvent.click(trigger)
    const disabled = screen.getByRole('menuitem', { name: 'Tags' })
    expect(disabled).toBeDisabled()
    fireEvent.click(disabled)
    expect(tags).not.toHaveBeenCalled()
    expect(screen.getByRole('menu')).toBeInTheDocument()
  })

  it('marks a danger item with its modifier class', () => {
    const { trigger } = renderMenu()
    fireEvent.click(trigger)
    expect(screen.getByRole('menuitem', { name: 'Delete' })).toHaveClass('ui-action-menu__item--danger')
    expect(screen.getByRole('menuitem', { name: 'Rename' })).not.toHaveClass(
      'ui-action-menu__item--danger',
    )
  })
})
