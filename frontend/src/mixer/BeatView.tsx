import { useTranslation } from 'react-i18next'

import { BAND_HOP_FRAMES } from '../audio/bands'
import { SAMPLE_RATE } from '../audio/types'
import { Panel } from '../ui/Panel'
import { ZoomStrip } from '../ui/ZoomStrip'
import type { ZoomGrab } from '../ui/ZoomStrip'
import { grabNudgeSeconds } from '../ui/zoomGrab'
import type { DeckControls, ZoomSource } from '../deck/useDeck'

/** What a grab needs from a deck. */
type GrabDeck = Pick<
  DeckControls,
  'mode' | 'track' | 'seekTrack' | 'nudgeTrackPhase'
>

const HOP_SECONDS = BAND_HOP_FRAMES / SAMPLE_RATE

/** The grab's meaning per deck state, like a platter: paused = cue by
 * dragging the waveform (a silent seek), playing = a beatmatching
 * touch (a gentle phase nudge through the jog's bend path, no scratch).
 * A live deck isn't grabbable at all (ADR-0004). */
function grabHandler(deck: GrabDeck | undefined) {
  if (deck?.mode !== 'playback' || !deck.track) return undefined
  const { playing, rate } = deck.track
  return (grab: ZoomGrab) => {
    if (playing) {
      const nudge = grabNudgeSeconds(
        grab.deltaHops * HOP_SECONDS,
        grab.elapsedSeconds,
        rate,
      )
      if (nudge !== 0) deck.nudgeTrackPhase(nudge)
    } else {
      deck.seekTrack((grab.startHop + grab.totalHops) * HOP_SECONDS)
    }
  }
}

type BeatViewProps = {
  /** Side-by-side vertical strips (time runs downward) instead of
   * the stacked horizontal pair. */
  vertical?: boolean
  getSourceA: () => ZoomSource | null
  getSourceB: () => ZoomSource | null
  /** Decks whose strips can be grabbed (playback only). */
  deckA?: GrabDeck
  deckB?: GrabDeck
}

/** The visual beatmatcher (M22): both decks' band-coloured close-ups
 * stacked (or side by side, vertical), playheads aligned mid-view —
 * when M20 says the decks are locked, the beat marks coincide. */
export function BeatView({
  vertical,
  getSourceA,
  getSourceB,
  deckA,
  deckB,
}: BeatViewProps) {
  const { t } = useTranslation()
  return (
    <Panel
      className={`beatview${vertical ? ' beatview--vertical' : ''}`}
      aria-label={t('beatview.title')}
    >
      <ZoomStrip
        label={t('beatview.deck', { id: 'A' })}
        deck="a"
        vertical={vertical}
        getSource={getSourceA}
        onGrab={grabHandler(deckA)}
      />
      <ZoomStrip
        label={t('beatview.deck', { id: 'B' })}
        deck="b"
        vertical={vertical}
        getSource={getSourceB}
        onGrab={grabHandler(deckB)}
      />
    </Panel>
  )
}
