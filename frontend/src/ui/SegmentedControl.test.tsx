import { fireEvent, render, screen } from '@testing-library/react'
import { useState } from 'react'
import { describe, expect, it } from 'vitest'

import { SegmentedControl } from './SegmentedControl'

function Fixture() {
  const [value, setValue] = useState<'basic' | 'advanced'>('basic')
  return (
    <SegmentedControl
      label="Generation mode"
      value={value}
      options={[
        { value: 'basic', label: 'Basic' },
        { value: 'advanced', label: 'Advanced' },
      ]}
      onChange={setValue}
    />
  )
}

describe('SegmentedControl', () => {
  it('exposes one radio choice and supports arrow-key selection', () => {
    render(<Fixture />)
    const basic = screen.getByRole('radio', { name: 'Basic' })
    const advanced = screen.getByRole('radio', { name: 'Advanced' })
    expect(basic).toHaveAttribute('aria-checked', 'true')
    expect(basic).toHaveAttribute('tabindex', '0')
    expect(advanced).toHaveAttribute('tabindex', '-1')

    fireEvent.keyDown(basic, { key: 'ArrowRight' })
    expect(advanced).toHaveAttribute('aria-checked', 'true')
    expect(advanced).toHaveFocus()
  })

  it('removes every option from interaction when disabled', () => {
    render(
      <SegmentedControl
        label="Mode"
        value="one"
        options={[
          { value: 'one', label: 'One' },
          { value: 'two', label: 'Two' },
        ]}
        disabled
        onChange={() => {}}
      />,
    )
    expect(screen.getByRole('radio', { name: 'One' })).toBeDisabled()
    expect(screen.getByRole('radio', { name: 'Two' })).toHaveAttribute('tabindex', '-1')
  })
})
