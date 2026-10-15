import { describe, expect, it, vi } from 'vitest'

import { initialDeckState, type DeckState } from '../deck/deckState'
import type { DeckControls } from '../deck/useDeck'
import {
  applyAppIntent,
  JOG_NUDGE_SECONDS,
  JOG_SCRUB_SECONDS,
  JOG_SEEK_SECONDS,
} from './appIntents'

function fakeDeck(state: Partial<DeckState> = {}): DeckControls {
  return {
    state: { ...initialDeckState, connection: 'open', ...state },
    volume: 0.8,
    eq: { low: 0.5, mid: 0.5, high: 0.5 },
    cue: false,
    setCue: vi.fn(),
    fx: { kind: null, amount: 0 },
    setFx: vi.fn(),
    setFxAmount: vi.fn(),
    loop: {
      slots: Array.from({ length: 4 }, () => ({ state: 'empty' as const })),
      active: null,
      layering: [],
      seconds: 4,
    },
    toggleLoopPad: vi.fn(),
    clearLoopPad: vi.fn(),
    setLoopSeconds: vi.fn(),
    generateToPad: vi.fn(),
    loadSampleToSlot: vi.fn(async () => true),
    generateError: null,
    bpm: null,
    captureStyleSample: vi.fn(async () => null),
    mode: 'realtime' as const,
    track: null,
    loadTrack: vi.fn(async () => true),
    leavePlayback: vi.fn(),
    seekTrack: vi.fn(),
    nudgeTrack: vi.fn(),
    setTrackRate: vi.fn(),
    nudgeTrackPhase: vi.fn(),
    syncTrack: vi.fn(() => ({ status: 'synced' }) as const),
    hotCuePad: vi.fn(),
    clearHotCue: vi.fn(),
    loopIn: vi.fn(),
    loopOut: vi.fn(),
    loopExit: vi.fn(),
    beatLoop: vi.fn(),
    halveLoop: vi.fn(),
    doubleLoop: vi.fn(),
    getTrackBeat: vi.fn(() => null),
    getLiveBeat: vi.fn(() => null),
    getZoomSource: vi.fn(() => null),
    getTrackPeaks: vi.fn(() => null),
    trim: { mode: 'auto' as const, db: 0 },
    setTrimDb: vi.fn(),
    enableAutoTrim: vi.fn(),
    primed: false,
    prime: vi.fn(async () => {}),
    play: vi.fn(async () => {}),
    stop: vi.fn(),
    setModel: vi.fn(),
    restartWorker: vi.fn(),
    setVolume: vi.fn(),
    setEqBand: vi.fn(),
    getChannelLevel: () => 0,
  }
}

function decks(a = fakeDeck(), b = fakeDeck()) {
  return { a, b }
}

const noHandlers = { onCrossfade: () => {}, onCueMix: () => {} }

