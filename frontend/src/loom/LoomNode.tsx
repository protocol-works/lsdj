import {
  memo,
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type DragEvent,
  type MouseEvent,
  type PointerEvent,
} from 'react'
import { useTranslation } from 'react-i18next'

import type { DeckId } from '../audio/types'
import { songDropTarget } from '../generation/library'
import { Button } from '../ui/Button'
import { keyHue } from './keyHue'
import type { LoomNode as LoomNodeModel } from './loomState'
import {
  barSeconds,
  fmtClock,
  MIN_BARS,
  nodeEndSeconds,
  nodeStartSeconds,
  RENDER,
  renderEta,
  TAIL_BARS,
} from './timing'

/** A `library_clip_peaks` envelope: per-bucket min/max over the whole clip. */
export type Peaks = { min: ArrayLike<number>; max: ArrayLike<number>; duration: number }
export type PeaksSource = (file: string, buckets: number) => Promise<Peaks | null>
/** A render in flight, from the render queue's progress ticks. */
export type RenderProgress = { elapsed: number; eta: number }

/** The view's gestures, one stable object for every node (so a node only
 * re-renders when its own data changes). */
export type NodeActions = {
  select: (id: string) => void
  tie: (deck: DeckId, id: string, cut: boolean) => void
  untie: (deck: DeckId, id: string) => void
  cue: (id: string) => void
  wave: (id: string, bar: number) => void
  keep: (id: string) => void
  discard: (id: string) => void
  retry: (id: string) => void
  weave: (id: string, anchor: HTMLElement) => void
  bounce: (id: string) => void
  trimPreview: (id: string, inBar: number, outBar: number) => void
  trimCommit: (id: string) => void
  /** A library song dropped on the node: it becomes the node's next thread. */
  dropSong: (file: string, parentId: string) => void
}

type LoomNodeProps = {
  node: LoomNodeModel
  /** The layout box, as primitives so an unchanged node skips re-rendering. */
  x: number
  y: number
  width: number
  pxPerBar: number
  onA: boolean
  onB: boolean
  queuedA: boolean
  queuedB: boolean
  cued: boolean
  /** The bar a cue starts from when it is inside the body; null = the join. */
  cueFromBar: number | null
  selected: boolean
  weaving: boolean
  hasChildren: boolean
  /** 1-based position in an open take group, or null. */
  take: number | null
  progress: RenderProgress | undefined
  peaks: PeaksSource
  actions: NodeActions
}

const MIN_BUCKETS = 64
const MAX_BUCKETS = 2048
const TIGHT_PX = 130
const TINY_PX = 64

/** One bucket per pixel of the whole clip, rounded up to a power of two so
 * a zoom gesture re-fetches a handful of sizes, not one per step. */
const bucketsFor = (clipPx: number) =>
  Math.min(MAX_BUCKETS, Math.max(MIN_BUCKETS, 2 ** Math.ceil(Math.log2(Math.max(1, clipPx)))))

/** The min/max envelope as one closed path in a `buckets × [-1, 1]` box,
 * square-root compressed like the deck overview. */
function wavePath(peaks: Peaks): string {
  const count = Math.min(peaks.min.length, peaks.max.length)
  const top: string[] = []
  const bottom: string[] = []
  for (let i = 0; i < count; i++) {
    const high = Math.sqrt(Math.min(1, Math.max(0, peaks.max[i])))
    const low = Math.sqrt(Math.min(1, Math.max(0, -peaks.min[i])))
    top.push(`${i ? 'L' : 'M'}${i},${(-high).toFixed(3)}`)
    bottom.push(`L${i},${low.toFixed(3)}`)
  }
  return count ? `${top.join('')}${bottom.reverse().join('')}Z` : ''
}

const clamp = (value: number, min: number, max: number) => Math.min(max, Math.max(min, value))

