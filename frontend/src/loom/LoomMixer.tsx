import { useCallback, useMemo, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'

import { EQ_BANDS, type EqBand } from '../audio/eq'
import { FX_KINDS, fxRestPosition, type FxKind } from '../audio/fx'
import { knobFromTrimDb, trimDbFromKnob } from '../audio/master'
import type { DeckId } from '../audio/types'
import type { ChannelControls } from '../mixer/MixerStrip'
import { Button } from '../ui/Button'
import { Knob } from '../ui/Knob'
import { LevelMeter } from '../ui/LevelMeter'
import { PhaseMeter } from '../ui/PhaseMeter'
import { SegmentedControl } from '../ui/SegmentedControl'
import { Select } from '../ui/Select'
import { Slider } from '../ui/Slider'
import { TransportButton } from '../ui/TransportButton'
import { VerticalFader } from '../ui/VerticalFader'
import './loom.css'

export type LoopBars = 4 | 8 | 16

/** One deck's loom-side state and gestures; the channel strip (fader, trim,
 * EQ, cue) stays on the shared `ChannelControls` contract. */
export type LoomDeckPanel = {
  /** The current node's title; null while nothing is loaded. */
  title: string | null
  /** The countdown tag ("12 → breakbeat", "12 → HOLD"); `warn` turns it
   * amber (HOLD within eight bars, a tied render that will be late). */
  next: { label: string; warn: boolean } | null
  /** Position in the node, e.g. "bar 9 / 32". */
  bar: string | null
  playing: boolean
  onPlay: () => void
  onStop: () => void
  onSync: () => void
  syncDisabled: boolean
  auto: boolean
  onAuto: (on: boolean) => void
  loopBars: LoopBars | null
  /** Pressing the lit length releases the loop: `onLoop(null)`. */
  onLoop: (bars: LoopBars | null) => void
  fx: { kind: FxKind | null; amount: number }
  onSetFx: (kind: FxKind | null) => void
  onSetFxAmount: (amount: number) => void
  onBounce?: () => void
  /** A loom node dragged onto the panel (`text/plain` = `loom:<id>`): tie it. */
  onDropNode?: (id: string) => void
  /** The deck's beat clock, for the badge pulse (visuals pass). */
  getBeat?: () => { periodSeconds: number; beatAtContext: number } | null
}

export type LoomMixerProps = {
  channels: Record<DeckId, ChannelControls>
  decks: Record<DeckId, LoomDeckPanel>
  crossfade: number
  onCrossfade: (position: number) => void
  cueMix: number
  onCueMix: (position: number) => void
  getMasterLevel: () => number
  getPhaseOffset: () => number | null
  /** The scope window's contents (the Lissajous canvas); empty when absent. */
  scope?: ReactNode
}

const LOOP_BARS: LoopBars[] = [4, 8, 16]
const LOOP_OPTIONS = LOOP_BARS.map((bars) => ({ value: String(bars), label: String(bars) }))
// Hardware mixers stack HI first; EQ_BANDS stays low→high for the audio
// chain, this is display order only (as in MixerStrip).
const EQ_DISPLAY_ORDER: EqBand[] = [...EQ_BANDS].reverse()

/** The loom theme's mixer bar: deck A | core | deck B in one 150 px row.
 * Controlled: every value comes in as a prop and every gesture goes out as a
 * callback, so the DJ theme's channel wiring and the FLX4 intents drive it
 * unchanged. */
export function LoomMixer({
  channels,
  decks,
  crossfade,
  onCrossfade,
  cueMix,
  onCueMix,
  getMasterLevel,
  getPhaseOffset,
  scope,
}: LoomMixerProps) {
  const { t } = useTranslation()
  return (
    <section className="loom-mixer" aria-label={t('mixer.title')}>
      <LoomDeck deckId="a" channel={channels.a} deck={decks.a} />
      <div className="loom-mixer__core">
        <div className="loom-mixer__crossfade">
          <span className="loom-mixer__edge loom-mixer__edge--a">{t('mixer.deckA')}</span>
          <div className="loom-mixer__crossfade-slider">
            <Slider
              label={t('mixer.crossfade')}
              min={0}
              max={1}
              step={0.01}
              value={crossfade}
              data-shortcut="crossfade"
              onChange={onCrossfade}
            />
          </div>
          <span className="loom-mixer__edge loom-mixer__edge--b">{t('mixer.deckB')}</span>
        </div>
        <div className="loom-mixer__master">
          <div className="loom-mixer__scope-column">
            <div className="loom-mixer__scope">{scope}</div>
            <PhaseMeter label={t('mixer.phase')} getOffset={getPhaseOffset} />
          </div>
          <Knob
            label={t('mixer.cueMix')}
            accent="master"
            size="s"
            value={cueMix}
            onChange={onCueMix}
          />
          <div className="loom-mixer__meter">
            <LevelMeter label={t('mixer.masterLevel')} getLevel={getMasterLevel} />
            <span className="loom-mixer__caption">{t('mixer.master')}</span>
          </div>
        </div>
      </div>
      <LoomDeck deckId="b" channel={channels.b} deck={decks.b} />
    </section>
  )
}

function LoomDeck({
  deckId,
  channel,
  deck,
}: {
  deckId: DeckId
  channel: ChannelControls
  deck: LoomDeckPanel
}) {
  const { t } = useTranslation()
  const { onSetFx } = deck
  // Select is memoised and dismisses its open popup on a re-commit, so the
  // options and handler must keep their identity across the 4 Hz track polls.
  const fxOptions = useMemo(
    () => [
      { value: '', label: t('deck.fx.off') },
      ...FX_KINDS.map((kind) => ({ value: kind, label: t(`deck.fx.names.${kind}`) })),
    ],
    [t],
  )
  const handleFx = useCallback(
    (value: string) => onSetFx(FX_KINDS.find((kind) => kind === value) ?? null),
    [onSetFx],
  )

  return (
    <div
      className={`loom-mixer__deck loom-mixer__deck--${deckId}`}
      role="group"
      aria-label={t('deck.title', { id: deckId })}
      onDragOver={(event) => {
        if (deck.onDropNode) event.preventDefault()
      }}
      onDrop={(event) => {
        const data = event.dataTransfer.getData('text/plain')
        if (!deck.onDropNode || !data.startsWith('loom:')) return
        // Handled here: the booth's song drop must not see it too.
        event.preventDefault()
        event.stopPropagation()
        deck.onDropNode(data.slice('loom:'.length))
      }}
    >
      <div className="loom-mixer__status">
        <span className="loom-mixer__letter">
          {t(deckId === 'a' ? 'mixer.deckA' : 'mixer.deckB')}
        </span>
        <span
          className={`loom-mixer__title${deck.title === null ? ' loom-mixer__title--empty' : ''}`}
          title={deck.title ?? undefined}
        >
          {deck.title ?? t('loom.deck.nothingTied')}
        </span>
        {deck.next && (
          <span
            className={`loom-mixer__next${deck.next.warn ? ' loom-mixer__next--warn' : ''}`}
          >
            {deck.next.label}
          </span>
        )}
        {deck.bar && <span className="loom-mixer__bar">{deck.bar}</span>}
        {deck.onBounce && (
          <Button
            aria-label={t('loom.deck.bounce')}
            title={t('loom.deck.bounce')}
            onClick={deck.onBounce}
          >
            <span aria-hidden="true">⤓</span>
          </Button>
        )}
      </div>

      <div className="loom-mixer__strip">
        <div className="loom-mixer__transport">
          {deck.playing ? (
            <TransportButton
              kind="stop"
              accent={deckId}
              lit
              label={t('deck.stop')}
              onClick={deck.onStop}
            />
          ) : (
            <TransportButton
              kind="play"
              accent={deckId}
              label={t('deck.play')}
              disabled={deck.title === null}
              onClick={deck.onPlay}
            />
          )}
          <Button
            lit={channel.cue}
            aria-pressed={channel.cue}
            onClick={() => channel.onSetCue(!channel.cue)}
          >
            {t('mixer.cue')}
          </Button>
          <Button disabled={deck.syncDisabled} onClick={deck.onSync}>
            {t('deck.track.sync')}
          </Button>
          <Button
            lit={deck.auto}
            aria-pressed={deck.auto}
            title={t('loom.deck.autoHint')}
            onClick={() => deck.onAuto(!deck.auto)}
          >
            {t('loom.deck.auto')}
          </Button>
        </div>

        <SegmentedControl
          label={t('loom.deck.loop')}
          value={deck.loopBars === null ? '' : String(deck.loopBars)}
          options={LOOP_OPTIONS}
          onChange={(value) => {
            const bars = LOOP_BARS.find((length) => String(length) === value) ?? null
            deck.onLoop(bars === deck.loopBars ? null : bars)
          }}
        />

        <div className="loom-mixer__fader">
          <VerticalFader
            label={t('deck.volume')}
            accent={deckId}
            value={channel.volume}
            onChange={channel.onSetVolume}
          />
          <LevelMeter
            label={t('mixer.channelLevel', { id: deckId })}
            getLevel={channel.getLevel}
          />
        </div>

        <div className="loom-mixer__eq">
          <Knob
            label={t('mixer.trim')}
            size="s"
            accent={deckId}
            value={knobFromTrimDb(channel.trim.db)}
            onChange={(value) => channel.onSetTrimDb(trimDbFromKnob(value))}
          />
          {EQ_DISPLAY_ORDER.map((band) => (
            <Knob
              key={band}
              label={t(`deck.eq.${band}`)}
              size="s"
              accent={deckId}
              value={channel.eq[band]}
              onChange={(value) => channel.onSetEqBand(band, value)}
            />
          ))}
        </div>

        <div className="loom-mixer__fx">
          <Select
            label={t('deck.fx.effect')}
            value={deck.fx.kind ?? ''}
            options={fxOptions}
            onChange={handleFx}
          />
          <Knob
            label={t('deck.fx.amount')}
            size="s"
            accent={deckId}
            value={deck.fx.amount}
            disabled={!deck.fx.kind}
            resetValue={deck.fx.kind ? fxRestPosition(deck.fx.kind) : 0}
            onChange={deck.onSetFxAmount}
          />
        </div>
      </div>
    </div>
  )
}
