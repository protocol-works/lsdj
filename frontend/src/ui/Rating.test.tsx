import { fireEvent, render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import { Rating } from './Rating'

const starLabel = (count: number) => (count === 1 ? '1 star' : `${count} stars`)

function renderRating(value: number, onChange = vi.fn()) {
  render(
    <Rating
      value={value}
      onChange={onChange}
      label="Rating for Dub #3"
      starLabel={starLabel}
      clearHint="Click the current star to clear"
    />,
  )
  return onChange
}

describe('Rating', () => {
  it('is a labelled radiogroup carrying the clear hint', () => {
    renderRating(0)
    const group = screen.getByRole('radiogroup', { name: 'Rating for Dub #3' })
    expect(group).toHaveAttribute('title', 'Click the current star to clear')
  })

  it('renders five labelled radios with the current value checked', () => {
    renderRating(3)
    const radios = screen.getAllByRole('radio')
    expect(radios.map((radio) => radio.getAttribute('aria-label'))).toEqual([
      '1 star',
      '2 stars',
      '3 stars',
      '4 stars',
      '5 stars',
    ])
    expect(screen.getByRole('radio', { name: '3 stars' })).toBeChecked()
    expect(radios.filter((radio) => (radio as HTMLInputElement).checked)).toHaveLength(1)
  })

  it('lights the stars up to the value', () => {
    const { container } = render(
      <Rating value={2} onChange={vi.fn()} label="Rating" starLabel={starLabel} />,
    )
    expect(container.querySelectorAll('.ui-rating__star--on')).toHaveLength(2)
  })

  it('leaves every radio unchecked when unrated', () => {
    renderRating(0)
    for (const radio of screen.getAllByRole('radio')) expect(radio).not.toBeChecked()
  })

  it('sets the clicked star', () => {
    const onChange = renderRating(2)
    fireEvent.click(screen.getByRole('radio', { name: '4 stars' }))
    expect(onChange).toHaveBeenCalledTimes(1)
    expect(onChange).toHaveBeenCalledWith(4)
  })

  it('clears to 0 when the current star is clicked', () => {
    const onChange = renderRating(3)
    fireEvent.click(screen.getByRole('radio', { name: '3 stars' }))
    expect(onChange).toHaveBeenCalledTimes(1)
    expect(onChange).toHaveBeenCalledWith(0)
  })

  it('shares one radio name, so the browser moves ArrowRight from star 2 to star 3', () => {
    // jsdom does not emulate native arrow-key radio movement. Assert the
    // grouping that gives it to us, then replay what the browser does: check
    // the next radio and fire its click.
    const onChange = renderRating(2)
    const radios = screen.getAllByRole('radio') as HTMLInputElement[]
    expect(new Set(radios.map((radio) => radio.name)).size).toBe(1)
    radios[1].focus()
    fireEvent.keyDown(radios[1], { key: 'ArrowRight' })
    fireEvent.click(radios[2])
    expect(onChange).toHaveBeenCalledWith(3)
    expect(onChange).not.toHaveBeenCalledWith(0)
  })
})
