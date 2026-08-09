import { useCallback, useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'

import {
  getAudioOutputHealth,
  reconnectAudioOutputs,
  type AudioOutputHealth as AudioOutputHealthState,
} from '../audio/nativeEngine'
import { Button } from './Button'

const HEALTH_POLL_MS = 1_500

function sameHealth(
  current: AudioOutputHealthState | null,
  next: AudioOutputHealthState,
): boolean {
  return (
    current?.mainHealthy === next.mainHealthy &&
    current.cueHealthy === next.cueHealthy &&
    current.mainError === next.mainError &&
    current.cueError === next.cueError &&
    current.canReconnect === next.canReconnect
  )
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

/** A small live status/recovery surface mounted only while Settings is open.
 * Polling is deliberately low-frequency and deduplicates equal snapshots, so
 * device health can change after launch without adding render churn elsewhere. */
export function AudioOutputHealth() {
  const { t } = useTranslation()
  const [health, setHealth] = useState<AudioOutputHealthState | null>(null)
  const [pollError, setPollError] = useState<string | null>(null)
  const [reconnecting, setReconnecting] = useState(false)

  const applyHealth = useCallback((next: AudioOutputHealthState) => {
    setHealth((current) => (sameHealth(current, next) ? current : next))
  }, [])

  useEffect(() => {
    let active = true
    const poll = () => {
      void getAudioOutputHealth().then(
        (next) => {
          if (active) {
            applyHealth(next)
            setPollError(null)
          }
        },
        (error: unknown) => {
          if (active) setPollError(message(error))
        },
      )
    }

    poll()
    const timer = window.setInterval(poll, HEALTH_POLL_MS)
    return () => {
      active = false
      window.clearInterval(timer)
    }
  }, [applyHealth])

  const reconnect = useCallback(async () => {
    setReconnecting(true)
    setPollError(null)
    try {
      applyHealth(await reconnectAudioOutputs())
      setPollError(null)
    } catch (error) {
      setPollError(message(error))
      // The command can recover one route while another fails. Refresh once so
      // that useful partial recovery is visible immediately.
      try {
        applyHealth(await getAudioOutputHealth())
      } catch {
        // Preserve the reconnect error; the bounded poll will try again later.
      }
    } finally {
      setReconnecting(false)
    }
  }, [applyHealth])

  if (!health && !pollError) {
    return (
      <p className="audio-health audio-health--checking" role="status">
        {t('mixer.outputChecking')}
      </p>
    )
  }

  if (health?.mainHealthy && health.cueHealthy && !pollError) {
    return (
      <p className="audio-health audio-health--healthy" role="status">
        {t('mixer.outputHealthy')}
      </p>
    )
  }

  return (
    <div className="audio-health audio-health--error" role="alert">
      <div className="audio-health__messages">
        {health && !health.mainHealthy && (
          <span>{health.mainError ?? t('mixer.outputMainUnavailable')}</span>
        )}
        {health && !health.cueHealthy && (
          <span>{health.cueError ?? t('mixer.outputCueUnavailable')}</span>
        )}
        {pollError && <span>{pollError}</span>}
      </div>
      {health?.canReconnect && (
        <Button onClick={() => void reconnect()} disabled={reconnecting}>
          {reconnecting ? t('mixer.outputReconnecting') : t('mixer.outputReconnect')}
        </Button>
      )}
    </div>
  )
}
