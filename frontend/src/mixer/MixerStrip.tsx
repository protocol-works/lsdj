import { useTranslation } from 'react-i18next'

import { EQ_BANDS, type EqBand } from '../audio/eq'
import type { DeckId } from '../audio/types'
import { useAudioEngine } from '../audio/engineContext'
import { knobFromTrimDb, trimDbFromKnob } from '../audio/master'
import { Button } from '../ui/Button'
import { Knob } from '../ui/Knob'
import { LevelMeter } from '../ui/LevelMeter'
import { PhaseMeter } from '../ui/PhaseMeter'
import { Slider } from '../ui/Slider'
import { VerticalFader } from '../ui/VerticalFader'
import './mixer.css'

export type ChannelControls = {
  volume: number
  eq: Record<EqBand, number>
  cue: boolean
  /** Gain-staging trim (M17): auto follows source loudness, a knob
   * move takes over, AUTO re-engages. */
  trim: { mode: 'auto' | 'manual'; db: number }
  onSetVolume: (value: number) => void
  onSetEqBand: (band: EqBand, value: number) => void
  onSetCue: (on: boolean) => void
  onSetTrimDb: (db: number) => void
  onEnableAutoTrim: () => void
  getLevel: () => number
}

type MixerStripProps = {
  channels: Record<DeckId, ChannelControls>
  crossfade: number
  onCrossfadeChange: (position: number) => void
  cueMix: number
  onCueMixChange: (position: number) => void
  /** Beat-phase offset between the decks (M20), null while either
   * clock is unconfident — the meter blanks. */
  getPhaseOffset: () => number | null
}

/** The centre mixer strip: per-channel EQ knob columns, level meters, and
 * vertical faders, then the cue row (cue mix flanked by the deck cue buttons),
 * the crossfader, and the master output meter. Channel state lives in each
 * deck's hook; the strip only renders it. Output-device routing lives in
 * Settings; recording lives in the top bar (RecordControl). */
export function MixerStrip({
  channels,
  crossfade,
  onCrossfadeChange,
  cueMix,
  onCueMixChange,
  getPhaseOffset,
}: MixerStripProps) {
  const { t } = useTranslation()
  const engine = useAudioEngine()

  // Hardware mixers stack HI on top; EQ_BANDS stays low→high for the
  // audio chain, this is display order only.
  const eqDisplayOrder: EqBand[] = [...EQ_BANDS].reverse()

  function renderChannel(deckId: DeckId) {
    const channel = channels[deckId]
    return (
      <div
        key={deckId}
        className="mixer__channel"
        role="group"
        aria-label={t('mixer.channel', { id: deckId })}
      >
        {/* Trim sits above the EQ like a hardware channel strip;
            the knob shows the live value even while auto rides it. */}
        <Knob
          label={t('mixer.trim')}
          value={knobFromTrimDb(channel.trim.db)}
          accent={deckId}
          onChange={(value) => channel.onSetTrimDb(trimDbFromKnob(value))}
        />
        <Button
          lit={channel.trim.mode === 'auto'}
          aria-pressed={channel.trim.mode === 'auto'}
          onClick={channel.onEnableAutoTrim}
        >
          {t('mixer.trimAuto')}
        </Button>
        {eqDisplayOrder.map((band) => (
          <Knob
            key={band}
            label={t(`deck.eq.${band}`)}
            value={channel.eq[band]}
            accent={deckId}
            onChange={(value) => channel.onSetEqBand(band, value)}
          />
        ))}
        <div className="mixer__fader-row">
          <LevelMeter
            label={t('mixer.channelLevel', { id: deckId })}
            getLevel={channel.getLevel}
          />
          <VerticalFader
            label={t('deck.volume')}
            accent={deckId}
            value={channel.volume}
            onChange={channel.onSetVolume}
          />
        </div>
        <Button
          lit={channel.cue}
          aria-pressed={channel.cue}
          onClick={() => channel.onSetCue(!channel.cue)}
        >
          {t('mixer.cue')}
        </Button>
      </div>
    )
  }

  return (
    <section className="mixer" aria-label={t('mixer.title')}>
      {/* The crossfader sits on top, first to hand while the browser is open. */}
      <div className="mixer__crossfade">
        <span className="mixer__edge">{t('mixer.deckA')}</span>
        <div className="mixer__crossfade-slider">
          <Slider
            label={t('mixer.crossfade')}
            min={0}
            max={1}
            step={0.01}
            value={crossfade}
            data-shortcut="crossfade"
            onChange={onCrossfadeChange}
          />
        </div>
        <span className="mixer__edge">{t('mixer.deckB')}</span>
      </div>

      {/* Two channel strips flank a centre column: the master output meter
          sits between the decks (where it belongs on a hardware mixer), and the
          cue-mix knob drops to the bottom so it lands between the two cue
          buttons. */}
      <div className="mixer__channels">
        {renderChannel('a')}
        <div className="mixer__centre">
          <div className="mixer__master" role="group" aria-label={t('mixer.masterLevel')}>
            <LevelMeter
              label={t('mixer.masterLevel')}
              getLevel={engine.getMasterLevel}
            />
            <span className="mixer__master-label">{t('mixer.master')}</span>
          </div>
          <Knob
            label={t('mixer.cueMix')}
            accent="master"
            value={cueMix}
            onChange={onCueMixChange}
          />
        </div>
        {renderChannel('b')}
      </div>

      <PhaseMeter label={t('mixer.phase')} getOffset={getPhaseOffset} />
    </section>
  )
}