describe('applyAppIntent', () => {
  it('starts a stopped deck on play_toggle', () => {
    const a = fakeDeck({ playing: false })
    applyAppIntent({ kind: 'play_toggle', deck: 'a' }, decks(a), noHandlers)
    expect(a.play).toHaveBeenCalled()
    expect(a.stop).not.toHaveBeenCalled()
  })

  it('stops a playing deck on play_toggle', () => {
    const b = fakeDeck({ playing: true })
    applyAppIntent(
      { kind: 'play_toggle', deck: 'b' },
      decks(fakeDeck(), b),
      noHandlers,
    )
    expect(b.stop).toHaveBeenCalled()
    expect(b.play).not.toHaveBeenCalled()
  })

  it.each([
    ['loading a model', { switchingModel: true }],
    ['crashed', { workerDied: true }],
  ])('refuses play_toggle while the deck is %s', (_label, state) => {
    const a = fakeDeck(state)
    applyAppIntent({ kind: 'play_toggle', deck: 'a' }, decks(a), noHandlers)
    expect(a.play).not.toHaveBeenCalled()
    expect(a.stop).not.toHaveBeenCalled()
  })

  it('routes volume to the addressed deck only', () => {
    const a = fakeDeck()
    const b = fakeDeck()
    applyAppIntent({ kind: 'volume', deck: 'b', value: 0.3 }, decks(a, b), noHandlers)
    expect(b.setVolume).toHaveBeenCalledWith(0.3)
    expect(a.setVolume).not.toHaveBeenCalled()
  })

  it('maps a trim turn to a manual gain on the addressed deck', () => {
    const a = fakeDeck()
    const b = fakeDeck()
    // Mid-travel is 0 dB; the ends hit ±TRIM_RANGE_DB (12).
    applyAppIntent({ kind: 'trim', deck: 'b', value: 0.5 }, decks(a, b), noHandlers)
    expect(b.setTrimDb).toHaveBeenCalledWith(0)
    applyAppIntent({ kind: 'trim', deck: 'b', value: 1 }, decks(a, b), noHandlers)
    expect(b.setTrimDb).toHaveBeenLastCalledWith(12)
    expect(a.setTrimDb).not.toHaveBeenCalled()
  })

  it('routes EQ band moves to the addressed deck', () => {
    const a = fakeDeck()
    applyAppIntent(
      { kind: 'eq', deck: 'a', band: 'mid', value: 0.7 },
      decks(a),
      noHandlers,
    )
    expect(a.setEqBand).toHaveBeenCalledWith('mid', 0.7)
  })

  it('toggles the addressed deck headphone cue', () => {
    const a = fakeDeck()
    const b = { ...fakeDeck(), cue: true }
    applyAppIntent({ kind: 'cue_toggle', deck: 'a' }, decks(a, b), noHandlers)
    expect(a.setCue).toHaveBeenCalledWith(true)
    applyAppIntent({ kind: 'cue_toggle', deck: 'b' }, decks(a, b), noHandlers)
    expect(b.setCue).toHaveBeenCalledWith(false)
  })

  it('primes a stopped deck on deck_prep and stops a rolling one', () => {
    const stopped = fakeDeck({ playing: false })
    applyAppIntent({ kind: 'deck_prep', deck: 'a' }, decks(stopped), noHandlers)
    expect(stopped.prime).toHaveBeenCalled()
    expect(stopped.stop).not.toHaveBeenCalled()

    const rolling = fakeDeck({ playing: true })
    applyAppIntent({ kind: 'deck_prep', deck: 'a' }, decks(rolling), noHandlers)
    expect(rolling.stop).toHaveBeenCalled()
    expect(rolling.prime).not.toHaveBeenCalled()
  })

  it('refuses deck_prep while the deck cannot take it', () => {
    const a = fakeDeck({ switchingModel: true })
    applyAppIntent({ kind: 'deck_prep', deck: 'a' }, decks(a), noHandlers)
    expect(a.prime).not.toHaveBeenCalled()
    expect(a.stop).not.toHaveBeenCalled()
  })

  it('routes the FX amount to the addressed deck', () => {
    const a = fakeDeck()
    const b = fakeDeck()
    applyAppIntent(
      { kind: 'fx_amount', deck: 'b', value: 0.7 },
      decks(a, b),
      noHandlers,
    )
    expect(b.setFxAmount).toHaveBeenCalledWith(0.7)
    expect(a.setFxAmount).not.toHaveBeenCalled()
  })

  it('selects an effect from a PAD FX pad and toggles it off on repeat', () => {
    const a = fakeDeck()
    applyAppIntent({ kind: 'fx_select', deck: 'a', index: 1 }, decks(a), noHandlers)
    expect(a.setFx).toHaveBeenCalledWith('dub_echo')

    const echoed = { ...fakeDeck(), fx: { kind: 'dub_echo' as const, amount: 0.4 } }
    applyAppIntent(
      { kind: 'fx_select', deck: 'a', index: 1 },
      decks(echoed),
      noHandlers,
    )
    expect(echoed.setFx).toHaveBeenCalledWith(null)
  })

  it('ignores PAD FX pads beyond the effect list', () => {
    const a = fakeDeck()
    applyAppIntent({ kind: 'fx_select', deck: 'a', index: 7 }, decks(a), noHandlers)
    expect(a.setFx).not.toHaveBeenCalled()
  })

  it('routes loop pads to the addressed deck', () => {
    const a = fakeDeck()
    const b = fakeDeck()
    applyAppIntent({ kind: 'loop_pad', deck: 'b', index: 2 }, decks(a, b), noHandlers)
    expect(b.toggleLoopPad).toHaveBeenCalledWith(2)
    expect(a.toggleLoopPad).not.toHaveBeenCalled()
  })

  it('routes loop clears to the addressed deck', () => {
    const a = fakeDeck()
    const b = fakeDeck()
    applyAppIntent(
      { kind: 'loop_clear', deck: 'a', index: 0 },
      decks(a, b),
      noHandlers,
    )
    expect(a.clearLoopPad).toHaveBeenCalledWith(0)
    expect(b.clearLoopPad).not.toHaveBeenCalled()
  })

  it('hands crossfade to the callback', () => {
    const onCrossfade = vi.fn()
    applyAppIntent(
      { kind: 'crossfade', value: 0.25 },
      decks(),
      { ...noHandlers, onCrossfade },
    )
    expect(onCrossfade).toHaveBeenCalledWith(0.25)
  })

  it('hands the cue mix to the callback', () => {
    const onCueMix = vi.fn()
    applyAppIntent(
      { kind: 'cue_mix', value: 0.9 },
      decks(),
      { ...noHandlers, onCueMix },
    )
    expect(onCueMix).toHaveBeenCalledWith(0.9)
  })

  it('leaves style and record intents to their owners', () => {
    const a = fakeDeck()
    const onCrossfade = vi.fn()
    const handlers = { ...noHandlers, onCrossfade }
    applyAppIntent(
      { kind: 'hot_cue_pad', deck: 'a', index: 0 },
      decks(a),
      handlers,
    )
    applyAppIntent({ kind: 'record_toggle' }, decks(a), handlers)
    expect(a.play).not.toHaveBeenCalled()
    expect(onCrossfade).not.toHaveBeenCalled()
  })

  function playbackDeck(
    playing: boolean,
    loop: { start: number; end: number } | null = null,
  ) {
    return {
      ...fakeDeck(),
      mode: 'playback' as const,
      track: {
        loadId: 1,
        title: 'Warehouse Anthem',
        duration: 120,
        position: 30,
        playing,
        ended: false,
        bpm: null,
        grid: null,
        rate: 1,
        cues: Array<number | null>(8).fill(null),
        loop,
        pendingLoopIn: null,
      },
    }
  }

  it('play_toggle on a playback deck answers to the track, not the worker', () => {
    // state.playing is honestly false while the worker is parked; the
    // pause decision must come from the track (M19, ADR-0013).
    const playing = playbackDeck(true)
    applyAppIntent({ kind: 'play_toggle', deck: 'a' }, decks(playing), noHandlers)
    expect(playing.stop).toHaveBeenCalled()
    expect(playing.play).not.toHaveBeenCalled()

    const paused = playbackDeck(false)
    applyAppIntent({ kind: 'play_toggle', deck: 'a' }, decks(paused), noHandlers)
    expect(paused.play).toHaveBeenCalled()
    expect(paused.stop).not.toHaveBeenCalled()
  })

  it('deck_prep on a playback deck returns the track to the top', () => {
    const playing = playbackDeck(true)
    applyAppIntent({ kind: 'deck_prep', deck: 'a' }, decks(playing), noHandlers)
    expect(playing.prime).toHaveBeenCalled()
    expect(playing.stop).not.toHaveBeenCalled()
  })

  it('jog ticks seek a parked track, scaled to seconds', () => {
    const deck = playbackDeck(false)
    applyAppIntent(
      { kind: 'track_seek', deck: 'a', steps: 3, shifted: false },
      decks(deck),
      noHandlers,
    )
    expect(deck.nudgeTrack).toHaveBeenCalledWith(3 * JOG_SEEK_SECONDS)
    expect(deck.nudgeTrackPhase).not.toHaveBeenCalled()
  })

  it('jog ticks nudge phase while the track plays — the platter dual role (M20)', () => {
    const deck = playbackDeck(true)
    applyAppIntent(
      { kind: 'track_seek', deck: 'a', steps: -2, shifted: false },
      decks(deck),
      noHandlers,
    )
    expect(deck.nudgeTrackPhase).toHaveBeenCalledWith(-2 * JOG_NUDGE_SECONDS)
    expect(deck.nudgeTrack).not.toHaveBeenCalled()
  })

  it('does not scrub a playback deck while a live deck is SHIFT-steered', () => {
    // Deck B (live) is being steered; its borrowed jog (here deck A's) must not
    // also scrub deck A's track.
    const playback = playbackDeck(false)
    applyAppIntent(
      { kind: 'track_seek', deck: 'a', steps: 3, shifted: false },
      decks(playback, fakeDeck()),
      noHandlers,
      'b',
    )
    expect(playback.nudgeTrack).not.toHaveBeenCalled()
    expect(playback.nudgeTrackPhase).not.toHaveBeenCalled()
  })

  it('still scrubs when the SHIFT-held deck is itself in playback', () => {
    // Holding SHIFT on a playback deck is a normal scrub — only a realtime
    // steered deck claims the jogs.
    const deck = playbackDeck(true)
    applyAppIntent(
      { kind: 'track_seek', deck: 'a', steps: 3, shifted: true },
      decks(deck),
      noHandlers,
      'a',
    )
    expect(deck.nudgeTrack).toHaveBeenCalledWith(3 * JOG_SCRUB_SECONDS)
  })

  it('SHIFT+jog fast-scrubs even while playing — the CDJ search convention', () => {
    const deck = playbackDeck(true)
    applyAppIntent(
      { kind: 'track_seek', deck: 'a', steps: 3, shifted: true },
      decks(deck),
      noHandlers,
    )
    expect(deck.nudgeTrack).toHaveBeenCalledWith(3 * JOG_SCRUB_SECONDS)
    expect(deck.nudgeTrackPhase).not.toHaveBeenCalled()
  })

  it('tempo slider rides the rate on a playback deck only (M20)', () => {
    const deck = playbackDeck(true)
    applyAppIntent(
      { kind: 'track_rate', deck: 'a', value: 0 },
      decks(deck),
      noHandlers,
    )
    // Low MIDI values are the slow end — orientation measured on the
    // device after the chart assumption shipped inverted.
    expect(deck.setTrackRate).toHaveBeenCalledWith(0.92)
    // The slider's throw follows the chosen tempo range.
    applyAppIntent(
      { kind: 'track_rate', deck: 'a', value: 1 },
      decks(deck),
      noHandlers,
      null,
      0.16,
    )
    expect(deck.setTrackRate).toHaveBeenLastCalledWith(expect.closeTo(1.16, 5))

    const live = fakeDeck({ playing: true })
    applyAppIntent(
      { kind: 'track_rate', deck: 'a', value: 0 },
      decks(live),
      noHandlers,
    )
    expect(live.setTrackRate).not.toHaveBeenCalled()
  })

  it('HOT CUE pads mean position on a playback deck (M21)', () => {
    const deck = playbackDeck(true)
    applyAppIntent(
      { kind: 'hot_cue_pad', deck: 'a', index: 3 },
      decks(deck),
      noHandlers,
    )
    expect(deck.hotCuePad).toHaveBeenCalledWith(3)

    applyAppIntent(
      { kind: 'hot_cue_clear', deck: 'a', index: 3 },
      decks(deck),
      noHandlers,
    )
    expect(deck.clearHotCue).toHaveBeenCalledWith(3)
  })

  it('a realtime deck leaves the pad gesture to its style owner', () => {
    const deck = fakeDeck()
    applyAppIntent(
      { kind: 'hot_cue_pad', deck: 'a', index: 3 },
      decks(deck),
      noHandlers,
    )
    applyAppIntent(
      { kind: 'hot_cue_clear', deck: 'a', index: 3 },
      decks(deck),
      noHandlers,
    )
    expect(deck.hotCuePad).not.toHaveBeenCalled()
    expect(deck.clearHotCue).not.toHaveBeenCalled()
  })

  it('routes the LOOP section to a playback deck only (M21)', () => {
    const deck = playbackDeck(true)
    applyAppIntent({ kind: 'track_loop_in', deck: 'a' }, decks(deck), noHandlers)
    applyAppIntent({ kind: 'track_loop_out', deck: 'a' }, decks(deck), noHandlers)
    expect(deck.loopIn).toHaveBeenCalled()
    expect(deck.loopOut).toHaveBeenCalled()

    const live = fakeDeck({ playing: true })
    applyAppIntent({ kind: 'track_loop_in', deck: 'a' }, decks(live), noHandlers)
    expect(live.loopIn).not.toHaveBeenCalled()
  })

  it('the 4 BEAT/EXIT button toggles: sets when idle, exits when a loop runs (M23)', () => {
    // No loop: the one button drops a fresh 4-beat loop.
    const idle = playbackDeck(true)
    applyAppIntent(
      { kind: 'track_beat_loop', deck: 'a', beats: 4 },
      decks(idle),
      noHandlers,
    )
    expect(idle.beatLoop).toHaveBeenCalledWith(4)
    expect(idle.loopExit).not.toHaveBeenCalled()

    // Loop running: the same button releases it, reusing loopExit.
    const running = playbackDeck(true, { start: 8, end: 10 })
    applyAppIntent(
      { kind: 'track_beat_loop', deck: 'a', beats: 4 },
      decks(running),
      noHandlers,
    )
    expect(running.loopExit).toHaveBeenCalled()
    expect(running.beatLoop).not.toHaveBeenCalled()
  })

  it('routes halve and double to a playback deck only (M23)', () => {
    const deck = playbackDeck(true)
    applyAppIntent({ kind: 'track_loop_halve', deck: 'a' }, decks(deck), noHandlers)
    applyAppIntent({ kind: 'track_loop_double', deck: 'a' }, decks(deck), noHandlers)
    expect(deck.halveLoop).toHaveBeenCalled()
    expect(deck.doubleLoop).toHaveBeenCalled()

    const live = fakeDeck({ playing: true })
    applyAppIntent({ kind: 'track_loop_halve', deck: 'a' }, decks(live), noHandlers)
    applyAppIntent(
      { kind: 'track_beat_loop', deck: 'a', beats: 4 },
      decks(live),
      noHandlers,
    )
    expect(live.halveLoop).not.toHaveBeenCalled()
    expect(live.beatLoop).not.toHaveBeenCalled()
  })

  it('a realtime deck ignores jog ticks — still no scratch concept', () => {
    const deck = fakeDeck({ playing: true })
    applyAppIntent(
      { kind: 'track_seek', deck: 'a', steps: 3, shifted: false },
      decks(deck),
      noHandlers,
    )
    expect(deck.nudgeTrack).not.toHaveBeenCalled()
  })
})