/** One thread on the loom: its waveform (trimmed to `[in, out]`), head,
 * trim handles, cue marker and tools. Controlled: every gesture goes out
 * through `actions`; the only local state is the fetched envelope and a trim
 * drag in progress. */
export const LoomNode = memo(function LoomNode({
  node,
  x,
  y,
  width,
  pxPerBar,
  onA,
  onB,
  queuedA,
  queuedB,
  cued,
  cueFromBar,
  selected,
  weaving,
  hasChildren,
  take,
  progress,
  peaks,
  actions,
}: LoomNodeProps) {
  const { t } = useTranslation()
  const trim = useRef<{ side: 'in' | 'out'; x: number; in0: number; out0: number } | null>(null)
  const [wave, setWave] = useState<{ file: string; path: string; buckets: number; duration: number } | null>(null)
  const [songOver, setSongOver] = useState(false)
  // Visuals: a ripple when a deck ties this node or its take is kept — a
  // fresh `key` restarts the one-shot CSS animation; loom.css gates it.
  const tied = onA || onB || queuedA || queuedB
  const [ripple, setRipple] = useState({ tied, take, count: 0 })
  if (ripple.tied !== tied || ripple.take !== take) {
    const fire = (tied && !ripple.tied) || (take === null && ripple.take !== null)
    setRipple({ tied, take, count: ripple.count + (fire ? 1 : 0) })
  }

  const ready = node.status === 'ready'
  const ghost = node.status === 'rendering'
  const body = node.out - node.in
  const clipSeconds = node.durationSeconds ?? node.tailSeconds + node.bars * barSeconds(node.bpm)
  const buckets = bucketsFor((clipSeconds / barSeconds(node.bpm)) * pxPerBar)
  const file = node.file

  useEffect(() => {
    if (!file) return
    let live = true
    void peaks(file, buckets).then((fetched) => {
      if (!live || !fetched) return
      setWave({ file, path: wavePath(fetched), buckets: Math.min(fetched.min.length, fetched.max.length), duration: fetched.duration })
    })
    return () => {
      live = false
    }
  }, [file, buckets, peaks])

  // The visible window is the trimmed body: nominal seconds → a fraction of
  // the clip the envelope spans.
  const shown = wave && wave.file === file && wave.duration > 0 ? wave : null
  const viewBox = shown
    ? `${(nodeStartSeconds(node) / shown.duration) * shown.buckets} -1 ${
        (Math.max(0.001, nodeEndSeconds(node) - nodeStartSeconds(node)) / shown.duration) * shown.buckets
      } 2`
    : '0 -1 1 2'

  const startTrim = (side: 'in' | 'out') => (event: PointerEvent<HTMLSpanElement>) => {
    // Never a drag of the node, a pan of the canvas or a select.
    event.stopPropagation()
    event.preventDefault()
    event.currentTarget.setPointerCapture?.(event.pointerId)
    trim.current = { side, x: event.clientX, in0: node.in, out0: node.out }
  }
  const moveTrim = (event: PointerEvent<HTMLSpanElement>) => {
    const drag = trim.current
    if (!drag) return
    const delta = Math.round((event.clientX - drag.x) / pxPerBar)
    const minBody = Math.min(MIN_BARS, node.bars)
    if (drag.side === 'in') {
      actions.trimPreview(node.id, clamp(drag.in0 + delta, 0, drag.out0 - minBody), drag.out0)
    } else {
      actions.trimPreview(node.id, drag.in0, clamp(drag.out0 + delta, drag.in0 + minBody, node.bars))
    }
  }
  const endTrim = () => {
    if (!trim.current) return
    trim.current = null
    actions.trimCommit(node.id)
  }

  const onDragStart = (event: DragEvent<HTMLDivElement>) => {
    if (trim.current) {
      event.preventDefault()
      return
    }
    event.dataTransfer.setData('text/plain', `loom:${node.id}`)
    event.dataTransfer.setData('text/lsdj-node', node.id)
    event.dataTransfer.effectAllowed = 'link'
  }

  const onWaveClick = (event: MouseEvent<SVGSVGElement>) => {
    if (!ready) return
    event.stopPropagation()
    const rect = event.currentTarget.getBoundingClientRect()
    if (!(rect.width > 0)) return
    const bar = node.in + clamp(Math.floor(((event.clientX - rect.left) / rect.width) * body), 0, body - 1)
    actions.wave(node.id, bar)
  }

  const onClick = (event: MouseEvent<HTMLDivElement>) => {
    if ((event.target as Element).closest('button, .loom-node__handle')) return
    actions.select(node.id)
  }

  const meta = (() => {
    if (ghost) {
      const eta =
        progress?.eta ??
        renderEta({ bars: node.bars, tailBars: node.parentId ? TAIL_BARS : 0, bpm: node.bpm, loras: node.loras })
      const time = fmtClock(eta - (progress?.elapsed ?? 0))
      const merging = progress && node.loras.length > 0 && progress.elapsed < RENDER.mergePerLora * node.loras.length
      const label = t(!progress ? 'loom.node.queued' : merging ? 'loom.node.merging' : 'loom.node.rendering', { time })
      return node.auto ? t('loom.node.autoLabel', { label }) : label
    }
    if (node.status === 'failed') return t('loom.node.failed', { error: node.error ?? '' })
    if (node.status === 'missing') return t('loom.node.missing')
    const parts = [
      node.auto ? t('loom.node.auto') : null,
      node.loras.length > 0 ? t('loom.node.lora') : null,
      node.key ?? null,
      body < node.bars ? t('loom.node.trimmedBars', { length: body, bars: node.bars }) : t('loom.node.bars', { bars: node.bars }),
      take ? t('loom.node.take', { index: take }) : null,
    ]
    return parts.filter(Boolean).join(' · ')
  })()
  const metaTitle = [
    t('loom.node.seed', { seed: node.seed }),
    ...node.loras.map((lora) => t('loom.node.loraTitle', { name: lora.name, strength: lora.strength })),
  ].join(' · ')

  const hue = keyHue(node.key)
  const classes = [
    'loom-node',
    onA && 'loom-node--on-a',
    onB && 'loom-node--on-b',
    queuedA && 'loom-node--q-a',
    queuedB && 'loom-node--q-b',
    cued && 'loom-node--cued',
    cued && cueFromBar !== null && 'loom-node--from',
    (selected || weaving) && 'loom-node--selected',
    songOver && 'loom-node--drop',
    ghost && 'loom-node--ghost',
    node.status === 'failed' && 'loom-node--failed',
    node.status === 'missing' && 'loom-node--missing',
    take !== null && 'loom-node--take',
    width < TIGHT_PX && 'loom-node--tight',
    width < TINY_PX && 'loom-node--tiny',
  ]
    .filter(Boolean)
    .join(' ')
  const style = { left: x, top: y, width, '--loom-node-hue': hue ?? undefined } as CSSProperties

  const cueLabel =
    cueFromBar !== null
      ? t('loom.node.cueFrom', { bar: cueFromBar - node.in + 1 })
      : t(node.parentId || node.in > 0 ? 'loom.node.cueJoin' : 'loom.node.cueTop')
  const tieButton = (deck: DeckId, queued: boolean) => (
    <Button
      title={t(queued ? 'loom.tools.untieHint' : 'loom.tools.tieHint', { deck: t(deck === 'a' ? 'mixer.deckA' : 'mixer.deckB') })}
      onClick={(event) => (queued ? actions.untie(deck, node.id) : actions.tie(deck, node.id, event.shiftKey))}
    >
      {t(queued ? (deck === 'a' ? 'loom.tools.untieA' : 'loom.tools.untieB') : deck === 'a' ? 'loom.tools.tieA' : 'loom.tools.tieB')}
    </Button>
  )
  const discardButton = (
    <Button aria-label={t('loom.tools.discard')} title={t('loom.tools.discard')} onClick={() => actions.discard(node.id)}>
      {t('loom.tools.discardMark')}
    </Button>
  )

  return (
    <div
      className={classes}
      style={style}
      role="group"
      aria-label={node.prompt}
      tabIndex={0}
      draggable
      onDragStart={onDragStart}
      {...songDropTarget(setSongOver, (song) => actions.dropSong(song, node.id))}
      onClick={onClick}
    >
      <svg
        className="loom-node__wave"
        viewBox={viewBox}
        preserveAspectRatio="none"
        aria-hidden="true"
        onClick={onWaveClick}
      >
        {shown ? (
          <>
            <path className="loom-node__peaks" d={shown.path} />
            <path className="loom-node__body" d={shown.path} transform="scale(1 .55)" />
          </>
        ) : null}
      </svg>
      <div className="loom-node__head">
        <span className="loom-node__prompt" title={node.prompt}>
          {node.prompt}
        </span>
        <span className="loom-node__meta" title={ready ? metaTitle : node.error}>
          {meta}
        </span>
      </div>
      {ready ? (
        <>
          <span
            className={`loom-node__handle loom-node__handle--in${node.in > 0 ? ' loom-node__handle--set' : ''}`}
            title={t('loom.node.trimIn')}
            onPointerDown={startTrim('in')}
            onPointerMove={moveTrim}
            onPointerUp={endTrim}
            onPointerCancel={endTrim}
          />
          {/* A parent's out-point is where its children start: not trimmable. */}
          {hasChildren ? null : (
            <span
              className={`loom-node__handle loom-node__handle--out${node.out < node.bars ? ' loom-node__handle--set' : ''}`}
              title={t('loom.node.trimOut')}
              onPointerDown={startTrim('out')}
              onPointerMove={moveTrim}
              onPointerUp={endTrim}
              onPointerCancel={endTrim}
            />
          )}
        </>
      ) : null}
      {cued && cueFromBar !== null ? (
        <i className="loom-node__cue-point" style={{ left: `${((cueFromBar - node.in) / body) * 100}%` }} />
      ) : null}
      {cued ? <span className="loom-node__cue-label">{cueLabel}</span> : null}
      {ripple.count > 0 ? <i key={ripple.count} className="loom-node__ripple" aria-hidden="true" /> : null}
      {ghost ? (
        <div className="loom-node__progress">
          <i style={{ width: `${progress ? clamp(progress.elapsed / progress.eta, 0, 1) * 100 : 0}%` }} />
        </div>
      ) : null}
      <div className="loom-node__tools">
        {ready ? (
          <>
            {take !== null ? (
              <>
                <Button title={t('loom.tools.keepHint')} onClick={() => actions.keep(node.id)}>
                  {t('loom.tools.keep')}
                </Button>
                {discardButton}
              </>
            ) : null}
            <Button lit={cued} aria-pressed={cued} title={t('loom.tools.cueHint')} onClick={() => actions.cue(node.id)}>
              {t('loom.tools.cue')}
            </Button>
            {tieButton('a', queuedA)}
            {tieButton('b', queuedB)}
            <Button
              aria-label={t('loom.tools.weave')}
              title={t('loom.tools.weave')}
              onClick={(event) => actions.weave(node.id, event.currentTarget)}
            >
              {t('loom.tools.weaveMark')}
            </Button>
            <Button aria-label={t('loom.bounce.path')} title={t('loom.bounce.path')} onClick={() => actions.bounce(node.id)}>
              {t('loom.bounce.pathMark')}
            </Button>
          </>
        ) : node.status === 'failed' ? (
          <>
            <Button onClick={() => actions.retry(node.id)}>{t('loom.tools.retry')}</Button>
            {discardButton}
          </>
        ) : (
          discardButton
        )}
      </div>
    </div>
  )
})
