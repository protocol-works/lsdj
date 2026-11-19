import { fireEvent, render, screen } from '@testing-library/react'
import { useRef, useState } from 'react'
import { describe, expect, it, vi } from 'vitest'

import { AnchoredPanel } from './AnchoredPanel'

/** An outer panel whose body opens an inner panel — the weave form's LoRA rack. */
function Nested({ onCloseOuter }: { onCloseOuter: () => void }) {
  const outerAnchor = useRef<HTMLButtonElement>(null)
  const innerAnchor = useRef<HTMLButtonElement>(null)
  const [inner, setInner] = useState(false)
  return (
    <>
      <button ref={outerAnchor}>outer trigger</button>
      <p>elsewhere</p>
      <AnchoredPanel open anchorRef={outerAnchor} onClose={onCloseOuter} label="Outer">
        <button ref={innerAnchor} onClick={() => setInner(true)}>
          open inner
        </button>
      </AnchoredPanel>
      <AnchoredPanel open={inner} anchorRef={innerAnchor} onClose={() => setInner(false)} label="Inner">
        <button>inside inner</button>
      </AnchoredPanel>
    </>
  )
}

describe('AnchoredPanel', () => {
  it('a press inside a panel opened from it does not dismiss it', () => {
    const onCloseOuter = vi.fn()
    render(<Nested onCloseOuter={onCloseOuter} />)
    fireEvent.click(screen.getByRole('button', { name: 'open inner' }))

    fireEvent.pointerDown(screen.getByRole('button', { name: 'inside inner' }))

    expect(onCloseOuter).not.toHaveBeenCalled()
    expect(screen.getByRole('dialog', { name: 'Inner' })).toBeInTheDocument()
  })

  it('a press outside every panel still dismisses it', () => {
    const onCloseOuter = vi.fn()
    render(<Nested onCloseOuter={onCloseOuter} />)

    fireEvent.pointerDown(screen.getByText('elsewhere'))

    expect(onCloseOuter).toHaveBeenCalledTimes(1)
  })
})
