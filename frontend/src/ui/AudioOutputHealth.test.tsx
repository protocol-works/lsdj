import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import {
  getAudioOutputHealth,
  reconnectAudioOutputs,
  type AudioOutputHealth as Health,
} from '../audio/nativeEngine'
import { AudioOutputHealth } from './AudioOutputHealth'

vi.mock('../audio/nativeEngine', () => ({
  getAudioOutputHealth: vi.fn(),
  reconnectAudioOutputs: vi.fn(),
}))

const HEALTHY: Health = {
  mainHealthy: true,
  cueHealthy: true,
  mainError: null,
  cueError: null,
  canReconnect: false,
}

beforeEach(() => {
  vi.mocked(getAudioOutputHealth).mockReset()
  vi.mocked(reconnectAudioOutputs).mockReset()
})

describe('AudioOutputHealth', () => {
  it('projects a live healthy snapshot', async () => {
    vi.mocked(getAudioOutputHealth).mockResolvedValue(HEALTHY)

    render(<AudioOutputHealth />)

    expect(await screen.findByRole('status')).toHaveTextContent(
      'Main and cue outputs are healthy.',
    )
  })

  it('shows the failed route and reconnects it explicitly', async () => {
    const failed: Health = {
      mainHealthy: true,
      cueHealthy: false,
      mainError: null,
      cueError: 'The cue audio stream stopped after it started.',
      canReconnect: true,
    }
    vi.mocked(getAudioOutputHealth).mockResolvedValue(failed)
    vi.mocked(reconnectAudioOutputs).mockResolvedValue(HEALTHY)

    render(<AudioOutputHealth />)

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'The cue audio stream stopped after it started.',
    )
    fireEvent.click(screen.getByRole('button', { name: 'Reconnect audio' }))

    expect(reconnectAudioOutputs).toHaveBeenCalledTimes(1)
    await waitFor(() =>
      expect(screen.getByRole('status')).toHaveTextContent(
        'Main and cue outputs are healthy.',
      ),
    )
  })

  it('does not offer a retry when a route change is required', async () => {
    vi.mocked(getAudioOutputHealth).mockResolvedValue({
      mainHealthy: true,
      cueHealthy: false,
      mainError: null,
      cueError: 'Phones on main require an output with channels 3/4.',
      canReconnect: false,
    })

    render(<AudioOutputHealth />)

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'Phones on main require an output with channels 3/4.',
    )
    expect(screen.queryByRole('button', { name: 'Reconnect audio' })).toBeNull()
  })

  it('keeps a reconnect failure visible after refreshing partial health', async () => {
    const failed: Health = {
      mainHealthy: false,
      cueHealthy: true,
      mainError: 'The main audio stream stopped after it started.',
      cueError: null,
      canReconnect: true,
    }
    vi.mocked(getAudioOutputHealth).mockResolvedValue(failed)
    vi.mocked(reconnectAudioOutputs).mockRejectedValue(new Error('device unplugged'))

    render(<AudioOutputHealth />)
    fireEvent.click(
      await screen.findByRole('button', { name: 'Reconnect audio' }),
    )

    await waitFor(() =>
      expect(screen.getByRole('alert')).toHaveTextContent('device unplugged'),
    )
    expect(getAudioOutputHealth).toHaveBeenCalledTimes(2)
  })
})
