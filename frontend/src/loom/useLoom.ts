/** The loom's owner in the booth (plan §7): loom state, autosave, the render
 * queue (`useRenderQueue`), and the shuttle — every deck poll turns into `planDeck` and the plan
 * is applied to the engine only when it changed, so a 4 Hz poll costs a
 * comparison. Gestures fold their actions into `stateRef` at once (the next
 * gesture sees them) and React renders the copy. Deck runtime is never saved. */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { useAudioEngine } from '../audio/engineContext'
import { invoke, subscribeLibraryChanged } from '../audio/nativeEngine'
import type { DeckId, TrackSource } from '../audio/types'
import type { DeckControls } from '../deck/useDeck'
import type { SongEntry } from '../generation/library'
import { mintSa3Seed, SA3_SEED_MAX } from '../generation/songGeneration'
import type { LoopBars } from './LoomMixer'
import type { WeaveForm } from './LoomView'
import {
  auditionSongClip,
  deleteLoom as deleteLoomFile,
  listLooms,
  readLoom,
  saveLoom,
  type LoomSummary,
} from './loomLibrary'
import {
  emptyLoom,
  initialLoomState,
  isOnAir,
  kids,
  loomReducer,
  nodeById,
  parseLoom,
  persistable,
  type Loom,
  type LoomAction,
  type LoomNode,
} from './loomState'
import { renderAheadCandidate } from './renderAhead'
import { cutPlan, planDeck, type Region } from './schedule'
import { barAt, barStartSeconds, DEFAULT_BARS, DEFAULT_BPM, nodeStartSeconds } from './timing'
import { useRenderQueue } from './useRenderQueue'

const AUTOSAVE_MS = 700
const STATUS_MS = 6000
/** How long after placing a loop the engine snapshot may still show the old
 * one (a command rides the next frame; the poll can read just before it). */
const SETTLE_MS = 200
const DECKS: DeckId[] = ['a', 'b']
const LOOP_STEPS: LoopBars[] = [4, 8, 16]

const newId = () => crypto.randomUUID().toLowerCase()
const song = (name: string): TrackSource => ({ kind: 'song', name })
const near = (a: number, b: number) => Math.abs(a - b) < 0.01
const sameRegion = (a: Region | null, b: Region | null) =>
  a !== null && b !== null && near(a.start, b.start) && near(a.end, b.end)
const errorText = (error: unknown) => (error instanceof Error ? error.message : String(error))
/** A library prompt carries the render's tempo stamp; a node stores it bare. */
const unstamped = (prompt: string | null | undefined) => prompt?.replace(/,\s*[\d.]+\s*BPM\s*$/i, '') || null

type QueuedSwap = { nodeId: string; file: string; title: string; startSeconds: number; switchAt: number; cut: boolean }

/** What the engine was last told for one deck's current clip. */
type Applied = {
  loadId: number | null
  loop: Region | null
  armedAt: number
  queue: QueuedSwap | null
}

const fresh = (loadId: number | null): Applied => ({ loadId, loop: null, armedAt: 0, queue: null })

export type UseLoomOptions = { deckA: DeckControls; deckB: DeckControls; renderAhead: boolean }

