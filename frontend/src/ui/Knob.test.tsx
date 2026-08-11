import { fireEvent, render } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import { Knob } from './Knob'

const CENTRE = 22 // SIZE / 2

function pointerTip(container: HTMLElement) {
  const line = container.querySelector('.ui-knob__pointer')
  if (!line) throw new Error('pointer line not rendered')
  return {
    x: Number(line.getAttribute('x2')),
    y: Number(line.getAttribute('y2')),
  }
}

describe('Knob', () => {
  // Regression for the inverted dial geometry (the value arc swept
  // counter-clockwise from the wrong anchor): min must point to 7 o'clock,
  // max to 5 o'clock, sweeping clockwise over the top.
  it("points the dial at 7 o'clock at minimum", () => {
    const { container } = render(
      <Knob label="EQ Low" value={0} onChange={() => {}} />,
    )
    const tip = pointerTip(container)
    expect(tip.x).toBeLessThan(CENTRE)
    expect(tip.y).toBeGreaterThan(CENTRE)
  })

  it('points the dial straight up at centre', () => {
    const { container } = render(
      <Knob label="EQ Low" value={0.5} onChange={() => {}} />,
    )
    const tip = pointerTip(container)
    expect(tip.x).toBeCloseTo(CENTRE, 5)
    expect(tip.y).toBeLessThan(CENTRE)
  })

  it("points the dial at 5 o'clock at maximum", () => {
    const { container } = render(
      <Knob label="EQ Low" value={1} onChange={() => {}} />,
    )
    const tip = pointerTip(container)
    expect(tip.x).toBeGreaterThan(CENTRE)
    expect(tip.y).toBeGreaterThan(CENTRE)
  })

  it('renders finite SVG geometry when max equals min', () => {
    const { container } = render(
      <Knob label="EQ Low" value={1} min={1} max={1} onChange={() => {}} />,
    )
    const value = container.querySelector('.ui-knob__value')
    expect(value?.getAttribute('d')).not.toContain('NaN')
  })

  it('drives onChange through the native range input', () => {
    const onChange = vi.fn()
    const { getByLabelText } = render(
      <Knob label="EQ Low" value={0.5} onChange={onChange} />,
    )
    fireEvent.change(getByLabelText('EQ Low'), { target: { value: '0.8' } })
    expect(onChange).toHaveBeenCalledWith(0.8)
  })

  it('uses a precise relative pointer drag independent of the dial width', () => {
    const onChange = vi.fn()
    const { container } = render(
      <Knob
        label="LoRA strength"
        value={1}
        min={0}
        max={2}
        step={0.25}
        size="s"
        onChange={onChange}
      />,
    )
    const dial = container.querySelector('.ui-knob__dial')
    if (!(dial instanceof HTMLElement)) throw new Error('dial not rendered')

    fireEvent.pointerDown(dial, {
      pointerId: 1,
      pointerType: 'mouse',
      button: 0,
      clientY: 100,
    })
    // Four pixels used to cover more than one LoRA step on the 28 px native
    // range. It should now be too small to move the value at all.
    fireEvent.pointerMove(dial, { pointerId: 1, clientY: 96 })
    expect(onChange).not.toHaveBeenCalled()

    fireEvent.pointerMove(dial, { pointerId: 1, clientY: 80 })
    fireEvent.pointerMove(dial, { pointerId: 1, clientY: 60 })
    expect(onChange.mock.calls).toEqual([[1.25], [1.5]])

    fireEvent.pointerUp(dial, { pointerId: 1 })
    fireEvent.pointerMove(dial, { pointerId: 1, clientY: 20 })
    expect(onChange).toHaveBeenCalledTimes(2)
  })

  it('resets to the centre of the range on double-click', () => {
    const onChange = vi.fn()
    const { getByLabelText } = render(
      <Knob label="EQ Low" value={0.9} onChange={onChange} />,
    )
    fireEvent.doubleClick(getByLabelText('EQ Low'))
    expect(onChange).toHaveBeenCalledWith(0.5)
  })

  it('parks at resetValue on double-click when one is given', () => {
    const onChange = vi.fn()
    const { getByLabelText } = render(
      <Knob label="FX amount" value={0.9} resetValue={0} onChange={onChange} />,
    )
    fireEvent.doubleClick(getByLabelText('FX amount'))
    expect(onChange).toHaveBeenCalledWith(0)
  })
})
