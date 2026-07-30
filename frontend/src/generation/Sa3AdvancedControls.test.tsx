import { fireEvent, render, screen } from '@testing-library/react'
import { useState } from 'react'
import { describe, expect, it, vi } from 'vitest'

import { DEFAULT_SA3_STEERING, type Sa3SteeringDraft } from './songGeneration'
import { Sa3AdvancedControls } from './Sa3AdvancedControls'

function Fixture() {
  const [value, setValue] = useState<Sa3SteeringDraft>({
    ...DEFAULT_SA3_STEERING,
    seed: { mode: 'random' },
  })
  return (
    <Sa3AdvancedControls
      value={value}
      onChange={setValue}
      onSubmit={vi.fn()}
    />
  )
}

describe('Sa3AdvancedControls', () => {
  it('activates guidance with an Avoid chip and validates a fixed seed', () => {
    render(<Fixture />)
    expect(screen.getByRole('switch', { name: 'Guidance: Off' })).toBeInTheDocument()
    expect(screen.getByLabelText('Avoid concepts')).toBeDisabled()
    expect(screen.getByRole('button', { name: 'No drums' })).toBeDisabled()
    const cfgHelp = screen.getByRole('button', {
      name: 'Explain Classifier-Free Guidance',
    })
    expect(cfgHelp).toBeEnabled()
    expect(cfgHelp).toHaveAccessibleDescription(/CFG as the accelerator/)
    const apgHelp = screen.getByRole('button', {
      name: 'Explain Adaptive Projected Guidance',
    })
    expect(apgHelp).toBeEnabled()
    expect(apgHelp).toHaveAccessibleDescription(/APG as traction control/)
    expect(screen.getByLabelText(/Classifier-Free Guidance.*3.0/)).toBeDisabled()

    fireEvent.click(screen.getByRole('switch', { name: 'Guidance: Off' }))
    expect(screen.getByLabelText('Avoid concepts')).toBeEnabled()
    fireEvent.click(screen.getByRole('button', { name: 'No drums' }))
    expect(screen.getByLabelText('Avoid concepts')).toHaveValue('drums')
    const guidance = screen.getByRole('switch', { name: 'Guidance: On' })
    expect(guidance).toBeEnabled()
    expect(screen.getByLabelText(/Classifier-Free Guidance.*3.0/)).toBeEnabled()
    expect(screen.getByLabelText(/Classifier-Free Guidance.*3.0/)).toHaveAttribute(
      'max',
      '4',
    )
    expect(screen.getByLabelText(/Adaptive Projected Guidance.*1.0/)).toBeEnabled()

    fireEvent.click(guidance)
    expect(screen.getByRole('switch', { name: 'Guidance: Off' })).toBeEnabled()
    expect(screen.getByLabelText('Avoid concepts')).toHaveValue('drums')
    expect(screen.getByLabelText('Avoid concepts')).toBeDisabled()
    expect(screen.getByRole('button', { name: 'No drums' })).toBeDisabled()
    expect(screen.getByLabelText(/Classifier-Free Guidance.*3.0/)).toBeDisabled()
    expect(screen.getByLabelText(/Adaptive Projected Guidance.*1.0/)).toBeDisabled()
    expect(cfgHelp).toBeEnabled()
    expect(apgHelp).toBeEnabled()

    fireEvent.click(screen.getByRole('switch', { name: 'Guidance: Off' }))
    expect(screen.getByRole('switch', { name: 'Guidance: On' })).toBeEnabled()
    expect(screen.getByLabelText('Avoid concepts')).toHaveValue('drums')
    expect(screen.getByLabelText('Avoid concepts')).toBeEnabled()

    fireEvent.change(screen.getByLabelText('Seed behavior'), {
      target: { value: 'fixed' },
    })
    expect(screen.getByRole('alert')).toHaveTextContent('whole-number seed')
    expect(screen.queryByRole('button', { name: 'Reset advanced' })).toBeNull()
  })
})