export function useLoom({ deckA, deckB, renderAhead }: UseLoomOptions) {
  const { t } = useTranslation()
  const engine = useAudioEngine()
  const [state, setState] = useState(() => initialLoomState(emptyLoom('', 0)))
  const [savedLoom, setSavedLoom] = useState<Loom | null>(null)
  const [status, setStatus] = useState<string | null>(null)
  const [looms, setLooms] = useState<LoomSummary[]>([])
  const [booted, setBooted] = useState(false)

  const stateRef = useRef(state)
  const latest = useRef({ decks: { a: deckA, b: deckB }, t, engine })
  useEffect(() => {
    latest.current = { decks: { a: deckA, b: deckB }, t, engine }
  })
  const applied = useRef<Record<DeckId, Applied>>({ a: fresh(null), b: fresh(null) })
  // Monotonic per deck; `done` once the newest queue request reached the engine.
  const queueRequest = useRef<Record<DeckId, { seq: number; done: boolean }>>({
    a: { seq: 0, done: true },
    b: { seq: 0, done: true },
  })
  // A node a gesture is loading onto the deck: the next `loadId` lands there.
  const landing = useRef<Record<DeckId, string | null>>({ a: null, b: null })
  // The node each deck last ran the render-ahead check on.
  const aheadFor = useRef<Record<DeckId, string | null>>({ a: null, b: null })
  // The loom as last sent to the shell; anything else is unsaved.
  const sentLoom = useRef<Loom | null>(null)
  const saveFailed = useRef(false)
  const bootedRef = useRef(false)

  const act = useCallback((action: LoomAction) => {
    const next = loomReducer(stateRef.current, action)
    if (next === stateRef.current) return
    stateRef.current = next
    setState(next)
  }, [])

  const say = useCallback((key: string, options?: Record<string, unknown>) => {
    setStatus(latest.current.t(key, options))
  }, [])

  const { progress, pending } = useRenderQueue(state.loom, act)

  // Registry facts the loom shows: keys as the analysis lands, and files gone
  // from the songs folder (greyed, the rest plays).
  const refreshSongs = useCallback(() => {
    // Only files the loom had when asking: a render saved after the list was
    // read is not missing.
    const files = new Set(stateRef.current.loom.nodes.flatMap((n) => (n.file ? [n.file] : [])))
    invoke<SongEntry[]>('list_generated_songs').then(
      (rows) => {
        const byFile = new Map(rows.map((row) => [row.file, row]))
        for (const file of files) {
          const row = byFile.get(file)
          if (!row) act({ type: 'file_missing', file })
          else if (row.key) act({ type: 'key_known', file, key: row.key })
        }
      },
      () => {},
    )
  }, [act])

  const save = useCallback(() => {
    const current = stateRef.current
    if (!bootedRef.current || current.loom === sentLoom.current) return
    sentLoom.current = current.loom
    const loom = persistable(current, Date.now())
    saveLoom(loom.id, loom.name, loom).then(
      () => {
        saveFailed.current = false
        setSavedLoom(current.loom)
        listLooms().then(setLooms, () => {})
      },
      (error) => {
        // Unsent again, so the next change or flush retries; said once per run of failures.
        if (sentLoom.current === current.loom) sentLoom.current = null
        if (!saveFailed.current) say('loom.status.saveFailed', { message: errorText(error) })
        saveFailed.current = true
      },
    )
  }, [say])

  const gestures = useMemo(() => {
    const deckOf = (deck: DeckId) => latest.current.decks[deck]
    const nodeOf = (id: string | null) => nodeById(stateRef.current.loom.nodes, id)
    const letter = (deck: DeckId) => deck.toUpperCase()

    /** Bring the engine's queue in line with `applied[deck].queue`. Requests
     * resolve out of order (each decodes its clip first), so a request that
     * lands after a newer one has finished is re-sent from the current state. */
    const sendQueue = (deck: DeckId) => {
      const controls = deckOf(deck)
      const want = applied.current[deck].queue
      const request = queueRequest.current[deck]
      const seq = ++request.seq
      if (!want) {
        request.done = true
        controls.clearQueuedTrack()
        return
      }
      request.done = false
      controls.queueTrack(song(want.file), want.title, want.startSeconds, want.switchAt).then(
        () => {
          if (seq === request.seq) request.done = true
          else if (request.done) sendQueue(deck)
        },
        () => {
          if (seq === request.seq) say('loom.status.queueFailed', { deck: letter(deck), title: want.title })
        },
      )
    }

    /** Load a node onto the deck now, at its in-point; the poll lands it. */
    const load = async (deck: DeckId, node: LoomNode & { file: string }) => {
      const controls = deckOf(deck)
      landing.current[deck] = node.id
      const loaded = await controls.loadTrack(song(node.file), node.prompt).catch(() => false)
      if (!loaded) {
        landing.current[deck] = null
        say('loom.status.loadFailed', { deck: letter(deck), title: node.prompt })
        return false
      }
      controls.seekTrack(nodeStartSeconds(node))
      return true
    }

    /** →A is next, never now — except on a deck that is stopped or has no
     * node yet, where the node loads at its in-point. */
    const tieNode = async (deck: DeckId, node: LoomNode) => {
      const runtime = stateRef.current.decks[deck]
      if (runtime.nodeId && deckOf(deck).track?.playing) {
        act({ type: 'tie', deck, id: node.id })
        return true
      }
      if (node.status === 'ready' && node.file) return load(deck, { ...node, file: node.file })
      if (runtime.nodeId) act({ type: 'tie', deck, id: node.id })
      else say('loom.status.notReady', { title: node.prompt })
      return runtime.nodeId !== null
    }

    const tie = (deck: DeckId, id: string) => {
      const node = nodeOf(id)
      if (node) void tieNode(deck, node)
    }

    /** A loop the engine holds that the plan did not place (the FLX4's in/out). */
    const userLoopOf = (deck: DeckId): Region | null => {
      const track = deckOf(deck).track
      const was = applied.current[deck]
      const settled = Date.now() - was.armedAt >= SETTLE_MS
      return settled && track?.loop && !sameRegion(track.loop, was.loop) ? track.loop : null
    }

    const setLoop = (deck: DeckId, bars: LoopBars | null) => {
      const runtime = stateRef.current.decks[deck]
      const node = nodeOf(runtime.nodeId)
      if (!node) return
      if (bars === null) {
        act({ type: 'set_loop', deck, bars: null, startBar: null })
        return
      }
      // A running loop changes length in place; a new one starts on this bar.
      const position = deckOf(deck).track?.position ?? nodeStartSeconds(node)
      const startBar = runtime.loopBars ? runtime.loopStartBar : Math.floor(barAt(node, position) + 1e-6)
      act({ type: 'set_loop', deck, bars, startBar })
    }

    /** One deck's poll: adopt a landing, then apply the plan where it differs
     * from what the engine was last told. */
    const step = (deck: DeckId) => {
      const controls = deckOf(deck)
      const track = controls.track
      if (!track) return
      let was = applied.current[deck]
      if (track.loadId !== was.loadId) {
        const landedOn = landing.current[deck] ?? was.queue?.nodeId ?? null
        landing.current[deck] = null
        was = applied.current[deck] = fresh(track.loadId)
        // A load replaced the engine source and its queue: a request still in
        // flight is stale now, so its resolution clears what it armed.
        const request = queueRequest.current[deck]
        request.seq += 1
        request.done = true
        if (landedOn) act({ type: 'landed', deck, id: landedOn })
      }
      const userLoop = userLoopOf(deck)
      const plan = planDeck(stateRef.current, deck, userLoop)
      if (plan.kind === 'idle') return
      if (plan.kind === 'queue') {
        const q = was.queue
        const same =
          q?.nodeId === plan.nodeId &&
          (q.cut || (near(q.switchAt, plan.switchAt) && near(q.startSeconds, plan.startSeconds)))
        if (!same) {
          const title = nodeOf(plan.nodeId)?.prompt ?? plan.file
          was.queue = { nodeId: plan.nodeId, file: plan.file, title, startSeconds: plan.startSeconds, switchAt: plan.switchAt, cut: false }
          sendQueue(deck)
        }
      } else if (was.queue) {
        // A loop wins over a tie until it is released; an untie drops it.
        was.queue = null
        sendQueue(deck)
      }
      if (userLoop) return
      // The loop vanished (a seek exits it): put the floor back. Not while a
      // swap is armed — the engine hands over at the seam either way, and
      // mid-swap the old node's region would land on the new clip.
      const settled = Date.now() - was.armedAt >= SETTLE_MS
      const lost = settled && was.loop !== null && track.loop === null && track.queued === null
      if (lost || !sameRegion(plan.loop, was.loop)) {
        was.loop = plan.loop
        was.armedAt = Date.now()
        controls.setTrackLoopRegion(plan.loop.start, plan.loop.end)
      }
    }

    /** Render ahead: behind the node a playing deck is on, once per visit. */
    const growAhead = () => {
      const inFlight = new Set(pending())
      for (const deck of DECKS) {
        const nodeId = stateRef.current.decks[deck].nodeId
        if (!nodeId || !deckOf(deck).track?.playing || aheadFor.current[deck] === nodeId) continue
        aheadFor.current[deck] = nodeId
        const node = renderAheadCandidate(stateRef.current, deck, inFlight)
        if (!node) continue
        inFlight.add(node.id)
        act({
          type: 'weave',
          parentId: node.id,
          prompt: node.prompt,
          takes: 1,
          bars: DEFAULT_BARS,
          bpm: node.bpm,
          loras: [],
          sa3: node.sa3,
          seeds: [mintSa3Seed()],
          ids: [newId()],
          takeGroup: newId(),
          auto: true,
          now: Date.now(),
        })
      }
    }

    /** Leaving a loom: drop armed swaps so each deck holds its current clip
     * (the hold loop stays in the engine) instead of crossing into a node the
     * next loom does not have. */
    const detach = () => {
      for (const deck of DECKS) {
        landing.current[deck] = null
        if (!applied.current[deck].queue) continue
        applied.current[deck].queue = null
        sendQueue(deck)
      }
    }

    const addSong = async (file: string, title: string, parentId: string | null = null) => {
      const existing = stateRef.current.loom.nodes.find((n) => n.parentId === parentId && n.file === file)
      if (existing) return existing.id
      const [row, peaks] = await Promise.all([
        invoke<SongEntry[]>('list_generated_songs').then(
          (rows) => rows.find((entry) => entry.file === file) ?? null,
          () => null,
        ),
        invoke<{ duration: number }>('library_clip_peaks', { library: 'songs', name: file, buckets: 64 }),
      ])
      const id = newId()
      act({
        type: 'add_song',
        id,
        parentId,
        file,
        title: row?.title ?? title,
        bpm: row?.bpm ?? DEFAULT_BPM,
        durationSeconds: peaks.duration,
        prompt: unstamped(row?.prompt),
        now: Date.now(),
      })
      if (row?.key) act({ type: 'key_known', file, key: row.key })
      return id
    }

    const openLoom = async (id: string) => {
      save()
      const loom = parseLoom(await readLoom(id).catch(() => null))
      if (!loom) {
        say('loom.status.openFailed')
        return
      }
      detach()
      sentLoom.current = loom
      setSavedLoom(loom)
      act({ type: 'loom_loaded', loom })
      refreshSongs()
    }

    const newLoom = () => {
      save()
      detach()
      act({ type: 'loom_new', id: newId(), now: Date.now(), name: latest.current.t('loom.bar.defaultName') })
    }

    return {
      step,
      growAhead,
      detach,
      tie,
      untie: (deck: DeckId, id: string) => act({ type: 'untie', deck, id }),
      /** Shift-click or a drop on the shuttle: hand over on the next bar. */
      cut: (deck: DeckId, id: string) => {
        const node = nodeOf(id)
        const current = nodeOf(stateRef.current.decks[deck].nodeId)
        const track = deckOf(deck).track
        if (!node) return
        if (!current || !track?.playing) {
          void tieNode(deck, node)
          return
        }
        act({ type: 'tie', deck, id, front: true })
        // A cut is its own gesture: it releases whatever loop holds the deck.
        if (stateRef.current.decks[deck].loopBars) setLoop(deck, null)
        if (userLoopOf(deck)) deckOf(deck).loopExit()
        if (node.status !== 'ready' || !node.file) return
        const { startSeconds, switchAt } = cutPlan(current, track.position, node)
        applied.current[deck].queue = { nodeId: id, file: node.file, title: node.prompt, startSeconds, switchAt, cut: true }
        sendQueue(deck)
      },
      /** `id` null stops the cue; `fromBar` null = from the join. */
      cue: (id: string | null, fromBar: number | null) => {
        latest.current.engine.auditionStop()
        const node = nodeOf(id)
        if (!node?.file) {
          act({ type: 'cue', deck: 'a', id: null, fromBar: null })
          return
        }
        act({ type: 'cue', deck: 'a', id: node.id, fromBar })
        const start = barStartSeconds(node, stateRef.current.decks.a.cue?.fromBar ?? node.in)
        auditionSongClip(node.file, start).catch(() => say('loom.status.cueFailed', { title: node.prompt }))
      },
      scrub: (deck: DeckId, seconds: number) => deckOf(deck).seekTrack(seconds),
      trim: (id: string, inBar: number, outBar: number) => {
        const node = nodeOf(id)
        if (node && Math.round(outBar) !== node.out && kids(stateRef.current.loom.nodes, id).length > 0) {
          say('loom.status.trimRefused')
        }
        act({ type: 'trim', id, in: inBar, out: outBar })
      },
      keep: (id: string) => act({ type: 'keep', id }),
      discard: (id: string) => {
        if (isOnAir(stateRef.current, id) || kids(stateRef.current.loom.nodes, id).length > 0) {
          say('loom.status.discardRefused')
          return
        }
        act({ type: 'discard', id })
      },
      retry: (id: string) => act({ type: 'retry', id }),
      select: (id: string | null) => act({ type: 'select', id }),
      setAuto: (deck: DeckId, on: boolean) => act({ type: 'set_auto', deck, on }),
      setLoop,
      /** The FLX4 4 BEAT/EXIT in Loom theme: release whatever loop runs, else
       * a four-bar loom loop. */
      beatLoop: (deck: DeckId) => {
        if (stateRef.current.decks[deck].loopBars) setLoop(deck, null)
        else if (userLoopOf(deck)) deckOf(deck).loopExit()
        else setLoop(deck, 4)
      },
      /** CUE/LOOP CALL ◄/►: step a running loom loop 4 → 8 → 16, clamped. */
      stepLoop: (deck: DeckId, direction: -1 | 1) => {
        const bars = stateRef.current.decks[deck].loopBars
        if (!bars) return
        const at = Math.min(LOOP_STEPS.length - 1, Math.max(0, LOOP_STEPS.indexOf(bars) + direction))
        setLoop(deck, LOOP_STEPS[at])
      },
      weave: (parentId: string | null, form: WeaveForm) => {
        const ids = Array.from({ length: form.takes }, newId)
        const seeds = ids.map((_, i) => (form.seed === null ? mintSa3Seed() : (form.seed + i) % (SA3_SEED_MAX + 1)))
        act({
          type: 'weave',
          parentId,
          prompt: form.prompt,
          takes: form.takes,
          bars: form.bars,
          bpm: form.bpm,
          loras: form.loras,
          sa3: form.sa3,
          seeds,
          ids,
          takeGroup: newId(),
          auto: false,
          now: Date.now(),
        })
        if (form.tieTo) tie(form.tieTo, ids[0])
      },
      /** A song dropped on the canvas (or seeded from the explorer) becomes a
       * root; dropped on a node, its next thread. Either way it is selected —
       * or found, when that node already has it. */
      dropSong: (file: string, parentId: string | null = null) => {
        addSong(file, file, parentId).then(
          (id) => {
            const node = nodeOf(id)
            if (!node) return
            act({ type: 'select', id })
            const parent = nodeOf(parentId)
            if (parent) say('loom.status.songJoined', { title: node.prompt, parent: parent.prompt })
            else say('loom.status.songSeeded', { title: node.prompt })
          },
          (error) => say('loom.status.dropFailed', { file, message: errorText(error) }),
        )
      },
      /** The explorer's →A/→B and an MCP load in Loom theme: the song becomes a
       * root and is tied like any node. */
      loadSong: async (deck: DeckId, source: TrackSource, title: string) => {
        if (source.kind !== 'song') throw new Error(latest.current.t('loom.status.songsOnly'))
        const id = await addSong(source.name, title)
        const node = nodeOf(id)
        return node ? tieNode(deck, node) : false
      },
      rename: (name: string) => act({ type: 'rename', name }),
      newLoom,
      openLoom,
      duplicateLoom: () => {
        save()
        detach()
        const now = Date.now()
        const { loom } = stateRef.current
        const name = latest.current.t('loom.bar.copyName', { name: loom.name })
        act({ type: 'loom_loaded', loom: { ...loom, id: newId(), name, createdAt: now, updatedAt: now } })
      },
      /** The file goes to the Trash; its songs stay. The newest other loom opens. */
      deleteLoom: async () => {
        const { loom } = stateRef.current
        sentLoom.current = loom
        try {
          await deleteLoomFile(loom.id)
        } catch (error) {
          sentLoom.current = null
          say('loom.status.deleteFailed', { message: errorText(error) })
          return
        }
        const list = await listLooms().catch(() => [])
        setLooms(list)
        const next = list.find((summary) => summary.id !== loom.id)
        if (next) await openLoom(next.id)
        else newLoom()
      },
    }
  }, [act, pending, refreshSongs, save, say])

  // Boot: the newest loom, or a fresh one.
  useEffect(() => {
    let live = true
    listLooms()
      .then(async (list) => {
        if (live) setLooms(list)
        return list.length > 0 ? parseLoom(await readLoom(list[0].id)) : null
      })
      .catch(() => null)
      .then((loom) => {
        if (!live) return
        if (loom) {
          sentLoom.current = loom
          setSavedLoom(loom)
          act({ type: 'loom_loaded', loom })
          refreshSongs()
        } else {
          act({ type: 'loom_new', id: newId(), now: Date.now(), name: latest.current.t('loom.bar.defaultName') })
        }
        bootedRef.current = true
        setBooted(true)
      })
    return () => {
      live = false
    }
  }, [act, refreshSongs])

  // Once, with the latest refresh read through the closure's stable callback.
  useEffect(
    () =>
      subscribeLibraryChanged((library) => {
        if (library === 'songs') refreshSongs()
      }),
    [refreshSongs],
  )

  // Autosave, debounced; the unmount flush keeps the last change of a set.
  useEffect(() => {
    if (!booted) return
    const timer = setTimeout(save, AUTOSAVE_MS)
    return () => clearTimeout(timer)
  }, [state.loom, booted, save])
  useEffect(() => () => save(), [save])
  // Theme → DJ unmounts the booth: no swap stays armed on a deck the loom no
  // longer drives.
  useEffect(() => () => gestures.detach(), [gestures])

  useEffect(() => {
    if (!status) return
    const timer = setTimeout(() => setStatus(null), STATUS_MS)
    return () => clearTimeout(timer)
  }, [status])

  // The shuttle: every deck poll (and every loom change) re-plans both decks.
  const trackA = deckA.track
  const trackB = deckB.track
  useEffect(() => {
    for (const deck of DECKS) gestures.step(deck)
    if (renderAhead) gestures.growAhead()
  }, [state, trackA, trackB, renderAhead, gestures])

  return {
    state,
    progress,
    status,
    looms,
    saved: !booted || state.loom === savedLoom ? ('saved' as const) : ('saving' as const),
    /** A line in the loom bar for 6 s; the newest line replaces the last. */
    say,
    ...gestures,
  }
}

export type LoomControls = ReturnType<typeof useLoom>
