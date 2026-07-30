import { fireEvent, render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import { Slider } from './Slider'

describe('Slider', () => {
  it('reports the range value on change', () => {
    const onChange = vi.fn()
    const { container } = render(
      <Slider label="Temperature" min={0} max={3} step={0.01} value={1.1} onChange={onChange} />,
    )
    const input = container.querySelector('.ui-slider__input') as HTMLInputElement
    fireEvent.change(input, { target: { value: '2' } })
    expect(onChange).toHaveBeenCalledWith(2)
  })

  it('shows a reset control that fires onReset when provided', () => {
    const onReset = vi.fn()
    const { getByLabelText } = render(
      <Slider
        label="Temperature"
        min={0}
        max={3}
        step={0.01}
        value={1.1}
        onChange={() => {}}
        onReset={onReset}
        resetLabel="Reset Temperature to default"
      />,
    )
    fireEvent.click(getByLabelText('Reset Temperature to default'))
    expect(onReset).toHaveBeenCalledTimes(1)
  })

  it('renders no reset control without onReset', () => {
    const { container } = render(
      <Slider label="Temperature" min={0} max={3} step={0.01} value={1.1} onChange={() => {}} />,
    )
    expect(container.querySelector('.ui-slider__reset')).toBeNull()
  })

  it('keeps optional help focusable when the slider is disabled', () => {
    render(
      <Slider
        label="Guidance"
        help={{ label: 'Explain guidance', text: 'Higher follows the prompt more strongly.' }}
        min={1}
        max={4}
        step={0.1}
        value={3}
        disabled
        onChange={() => {}}
      />,
    )
    const trigger = screen.getByRole('button', { name: 'Explain guidance' })
    expect(trigger).toBeEnabled()
    expect(trigger).toHaveTextContent('?')
    expect(trigger).toHaveAccessibleDescription('Higher follows the prompt more strongly.')
    expect(screen.getByRole('tooltip')).toHaveTextContent(
      'Higher follows the prompt more strongly.',
    )
  })
})
