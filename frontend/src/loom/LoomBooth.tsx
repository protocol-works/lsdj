import { useTranslation } from 'react-i18next'

import type { DeckId } from '../audio/types'
import type { TempoRange } from '../audio/track'
import type { DeckControls } from '../deck/useDeck'
import type { ChannelControls } from '../mixer/MixerStrip'

/** The booth contract in Loom theme: App hands over both decks and the mixer
 * projections it already owns, so the loom is a theme, not a second app. */
export type LoomBoothProps = {
  deckA: DeckControls
  deckB: DeckControls
  channels: Record<DeckId, ChannelControls>
  crossfade: number
  onCrossfade: (position: number) => void
  cueMix: number
  onCueMix: (position: number) => void
  getPhaseOffset: () => number | null
  onSyncA: () => void
  onSyncB: () => void
  tempoRange: TempoRange
  renderAhead: boolean
  performanceVisuals: boolean
}

/** Placeholder until the loom view and mixer land (T9 fills the body and
 * starts reading the props). */
// eslint-disable-next-line @typescript-eslint/no-unused-vars
export function LoomBooth(_props: LoomBoothProps) {
  const { t } = useTranslation()
  return (
    <section className="loom-booth" aria-label={t('loom.title')}>
      <p>{t('loom.empty')}</p>
    </section>
  )
}
