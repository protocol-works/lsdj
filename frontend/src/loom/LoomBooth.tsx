import { useEffect, useImperativeHandle, useRef, useState, type Ref } from 'react'
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
import { deckPath, nodePath, pathParts, stitchSongs } from './bounce'
import { PX_DEFAULT } from './layout'
import { LoomBar } from './LoomBar'
import { LoomMixer, type LoomDeckPanel } from './LoomMixer'
import { LoomScope } from './LoomScope'
import { nodeById, type LoomNode } from './loomState'
import { LoomView, type Peaks } from './LoomView'
import { deckSeam } from './schedule'
import { barAt, DEFAULT_BPM, HOLD_WARN_BARS, nodeStartSeconds } from './timing'
import { useLoom, type LoomControls } from './useLoom'
import { useBeatPulse } from './visuals'

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
  /** The bar's explorer button: App's media drawer toggle. */
  onToggleExplorer: () => void
  /** Receives the layer over the canvas App mounts the explorer drawer in. */
  drawerRef?: (element: HTMLDivElement | null) => void
}

/** What App reaches through the booth's ref: the explorer's →A/→B and an MCP
 * load land in the loom; its ⟡ seed thread starts a new one. */
export type LoomBoothHandle = {
  loadSong: (deck: DeckId, source: TrackSource, title: string) => Promise<boolean>
  seedSong: (file: string) => void
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
  performanceVisuals,
  barSlot,
  mediaOpen,
  onToggleExplorer,
  drawerRef,
}: LoomBoothProps & { ref?: Ref<LoomBoothHandle> }) {
  const { t } = useTranslation()
  const engine = useAudioEngine()
  const bus = useControlBus()
  const loom = useLoom({ deckA, deckB, renderAhead })
  const { state, loadSong, dropSong, beatLoop, stepLoop } = loom
  const [pxPerBar, setPxPerBar] = useState(PX_DEFAULT)
  const [scrollX, setScrollX] = useState(0)
  // The view's zoom and new-thread buttons render in the bar.
  const [toolsSlot, setToolsSlot] = useState<HTMLElement | null>(null)

  useImperativeHandle(ref, () => ({ loadSong, seedSong: dropSong }), [loadSong, dropSong])

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

  // ---- bounce: a path stitched into one library song (plan decision 14) ----
  // Its lines share the loom's status slot, so the newest line wins.
  const [bouncing, setBouncing] = useState(false)
  const bounce = (ids: string[], endAt?: number) => {
    if (bouncing) return
    const plan = pathParts(state, ids, t, endAt)
    if (!plan) {
      loom.say('loom.bounce.empty')
      return
    }
    setBouncing(true)
    loom.say('loom.bounce.working', { title: plan.title })
    stitchSongs(plan)
      .then(
        (entry) => loom.say('loom.bounce.done', { title: entry.title }),
        (error: unknown) =>
          loom.say('loom.bounce.failed', { message: error instanceof Error ? error.message : String(error) }),
      )
      .finally(() => setBouncing(false))
  }
  // What the deck played, up to its playhead; nothing to offer on an idle deck.
  const bounceDeck = (deck: DeckId, controls: DeckControls) =>
    deckPath(state, deck).length > 0 ? () => bounce(deckPath(state, deck), controls.track?.position) : undefined
  // ---- end bounce ----

  // ---- visuals: the beat pulse on the booth and the mixer's scope ----
  // Both are frame loops outside React, off with the performance-visuals
  // switch and under reduced motion.
  const boothRef = useRef<HTMLElement>(null)
  useBeatPulse(boothRef, performanceVisuals, deckA.getTrackBeat, deckB.getTrackBeat, engine.getContextTime)
  const scope = performanceVisuals ? (
    <LoomScope
      crossfade={crossfade}
      decks={{
        a: { getLevel: channels.a.getLevel, getBeat: deckA.getTrackBeat },
        b: { getLevel: channels.b.getLevel, getBeat: deckB.getTrackBeat },
      }}
    />
  ) : undefined
  // ---- /visuals ----

  const statusOf = (controls: DeckControls) =>
    controls.track && { position: controls.track.position, playing: controls.track.playing, loop: controls.track.loop }

  // The bar's clock line: the playing deck's tempo (A first), the counts.
  const onAir = deckA.track?.playing ? 'a' : deckB.track?.playing ? 'b' : null
  const airTrack = onAir === 'a' ? deckA.track : onAir === 'b' ? deckB.track : null
  const airNode = onAir ? nodeById(state.loom.nodes, state.decks[onAir].nodeId) : null
  const masterBpm = airNode && airTrack ? airNode.bpm * airTrack.rate : DEFAULT_BPM
  const countOf = (status: LoomNode['status']) => state.loom.nodes.filter((node) => node.status === status).length

  return (
    <section
      ref={boothRef}
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
      {/* The aurora and grain under the booth; loom.css shows them only with
          performance visuals on and motion allowed. */}
      <div className="loom-aurora" aria-hidden="true">
        <i className="loom-aurora__wash" />
        <i className="loom-aurora__wash" />
        <i className="loom-aurora__wash" />
        <i className="loom-aurora__wash" />
        <i className="loom-aurora__glow loom-aurora__glow--a" />
        <i className="loom-aurora__glow loom-aurora__glow--b" />
        <i className="loom-aurora__spiral" />
        <i className="loom-aurora__spiral loom-aurora__spiral--reverse" />
      </div>
      <div className="loom-grain" aria-hidden="true" />
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
        onBounce={(id) => bounce(nodePath(state, id))}
        onWeave={loom.weave}
        onDropSong={loom.dropSong}
        onSelect={loom.select}
        renderAhead={renderAhead}
        explorerOpen={mediaOpen}
        toolsSlot={toolsSlot}
        drawerRef={drawerRef}
      />
      <LoomMixer
        channels={channels}
        decks={{
          a: { ...deckPanel(t, loom, 'a', deckA, onSyncA), onBounce: bounceDeck('a', deckA) },
          b: { ...deckPanel(t, loom, 'b', deckB, onSyncB), onBounce: bounceDeck('b', deckB) },
        }}
        crossfade={crossfade}
        onCrossfade={onCrossfade}
        cueMix={cueMix}
        onCueMix={onCueMix}
        getMasterLevel={engine.getMasterLevel}
        getPhaseOffset={getPhaseOffset}
        scope={scope}
      />
      {barSlot &&
        createPortal(
          <LoomBar
            name={state.loom.name}
            createdAt={state.loom.createdAt}
            currentId={state.loom.id}
            saved={loom.saved}
            status={loom.status}
            looms={loom.looms}
            onRename={loom.rename}
            onNew={loom.newLoom}
            onOpen={(id) => void loom.openLoom(id)}
            onDuplicate={loom.duplicateLoom}
            onDelete={() => void loom.deleteLoom()}
            explorerOpen={mediaOpen}
            onToggleExplorer={onToggleExplorer}
            toolsRef={setToolsSlot}
            playing={onAir !== null}
            bpm={masterBpm}
            threads={countOf('ready')}
            weaving={countOf('rendering')}
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
