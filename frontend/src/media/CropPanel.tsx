import { useEffect, useRef, useState } from 'react'
import type { KeyboardEvent as ReactKeyboardEvent, PointerEvent as ReactPointerEvent, RefObject } from 'react'
import { useTranslation } from 'react-i18next'

import { invoke } from '../audio/nativeEngine'
import { AnchoredPanel } from '../ui/AnchoredPanel'
import { Button } from '../ui/Button'
import { TextField } from '../ui/TextField'
import { TRACK_OVERVIEW_BUCKETS, TrackOverview } from '../ui/TrackOverview'
import { moveEdge, trimSilence, type Peaks } from './crop'

// Arrow keys on a handle nudge by 10 ms (Shift: 100 ms) — fine enough to land a cut.
const NUDGE_SECONDS = 0.01
const NUDGE_COARSE_SECONDS = 0.1

type Edge = 'start' | 'end'

/** Crop a library clip into a new file (the row ⋯ → Crop…): its waveform with
 * draggable start/end handles and fields, a preview of just the selection,
 * Auto-trim of leading/trailing near-silence, and Save. The shell writes the
 * new file and row; the original is never touched. */
export function CropPanel({
  library,
  file,
  name,
  anchorRef,
  onClose,
  onPreview,
  onStopPreview,
  onSaved,
}: {
  library: 'songs' | 'samples'
  /** The row's file — the registry identity the shell crops. */
  file: string
  /** The row's display label. */
  name: string
  anchorRef: RefObject<HTMLElement | null>
  onClose: () => void
  onPreview: (wav: ArrayBuffer) => Promise<void>
  onStopPreview: () => void
  onSaved: () => void
}) {
  const { t } = useTranslation()
  const [peaks, setPeaks] = useState<Peaks | null>(null)
  const [duration, setDuration] = useState(0)
  const [crop, setCrop] = useState({ start: 0, end: 0 })
  const [previewing, setPreviewing] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const waveRef = useRef<HTMLDivElement>(null)
  const previewingRef = useRef(false)

  useEffect(() => {
    let live = true
    invoke<{ min: number[]; max: number[]; duration: number }>('library_clip_peaks', {
      library,
      name: file,
      buckets: TRACK_OVERVIEW_BUCKETS,
    })
      .then((fetched) => {
        if (!live) return
        setPeaks({ min: Float32Array.from(fetched.min), max: Float32Array.from(fetched.max) })
        setDuration(fetched.duration)
        setCrop({ start: 0, end: fetched.duration })
      })
      .catch((cause) => live && setError(message(cause)))
    return () => {
      live = false
    }
  }, [library, file])

  // Never leave the selection ringing in the phones after the panel closes. Refs,
  // so the unmount-only cleanup sees the latest state and callback.
  const stopPreviewRef = useRef(onStopPreview)
  useEffect(() => {
    previewingRef.current = previewing
    stopPreviewRef.current = onStopPreview
  })
  useEffect(
    () => () => {
      if (previewingRef.current) stopPreviewRef.current()
    },
    [],
  )

  function move(edge: Edge, seconds: number) {
    setCrop((current) => moveEdge(current, edge, seconds, duration))
  }

  function secondsAt(clientX: number): number {
    const box = waveRef.current?.getBoundingClientRect()
    if (!box || box.width === 0) return 0
    return ((clientX - box.left) / box.width) * duration
  }

  function handle(edge: Edge) {
    const value = crop[edge]
    return (
      <div
        className={`media__crop-handle media__crop-handle--${edge}`}
        role="slider"
        tabIndex={0}
        aria-label={t(`media.crop.${edge}Handle`)}
        aria-valuemin={0}
        aria-valuemax={duration}
        aria-valuenow={value}
        style={{ left: `${duration > 0 ? (value / duration) * 100 : 0}%` }}
        onPointerDown={(event: ReactPointerEvent<HTMLDivElement>) => {
          event.currentTarget.setPointerCapture?.(event.pointerId)
        }}
        onPointerMove={(event: ReactPointerEvent<HTMLDivElement>) => {
          if (event.currentTarget.hasPointerCapture?.(event.pointerId)) {
            move(edge, secondsAt(event.clientX))
          }
        }}
        onKeyDown={(event: ReactKeyboardEvent<HTMLDivElement>) => {
          const step = event.shiftKey ? NUDGE_COARSE_SECONDS : NUDGE_SECONDS
          const delta = { ArrowLeft: -step, ArrowRight: step }[event.key as string]
          if (delta === undefined) return
          event.preventDefault()
          move(edge, value + delta)
        }}
      />
    )
  }

  async function togglePreview() {
    if (previewing) {
      onStopPreview()
      setPreviewing(false)
      return
    }
    setError(null)
    try {
      const wav = await invoke<ArrayBuffer>('library_crop_preview', {
        library,
        name: file,
        start: crop.start,
        end: crop.end,
      })
      await onPreview(wav)
      setPreviewing(true)
    } catch (cause) {
      setError(message(cause))
    }
  }

  function autoTrim() {
    if (!peaks) return
    const trimmed = trimSilence(peaks, duration)
    if (trimmed) setCrop(trimmed)
    else setError(t('media.crop.allSilent'))
  }

  async function save() {
    setBusy(true)
    setError(null)
    try {
      await invoke(library === 'songs' ? 'crop_generated_song' : 'crop_generated_sample', {
        name: file,
        start: crop.start,
        end: crop.end,
      })
      onSaved()
      onClose()
    } catch (cause) {
      setError(t('media.crop.failed', { title: name, message: message(cause) }))
      setBusy(false)
    }
  }

  const ready = peaks != null && duration > 0
  return (
    <AnchoredPanel
      open
      anchorRef={anchorRef}
      onClose={onClose}
      label={t('media.crop.label', { name })}
      className="media__crop"
    >
      <p className="media__crop-title">{t('media.crop.label', { name })}</p>
      {ready ? (
        <div ref={waveRef} className="media__crop-wave">
          <TrackOverview
            label={t('media.crop.waveform', { name })}
            peaks={peaks}
            position={crop.start}
            duration={duration}
            loop={crop}
            accent="a"
            // A click on the waveform moves the nearer handle there.
            onSeek={(seconds) =>
              move(
                Math.abs(seconds - crop.start) <= Math.abs(seconds - crop.end) ? 'start' : 'end',
                seconds,
              )
            }
          />
          {handle('start')}
          {handle('end')}
        </div>
      ) : (
        !error && <p className="media__empty">{t('media.crop.loading')}</p>
      )}
      {ready && (
        <div className="media__crop-fields">
          {(['start', 'end'] as const).map((edge) => (
            <TextField
              key={edge}
              type="number"
              min={0}
              max={duration}
              step={0.01}
              label={t(`media.crop.${edge}`)}
              value={crop[edge]}
              onChange={(event) => {
                if (event.target.value !== '') move(edge, Number(event.target.value))
              }}
            />
          ))}
          <span className="media__meta">
            {t('media.crop.length', { seconds: (crop.end - crop.start).toFixed(2) })}
          </span>
        </div>
      )}
      {error && (
        <p className="media__error" role="alert">
          {error}
        </p>
      )}
      <div className="media__crop-actions">
        <Button disabled={!ready} onClick={() => void togglePreview()}>
          {t(previewing ? 'media.crop.stop' : 'media.crop.preview')}
        </Button>
        <Button disabled={!ready} onClick={autoTrim}>
          {t('media.crop.autoTrim')}
        </Button>
        <Button disabled={!ready || busy} onClick={() => void save()}>
          {t('media.crop.save')}
        </Button>
        <Button onClick={onClose}>{t('media.crop.cancel')}</Button>
      </div>
    </AnchoredPanel>
  )
}

function message(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause)
}
