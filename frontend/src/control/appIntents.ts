import type { DeckId } from '../audio/types'
import { FX_KINDS } from '../audio/fx'
import { trimDbFromKnob } from '../audio/master'
import { tempoSliderToRate, TRACK_RATE_RANGE } from '../audio/track'
import { isDeckOperable } from '../deck/deckState'
import type { DeckControls } from '../deck/useDeck'
import type { ControlIntent } from './bus'

export type AppIntentHandlers = {
  onCrossfade: (position: number) => void
  onCueMix: (position: number) => void
}

/** Seconds per plain jog tick on a paused track: fine cueing, the
 * CDJ paused-platter convention. The 0.5 that shipped first read as
 * "way too sensitive" on the device — the platter packs dozens of
 * ticks into a casual turn. */
export const JOG_SEEK_SECONDS = 0.05
/** Seconds per SHIFT+jog tick: the fast search. Deliberately coarse —
 * a spin should cross bars, not nudge frames. */
export const JOG_SCRUB_SECONDS = 0.5
/** Phase slip per jog tick while the track plays (M20): a platter
 * drag, milliseconds at a time — verified by feel on the device. */
export const JOG_NUDGE_SECONDS = 0.01

/** The App-owned slice of the intent union: transport, deck prep, channel
 * volume/EQ/cue, the crossfader, and the cue mix. Style and record intents
 * are handled where that state lives (DeckColumn, MixerStrip). Pure
 * dispatch so the mapping is testable without rendering the app. */
export function applyAppIntent(
  intent: ControlIntent,
  decks: Record<DeckId, DeckControls>,
  handlers: AppIntentHandlers,
  shiftedDeck: DeckId | null = null,
  tempoRange: number = TRACK_RATE_RANGE,
): void {
  switch (intent.kind) {
    case 'play_toggle': {
      const deck = decks[intent.deck]
      // A playback deck answers to its track, not the worker — the
      // worker is parked and state.playing is honestly false (M19).
      if (deck.mode === 'playback') {
        if (deck.track?.playing) deck.stop()
        else void deck.play()
        return
      }
      // Same gating as the transport button: hardware must not start a
      // deck the UI would refuse to.
      if (!isDeckOperable(deck.state)) return
      if (deck.state.playing) deck.stop()
      else void deck.play()
      return
    }
    case 'deck_prep': {
      const deck = decks[intent.deck]
      // CUE on a track returns it to the top, parked (prime carries
      // the playback-mode semantics, ADR-0013).
      if (deck.mode === 'playback') {
        void deck.prime()
        return
      }
      if (!isDeckOperable(deck.state)) return
      // CUE on a rolling deck (primed or on air) stops with flush; on a
      // stopped deck it primes — generation audible only over the cue tap.
      if (deck.state.playing) deck.stop()
      else void deck.prime()
      return
    }
    case 'volume':
      decks[intent.deck].setVolume(intent.value)
      return
    case 'trim':
      // Mirror the on-screen TRIM knob; setting a manual gain drops auto-trim.
      decks[intent.deck].setTrimDb(trimDbFromKnob(intent.value))
      return
    case 'eq':
      decks[intent.deck].setEqBand(intent.band, intent.value)
      return
    case 'cue_toggle': {
      const deck = decks[intent.deck]
      deck.setCue(!deck.cue)
      return
    }
    case 'fx_amount':
      decks[intent.deck].setFxAmount(intent.value)
      return
    case 'fx_select': {
      // PAD FX bank: pads 1–6 are the six effects; pressing the active
      // effect's pad toggles it off, like a hardware FX section.
      const kind = FX_KINDS[intent.index]
      if (!kind) return
      const deck = decks[intent.deck]
      deck.setFx(deck.fx.kind === kind ? null : kind)
      return
    }
    case 'loop_pad':
      // SAMPLER bank (M13): empty captures-and-freezes, filled swaps
      // in, active returns to live — the semantics live in useDeck so
      // the on-screen pads behave identically.
      decks[intent.deck].toggleLoopPad(intent.index)
      return
    case 'loop_clear':
      decks[intent.deck].clearLoopPad(intent.index)
      return
    case 'track_seek': {
      const deck = decks[intent.deck]
      // Jog ticks only mean something on a playback deck; the live stream keeps
      // its no-scratch stance (ADR-0004). A realtime deck's jog is handled in
      // DeckColumn (reel the net's dots, or — under SHIFT — steer the cursor).
      if (deck.mode !== 'playback') return
      // SHIFT-steering a *live* deck borrows the other deck's jog for its second
      // axis, so while that's happening the borrowed jog must not also scrub a
      // playback deck on the same tick. Only a realtime steered deck claims the
      // jogs — holding SHIFT on a playback deck is a normal scrub.
      if (shiftedDeck != null && decks[shiftedDeck].mode === 'realtime') return
      // The dual role of a real platter (M20): playing = phase nudge,
      // paused = fine seek — and SHIFT+jog fast-scrubs regardless
      // (the CDJ search convention).
      if (intent.shifted) {
        deck.nudgeTrack(intent.steps * JOG_SCRUB_SECONDS)
      } else if (deck.track?.playing) {
        deck.nudgeTrackPhase(intent.steps * JOG_NUDGE_SECONDS)
      } else {
        deck.nudgeTrack(intent.steps * JOG_SEEK_SECONDS)
      }
      return
    }
    case 'track_rate': {
      const deck = decks[intent.deck]
      // Varispeed is a playback parameter; a realtime deck ignores
      // the slider — ADR-0004 stands for generation.
      if (deck.mode !== 'playback') return
      deck.setTrackRate(tempoSliderToRate(intent.value, tempoRange))
      return
    }
    case 'hot_cue_pad': {
      // Pads mean position on a playback deck (M21, ADR-0015); the
      // realtime style-snap meaning lives where that state does
      // (DeckColumn), gated on mode there.
      const deck = decks[intent.deck]
      if (deck.mode === 'playback') deck.hotCuePad(intent.index)
      return
    }
    case 'hot_cue_clear': {
      const deck = decks[intent.deck]
      if (deck.mode === 'playback') deck.clearHotCue(intent.index)
      return
    }
    case 'track_loop_in': {
      const deck = decks[intent.deck]
      if (deck.mode === 'playback') deck.loopIn()
      return
    }
    case 'track_loop_out': {
      const deck = decks[intent.deck]
      if (deck.mode === 'playback') deck.loopOut()
      return
    }
    case 'track_beat_loop': {
      const deck = decks[intent.deck]
      if (deck.mode !== 'playback') return
      // The FLX4 "4 BEAT/EXIT" is one button (ADR-0016): exit a running
      // loop, else drop a fresh beat loop. Exit reuses loopExit so the
      // release path stays single. The on-screen 4-beat button calls
      // beatLoop directly (set-only) — it has its own EXIT.
      if (deck.track?.loop) deck.loopExit()
      else deck.beatLoop(intent.beats)
      return
    }
    case 'track_loop_halve': {
      const deck = decks[intent.deck]
      if (deck.mode === 'playback') deck.halveLoop()
      return
    }
    case 'track_loop_double': {
      const deck = decks[intent.deck]
      if (deck.mode === 'playback') deck.doubleLoop()
      return
    }
    case 'crossfade':
      handlers.onCrossfade(intent.value)
      return
    case 'cue_mix':
      handlers.onCueMix(intent.value)
      return
  }
}
