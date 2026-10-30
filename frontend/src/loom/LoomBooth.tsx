import { useEffect, useImperativeHandle, useState, type Ref } from 'react'
import { createPortal } from 'react-dom'
import { useTranslation } from 'react-i18next'
import type { TFunction } from 'i18next'

import { useAudioEngine } from '../audio/engineContext'
import { invoke } from '../audio/nativeEngine'
import type { DeckId, TrackSource } from '../audio/types'
import type { TempoRange } from '../audio/track'
import { useControlBus } from '../control/busContext'
import type { DeckControls } from '../deck/useDeck'
import type { ChannelControls } from '../mixer/MixerStrip'
import { PX_DEFAULT } from './layout'
import { LoomBar } from './LoomBar'
import { LoomMixer, type LoomDeckPanel } from './LoomMixer'
import { nodeById } from './loomState'
import { LoomView, type Peaks } from './LoomView'
import { deckSeam } from './schedule'
import { barAt, HOLD_WARN_BARS, nodeStartSeconds } from './timing'
import { useLoom, type LoomControls } from './useLoom'

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
  /** Where the loom bar (name, saved, menu) renders: App's status bar. */
  barSlot: HTMLElement | null
  /** The media explorer is open (its rating keys own `0`). */
  mediaOpen: boolean
}

/** What App reaches through the booth's ref: the explorer's →A/→B and an MCP
 * load land in the loom. */
export type LoomBoothHandle = {
  loadSong: (deck: DeckId, source: TrackSource, title: string) => Promise<boolean>
}

const peaks = (file: string, buckets: number) =>
  invoke<Peaks>('library_clip_peaks', { library: 'songs', name: file, buckets })

/** The loom theme's booth: the canvas over the mixer bar. It owns the loom
 * (`useLoom`), the zoom and scroll, the song drop, and the FLX4 loop section. */
export function LoomBooth({
  ref,
  deckA,
  deckB,
  channels,
  crossfade,
  onCrossfade,
  cueMix,
  onCueMix,
  getPhaseOffset,
  onSyncA,
  onSyncB,
  renderAhead,
  barSlot,
  mediaOpen,
}: LoomBoothProps & { ref?: Ref<LoomBoothHandle> }) {
  const { t } = useTranslation()
  const engine = useAudioEngine()
  const bus = useControlBus()
  const loom = useLoom({ deckA, deckB, renderAhead })
  const { state, loadSong, beatLoop, stepLoop } = loom
  const [pxPerBar, setPxPerBar] = useState(PX_DEFAULT)
  const [scrollX, setScrollX] = useState(0)

  useImperativeHandle(ref, () => ({ loadSong }), [loadSong])

  // The FLX4 loop section in Loom theme (App skips these intents there): the
  // loom's nominal 4/8/16 instead of the grid-required beat loop.
  useEffect(
    () =>
      bus.subscribe((intent) => {
        if (intent.kind === 'track_beat_loop') beatLoop(intent.deck)
        else if (intent.kind === 'track_loop_halve') stepLoop(intent.deck, -1)
        else if (intent.kind === 'track_loop_double') stepLoop(intent.deck, 1)
      }),
    [bus, beatLoop, stepLoop],
  )

  const statusOf = (controls: DeckControls) =>
    controls.track && { position: controls.track.position, playing: controls.track.playing, loop: controls.track.loop }

  return (
    <section
      className="loom-booth"
      aria-label={t('loom.title')}
      onDragOver={(event) => event.preventDefault()}
      onDrop={(event) => {
        // A song row dropped on the canvas becomes a root; a dragged node
        // (`loom:<id>`) only means something on a shuttle or a deck panel.
        const data = event.dataTransfer.getData('text/plain')
        if (!data || data.startsWith('loom:') || (event.target as Element).closest('.loom-mixer')) return
        event.preventDefault()
        loom.dropSong(data)
      }}
    >
      <LoomView
        state={state}
        pxPerBar={pxPerBar}
        onPxPerBar={setPxPerBar}
        scrollX={scrollX}
        onScrollX={setScrollX}
        deckStatus={{ a: statusOf(deckA), b: statusOf(deckB) }}
        peaks={peaks}
        progress={loom.progress}
        onTie={loom.tie}
        onUntie={loom.untie}
        onCut={loom.cut}
        onCue={loom.cue}
        onScrub={loom.scrub}
        onTrim={loom.trim}
        onKeep={loom.keep}
        onDiscard={loom.discard}
        onRetry={loom.retry}
        onWeave={loom.weave}
        onSelect={loom.select}
        renderAhead={renderAhead}
        explorerOpen={mediaOpen}
      />
      <LoomMixer
        channels={channels}
        decks={{
          a: deckPanel(t, loom, 'a', deckA, onSyncA),
          b: deckPanel(t, loom, 'b', deckB, onSyncB),
        }}
        crossfade={crossfade}
        onCrossfade={onCrossfade}
        cueMix={cueMix}
        onCueMix={onCueMix}
        getMasterLevel={engine.getMasterLevel}
        getPhaseOffset={getPhaseOffset}
      />
      {barSlot &&
        createPortal(
          <LoomBar
            name={state.loom.name}
            currentId={state.loom.id}
            saved={loom.saved}
            status={loom.status}
            looms={loom.looms}
            onRename={loom.rename}
            onNew={loom.newLoom}
            onOpen={(id) => void loom.openLoom(id)}
            onDuplicate={loom.duplicateLoom}
            onDelete={() => void loom.deleteLoom()}
          />,
          barSlot,
        )}
    </section>
  )
}

/** One deck panel's status line and gestures: the node, what waits at the
 * seam (amber HOLD within eight bars, a late tie), and the bar it is on. */
function deckPanel(
  t: TFunction,
  loom: LoomControls,
  deck: DeckId,
  controls: DeckControls,
  onSync: () => void,
): LoomDeckPanel {
  const { state, progress } = loom
  const runtime = state.decks[deck]
  const node = nodeById(state.loom.nodes, runtime.nodeId)
  const track = controls.track
  let next: LoomDeckPanel['next'] = null
  let bar: string | null = null
  if (node) {
    const position = track?.position ?? nodeStartSeconds(node)
    const seam = deckSeam(state, deck, node, position, progress)
    const title =
      seam.late > 0
        ? t('loom.deck.late', { seconds: Math.ceil(seam.late) })
        : seam.hold
          ? t('loom.deck.hold')
          : (seam.next?.prompt ?? '')
    next = {
      label: t('loom.deck.next', { bars: seam.bars, title }),
      warn: seam.late > 0 || (seam.hold && seam.bars <= HOLD_WARN_BARS),
    }
    const length = node.out - node.in
    const at = Math.floor(barAt(node, position) + 1e-6) - node.in + 1
    bar = t('loom.deck.bar', { bar: Math.min(length, Math.max(1, at)), length })
  }
  return {
    title: node?.prompt ?? track?.title ?? null,
    next,
    bar,
    playing: track?.playing ?? false,
    onPlay: () => void controls.play(),
    onStop: controls.stop,
    onSync,
    syncDisabled: track === null,
    auto: runtime.auto,
    onAuto: (on) => loom.setAuto(deck, on),
    loopBars: runtime.loopBars,
    onLoop: (bars) => loom.setLoop(deck, bars),
    // Stable from useDeck: the FX Select memoises on it.
    fx: controls.fx,
    onSetFx: controls.setFx,
    onSetFxAmount: controls.setFxAmount,
    onDropNode: (id) => loom.tie(deck, id),
  }
}
