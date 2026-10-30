import {
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type DragEvent,
  type PointerEvent,
} from 'react'
import { useTranslation } from 'react-i18next'

import type { DeckId } from '../audio/types'
import { Button } from '../ui/Button'
import { fitPxPerBar, LAYOUT, layoutLoom, PX_DEFAULT, zoomAnchored, type NodeBox } from './layout'
import { LoomNode, type NodeActions, type PeaksSource, type Peaks, type RenderProgress } from './LoomNode'
import { LoomRuler } from './LoomRuler'
import { nodeById, type LoomNode as LoomNodeModel, type LoomState } from './loomState'
import { deckSeam } from './schedule'
import {
  barSeconds,
  barStartSeconds,
  fmtClock,
  holdRegion,
  HOLD_WARN_BARS,
  loopRegion,
  nodeEndSeconds,
  nodeStartSeconds,
} from './timing'
import { WeavePopover, type WeaveDeck, type WeaveForm } from './WeavePopover'
import './loom.css'

export type { Peaks, PeaksSource, RenderProgress } from './LoomNode'
export type { WeaveForm } from './WeavePopover'

/** A deck's engine status at its 4 Hz poll; null while nothing is loaded. */
export type LoomDeckStatus = {
  position: number
  playing: boolean
  loop: { start: number; end: number } | null
} | null

export type LoomViewProps = {
  state: LoomState
  pxPerBar: number
  onPxPerBar: (px: number) => void
  scrollX: number
  onScrollX: (x: number) => void
  deckStatus: Record<DeckId, LoomDeckStatus>
  /** `library_clip_peaks` for a song; the view caches by `file:buckets`. */
  peaks: PeaksSource
  /** Renders in flight by node id; a rendering node without one is queued. */
  progress: Record<string, RenderProgress>
  onTie: (deck: DeckId, id: string) => void
  onUntie: (deck: DeckId, id: string) => void
  onCut: (deck: DeckId, id: string) => void
  /** `id` null stops the cue; `fromBar` null = from the join. */
  onCue: (id: string | null, fromBar: number | null) => void
  onScrub: (deck: DeckId, seconds: number) => void
  onTrim: (id: string, inBar: number, outBar: number) => void
  onKeep: (id: string) => void
  onDiscard: (id: string) => void
  onRetry: (id: string) => void
  onWeave: (parentId: string | null, form: WeaveForm) => void
  onSelect: (id: string | null) => void
  renderAhead: boolean
  /** The media explorer is open: its rating keys own `0` (clear), so the zoom
   * reset leaves it alone. */
  explorerOpen?: boolean
}

const DECKS: DeckId[] = ['a', 'b']
const NODE_HEIGHT = 62
// The "what next?" slot after every leaf, and the room left of the first bar.
const SLOT_WIDTH = 236
const CANVAS_MARGIN = 120
// The deck's own path: the last few nodes it played, so a long set stays legible.
const TRAIL_NODES = 8

const clamp = (value: number, min: number, max: number) => Math.min(max, Math.max(min, value))
const deckLetter = (deck: DeckId) => (deck === 'a' ? 'mixer.deckA' : 'mixer.deckB')

/** Typing in a field keeps its keys (the `shortcuts.ts` guard). */
const isTyping = (target: EventTarget | null) =>
  target instanceof HTMLElement &&
  (['INPUT', 'SELECT', 'TEXTAREA'].includes(target.tagName) || target.isContentEditable)

/** A tree edge: along the row, or down from under the parent's end. */
function edgePath(parent: NodeBox, child: NodeBox) {
  const x0 = parent.x + parent.w
  const y0 = parent.y + NODE_HEIGHT / 2
  const x1 = child.x
  const y1 = child.y + NODE_HEIGHT / 2
  if (child.row === parent.row) return `M${x0},${y0}L${x1},${y1}`
  const sx = x0 - 22
  return `M${sx},${parent.y + NODE_HEIGHT - 1}C${sx},${y1 - 22} ${sx + 6},${y1} ${x1},${y1}`
}

/** A jump or a return between nodes that are not parent and child. */
function jumpPath(from: NodeBox, to: NodeBox) {
  const x0 = from.x + from.w
  const y0 = from.y + NODE_HEIGHT / 2
  const x1 = to.x
  const y1 = to.y + NODE_HEIGHT / 2
  if (x1 < x0) {
    const lift = Math.min(from.y, to.y) - LAYOUT.row * 0.55
    return `M${x0},${y0}C${x0 + 70},${lift} ${x1 - 70},${lift} ${x1},${y1}`
  }
  const mid = (x0 + x1) / 2
  return `M${x0},${y0}C${mid},${y0} ${mid},${y1} ${x1},${y1}`
}

/** The loom canvas: ruler, threads, nodes, take brackets, the decks'
 * shuttles, zoom and pan, and the weave popover. Controlled: the loom state
 * and deck status come in, every gesture goes out as a callback; the view
 * keeps only transient pointer state (a pan, a trim drag, the open weave). */
export function LoomView(props: LoomViewProps) {
  const { state, pxPerBar, scrollX, deckStatus, progress, renderAhead } = props
  const { t } = useTranslation()
  const latest = useRef(props)
  useEffect(() => {
    latest.current = props
  })
  const canvasRef = useRef<HTMLDivElement>(null)
  const weaveAnchor = useRef<HTMLElement | null>(null)
  const [weaving, setWeaving] = useState<{ parentId: string | null } | null>(null)
  const [trimDraft, setTrimDraft] = useState<{ id: string; in: number; out: number } | null>(null)
  const trimDraftRef = useRef(trimDraft)
  const peaksCache = useRef(new Map<string, Promise<Peaks | null>>())
  const pan = useRef<{ x: number; y: number; left: number; top: number; moved: boolean } | null>(null)

  // A trim previews live — the tree closes up behind the handle — and lands
  // once, on release.
  const nodes = useMemo(
    () =>
      trimDraft
        ? state.loom.nodes.map((n) => (n.id === trimDraft.id ? { ...n, in: trimDraft.in, out: trimDraft.out } : n))
        : state.loom.nodes,
    [state.loom.nodes, trimDraft],
  )
  const layout = useMemo(() => layoutLoom(nodes, pxPerBar), [nodes, pxPerBar])
  const width = layout.width + SLOT_WIDTH + CANVAS_MARGIN
  const height = layout.height + NODE_HEIGHT

  // Stable for the view's lifetime: they read the latest props through the
  // ref, so a node re-renders only when its own data changes.
  const view = useMemo(() => {
    const nodeOf = (id: string) => nodeById(latest.current.state.loom.nodes, id)
    const zoomTo = (target: number, clientX?: number, scrollBy = 0) => {
      const canvas = canvasRef.current
      const px = latest.current.pxPerBar
      const scroll = canvas?.scrollLeft ?? 0
      const rect = canvas?.getBoundingClientRect()
      const offset = canvas && rect ? (clientX ?? rect.left + canvas.clientWidth / 2) - rect.left : 0
      const zoom = zoomAnchored(px, target / px, (scroll + offset - LAYOUT.padX) / px)
      if (zoom.px !== px) {
        latest.current.onPxPerBar(zoom.px)
        latest.current.onScrollX(Math.max(0, scroll + zoom.scrollDelta + scrollBy))
      } else if (scrollBy && canvas) {
        canvas.scrollLeft = scroll + scrollBy
      }
    }
    const openWeave = (parentId: string | null, anchor: HTMLElement) => {
      weaveAnchor.current = anchor
      setWeaving({ parentId })
    }
    const actions: NodeActions = {
      select: (id) => latest.current.onSelect(latest.current.state.selected === id ? null : id),
      tie: (deck, id, cut) => (cut ? latest.current.onCut(deck, id) : latest.current.onTie(deck, id)),
      untie: (deck, id) => latest.current.onUntie(deck, id),
      cue: (id) => {
        const { decks } = latest.current.state
        latest.current.onCue((decks.a.cue ?? decks.b.cue)?.nodeId === id ? null : id, null)
      },
      // On the node a deck plays a click moves that deck; anywhere else it
      // cues from the clicked bar.
      wave: (id, bar) => {
        const node = nodeOf(id)
        const deck = DECKS.find((d) => latest.current.state.decks[d].nodeId === id)
        if (node && deck) latest.current.onScrub(deck, barStartSeconds(node, bar))
        else latest.current.onCue(id, bar)
      },
      keep: (id) => latest.current.onKeep(id),
      discard: (id) => latest.current.onDiscard(id),
      retry: (id) => latest.current.onRetry(id),
      weave: openWeave,
      trimPreview: (id, inBar, outBar) => {
        trimDraftRef.current = { id, in: inBar, out: outBar }
        setTrimDraft(trimDraftRef.current)
      },
      trimCommit: (id) => {
        const draft = trimDraftRef.current
        const node = nodeOf(id)
        trimDraftRef.current = null
        setTrimDraft(null)
        if (draft?.id === id && node && (draft.in !== node.in || draft.out !== node.out)) {
          latest.current.onTrim(id, draft.in, draft.out)
        }
      },
    }
    const peaks: PeaksSource = (file, buckets) => {
      const key = `${file}:${buckets}`
      let hit = peaksCache.current.get(key)
      if (!hit) {
        hit = latest.current.peaks(file, buckets).catch(() => null)
        peaksCache.current.set(key, hit)
      }
      return hit
    }
    return {
      actions,
      peaks,
      openWeave,
      zoomTo,
      zoomBy: (factor: number, clientX?: number) => zoomTo(latest.current.pxPerBar * factor, clientX),
      fit: () => {
        latest.current.onPxPerBar(
          fitPxPerBar(latest.current.state.loom.nodes, canvasRef.current?.clientWidth ?? 0),
        )
        latest.current.onScrollX(0)
      },
      stopCue: () => {
        const { decks } = latest.current.state
        if (decks.a.cue || decks.b.cue) latest.current.onCue(null, null)
      },
    }
  }, [])

  // Controlled scroll: follow `scrollX` when it moves away from the canvas
  // (a zoom anchors it); a native scroll reports back through onScroll.
  useLayoutEffect(() => {
    const canvas = canvasRef.current
    if (canvas && Math.abs(canvas.scrollLeft - scrollX) > 1) canvas.scrollLeft = scrollX
  }, [scrollX, pxPerBar])

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.metaKey || event.ctrlKey || event.altKey || isTyping(event.target)) return
      if (event.key === '=' || event.key === '+') view.zoomBy(1.25)
      else if (event.key === '-') view.zoomBy(0.8)
      else if (event.key === '0' && !latest.current.explorerOpen) view.zoomTo(PX_DEFAULT)
      else if (event.key === 'z' || event.key === 'Z') view.fit()
      else if (event.key === 'Escape') view.stopCue()
      else return
      event.preventDefault()
    }
    // ⌘-wheel (or a pinch) zooms at the cursor; React's wheel listener is
    // passive, so this one is registered by hand to keep the page from zooming.
    const canvas = canvasRef.current
    const onWheel = (event: WheelEvent) => {
      if (!(event.metaKey || event.ctrlKey)) return
      event.preventDefault()
      view.zoomBy(event.deltaY < 0 ? 1.1 : 0.9, event.clientX)
    }
    window.addEventListener('keydown', onKeyDown)
    canvas?.addEventListener('wheel', onWheel, { passive: false })
    return () => {
      window.removeEventListener('keydown', onKeyDown)
      canvas?.removeEventListener('wheel', onWheel)
    }
  }, [view])

  // Drag the background to pan; a plain click there clears the selection.
  const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    const canvas = canvasRef.current
    if (!canvas || (event.target as Element).closest('.loom-node, .loom__slot, .loom__tag, .loom__empty, button')) return
    pan.current = { x: event.clientX, y: event.clientY, left: canvas.scrollLeft, top: canvas.scrollTop, moved: false }
    canvas.setPointerCapture?.(event.pointerId)
  }
  const onPointerMove = (event: PointerEvent<HTMLDivElement>) => {
    const canvas = canvasRef.current
    const current = pan.current
    if (!canvas || !current) return
    const dx = event.clientX - current.x
    const dy = event.clientY - current.y
    current.moved ||= Math.abs(dx) + Math.abs(dy) > 3
    canvas.scrollLeft = current.left - dx
    canvas.scrollTop = current.top - dy
  }
  const onPointerUp = () => {
    if (pan.current && !pan.current.moved && state.selected) props.onSelect(null)
    pan.current = null
  }

  const cue = state.decks.a.cue ?? state.decks.b.cue
  const takeGroups = new Map<string, LoomNodeModel[]>()
  for (const node of nodes) {
    if (node.takeGroup) takeGroups.set(node.takeGroup, [...(takeGroups.get(node.takeGroup) ?? []), node])
  }
  for (const [group, members] of takeGroups) if (members.length < 2) takeGroups.delete(group)
  const parents = new Set(nodes.map((n) => n.parentId))

  const shuttles = DECKS.flatMap((deck) => {
    const runtime = state.decks[deck]
    const node = nodeById(nodes, runtime.nodeId)
    const box = node && layout.boxes.get(node.id)
    if (!node || !box) return []
    const status = deckStatus[deck]
    const start = nodeStartSeconds(node)
    const end = nodeEndSeconds(node)
    const position = status?.position ?? start
    const xAt = (seconds: number) => box.x + clamp((seconds - start) / Math.max(1e-6, end - start), 0, 1) * box.w
    const seam = deckSeam(state, deck, node, position, progress)
    const hold = holdRegion(node)
    // A loop the plan did not place (the FLX4's loop in/out) is the DJ's too.
    const engineLoop = status?.loop && Math.abs(status.loop.start - hold.start) > 0.01 ? status.loop : null
    const region = runtime.loopBars
      ? { ...loopRegion(node, runtime.loopStartBar ?? node.in, runtime.loopBars), bars: runtime.loopBars, hold: false }
      : engineLoop
        ? { ...engineLoop, bars: Math.round((engineLoop.end - engineLoop.start) / barSeconds(node.bpm)), hold: false }
        : seam.hold
          ? { ...hold, bars: 0, hold: true }
          : null
    const letter = t(deckLetter(deck))
    const title = seam.next?.prompt ?? ''
    let tag: string
    let warn = false
    if (!status?.playing) tag = t('loom.shuttle.paused', { deck: letter })
    else if (region && !region.hold) tag = t('loom.shuttle.loop', { deck: letter, bars: region.bars })
    else if (seam.next?.status === 'ready') {
      tag = t(seam.auto ? 'loom.shuttle.auto' : 'loom.shuttle.next', { deck: letter, bars: seam.bars, title })
    } else if (seam.next?.status === 'rendering') {
      warn = seam.late > 0
      tag = warn
        ? t('loom.shuttle.late', { deck: letter, bars: seam.bars, title, late: fmtClock(seam.late) })
        : t('loom.shuttle.rendering', { deck: letter, bars: seam.bars, title })
    } else {
      warn = seam.bars <= HOLD_WARN_BARS
      const skipped = renderAhead && !seam.next && node.loras.length > 0 && !parents.has(node.id)
      tag = t(skipped ? 'loom.shuttle.holdLora' : 'loom.shuttle.hold', { deck: letter, bars: seam.bars })
    }
    const x = xAt(position)
    const trailFrom = region && position >= region.start ? xAt(region.start) : box.x
    // Holding (or about to): the leaf's "what next?" slot lights up.
    const holding = Boolean(status?.playing && seam.hold && !runtime.loopBars && seam.bars <= HOLD_WARN_BARS)
    return [{ deck, node, box, region, xAt, x, trailFrom, tag, warn, holding }]
  })

  const weaveParent = weaving?.parentId ? nodeById(state.loom.nodes, weaving.parentId) : null
  const weaveDecks: WeaveDeck[] = weaveParent
    ? DECKS.filter((deck) => state.decks[deck].nodeId === weaveParent.id).map((deck) => {
        const status = deckStatus[deck]
        return { deck, secondsToSeam: status?.playing ? nodeEndSeconds(weaveParent) - status.position : null }
      })
    : []

  const dropOnTag = (deck: DeckId) => (event: DragEvent<HTMLDivElement>) => {
    const data = event.dataTransfer.getData('text/plain')
    if (!data.startsWith('loom:')) return
    event.preventDefault()
    props.onCut(deck, data.slice('loom:'.length))
  }

  return (
    <div className="loom" style={{ '--loom-node-height': `${NODE_HEIGHT}px` } as CSSProperties}>
      <div
        ref={canvasRef}
        className="loom__canvas"
        onScroll={(event) => props.onScrollX(event.currentTarget.scrollLeft)}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
      >
        <div className="loom__content" style={{ width, height }}>
          <LoomRuler width={width} pxPerBar={pxPerBar} onDrag={(px, anchorX, scrollBy) => view.zoomTo(px, anchorX, scrollBy)} />
          <svg className="loom__threads" width={width} height={height} aria-hidden="true">
            {layout.edges.map(({ from, to }) => {
              const parent = layout.boxes.get(from)
              const child = layout.boxes.get(to)
              if (!parent || !child) return null
              const ghost = nodeById(nodes, to)?.status === 'rendering'
              return (
                <path
                  key={`${from}>${to}`}
                  className={`loom__thread${ghost ? ' loom__thread--ghost' : ''}`}
                  d={edgePath(parent, child)}
                />
              )
            })}
            {DECKS.flatMap((deck) => {
              const runtime = state.decks[deck]
              const legs = (ids: string[], kind: 'past' | 'future') =>
                ids.slice(1).flatMap((id, i) => {
                  const from = nodeById(nodes, ids[i])
                  const to = nodeById(nodes, id)
                  const a = from && layout.boxes.get(from.id)
                  const b = to && layout.boxes.get(id)
                  if (!a || !b || !to) return []
                  return [
                    <path
                      key={`${deck}-${kind}-${i}`}
                      className={`loom__thread loom__thread--${deck} loom__thread--${kind}`}
                      d={to.parentId === ids[i] ? edgePath(a, b) : jumpPath(a, b)}
                    />,
                  ]
                })
              if (!runtime.nodeId) return []
              return [
                ...legs([...runtime.history.slice(-TRAIL_NODES), runtime.nodeId], 'past'),
                ...legs([runtime.nodeId, ...runtime.queue], 'future'),
              ]
            })}
          </svg>
          {[...takeGroups].map(([group, members]) => {
            const boxes = members.flatMap((m) => layout.boxes.get(m.id) ?? [])
            if (boxes.length === 0) return null
            const top = Math.min(...boxes.map((b) => b.y)) - 4
            const bottom = Math.max(...boxes.map((b) => b.y)) + NODE_HEIGHT + 4
            const ready = members.filter((m) => m.status === 'ready').length
            return (
              <div key={group} className="loom__takes" style={{ left: boxes[0].x - 11, top, height: bottom - top }}>
                <span>
                  {ready < members.length
                    ? t('loom.takes.rendering', { total: members.length, ready })
                    : t('loom.takes.pick', { total: members.length })}
                </span>
              </div>
            )
          })}
          {nodes.map((node) => {
            const box = layout.boxes.get(node.id)
            if (!box) return null
            const members = node.takeGroup ? takeGroups.get(node.takeGroup) : undefined
            return (
              <LoomNode
                key={node.id}
                node={node}
                x={box.x}
                y={box.y}
                width={box.w}
                pxPerBar={pxPerBar}
                onA={state.decks.a.nodeId === node.id}
                onB={state.decks.b.nodeId === node.id}
                queuedA={state.decks.a.queue.includes(node.id)}
                queuedB={state.decks.b.queue.includes(node.id)}
                cued={cue?.nodeId === node.id}
                cueFromBar={cue?.nodeId === node.id && cue.fromBar >= node.in ? cue.fromBar : null}
                selected={state.selected === node.id}
                weaving={weaving?.parentId === node.id}
                hasChildren={parents.has(node.id)}
                take={members ? members.indexOf(node) + 1 : null}
                progress={progress[node.id]}
                peaks={view.peaks}
                actions={view.actions}
              />
            )
          })}
          {nodes.map((node) => {
            const box = layout.boxes.get(node.id)
            if (!box || node.status !== 'ready' || parents.has(node.id)) return null
            const holder = shuttles.find((s) => s.node.id === node.id && s.holding)
            return (
              <div
                key={`slot-${node.id}`}
                className={`loom__slot${holder ? ' loom__slot--hot' : ''}`}
                style={{ left: box.x + box.w + LAYOUT.gap, top: box.y, width: SLOT_WIDTH - LAYOUT.gap }}
              >
                <Button onClick={(event) => view.openWeave(node.id, event.currentTarget)}>
                  {holder ? t('loom.view.holding', { deck: t(deckLetter(holder.deck)) }) : t('loom.view.whatNext')}
                </Button>
              </div>
            )
          })}
          {shuttles.map(({ deck, box, region, xAt, x, trailFrom, tag, warn }) => (
            <div key={deck} className={`loom__shuttle loom__shuttle--${deck}`}>
              {region ? (
                <div
                  className={`loom__loop${region.hold ? ' loom__loop--hold' : ''}`}
                  style={{ left: xAt(region.start), top: box.y, width: xAt(region.end) - xAt(region.start) }}
                >
                  <span>
                    {region.hold ? t('loom.shuttle.holdRegion') : t('loom.shuttle.loopRegion', { bars: region.bars })}
                  </span>
                </div>
              ) : null}
              <div className="loom__trail" style={{ left: trailFrom, top: box.y, width: Math.max(0, x - trailFrom) }} />
              <div className="loom__head" style={{ left: x, top: box.y - 8 }} />
              <div
                className={`loom__tag${warn ? ' loom__tag--warn' : ''}`}
                style={{ left: x, top: box.y - 28 }}
                title={t('loom.shuttle.dropHint')}
                onDragOver={(event) => event.preventDefault()}
                onDrop={dropOnTag(deck)}
              >
                {tag}
              </div>
            </div>
          ))}
        </div>
      </div>
      <div className="loom__zoom">
        <Button title={t('loom.view.newThread')} onClick={(event) => view.openWeave(null, event.currentTarget)}>
          {t('loom.view.newThreadMark')}
        </Button>
        <Button aria-label={t('loom.view.zoomOut')} title={t('loom.view.zoomOut')} onClick={() => view.zoomBy(0.8)}>
          {t('loom.view.zoomOutMark')}
        </Button>
        <Button title={t('loom.view.fitHint')} onClick={view.fit}>
          {t('loom.view.fit')}
        </Button>
        <Button aria-label={t('loom.view.zoomIn')} title={t('loom.view.zoomIn')} onClick={() => view.zoomBy(1.25)}>
          {t('loom.view.zoomInMark')}
        </Button>
      </div>
      {nodes.length === 0 ? (
        <div className="loom__empty">
          <p>{t('loom.empty')}</p>
          <Button variant="primary" aria-label={t('loom.view.newThread')} onClick={(event) => view.openWeave(null, event.currentTarget)}>
            {t('loom.view.newThreadMark')}
          </Button>
        </div>
      ) : null}
      <WeavePopover
        open={weaving !== null}
        anchorRef={weaveAnchor}
        parent={weaveParent}
        decks={weaveDecks}
        onWeave={(form) => props.onWeave(weaving?.parentId ?? null, form)}
        onClose={() => setWeaving(null)}
      />
    </div>
  )
}
