import { beforeEach, describe, expect, it } from 'vitest'

import {
  loadAppSettings,
  loadDeckSettings,
  takeLegacyDeckStyles,
  takeLegacyMixerSettings,
  takeLegacyShellSettings,
  updateAppSettings,
  updateDeckSettings,
} from './persistence'

beforeEach(() => localStorage.clear())

describe('persistence', () => {
  it('round-trips deck settings and merges partial updates', () => {
    updateDeckSettings('a', { loopSeconds: 8 })
    updateDeckSettings('a', { trimMode: 'manual' })
    expect(loadDeckSettings('a')).toEqual({
      loopSeconds: 8,
      trimMode: 'manual',
    })
  })

  it('keeps decks independent', () => {
    updateDeckSettings('a', { trimMode: 'manual' })
    updateDeckSettings('b', { trimMode: 'auto' })
    expect(loadDeckSettings('a').trimMode).toBe('manual')
    expect(loadDeckSettings('b').trimMode).toBe('auto')
  })

  it('round-trips the loop length and drops off-menu values', () => {
    updateDeckSettings('a', { loopSeconds: 8 })
    expect(loadDeckSettings('a').loopSeconds).toBe(8)

    localStorage.setItem(
      'lsdj:v1',
      JSON.stringify({ decks: { a: { loopSeconds: 7 } } }),
    )
    expect(loadDeckSettings('a').loopSeconds).toBeUndefined()
  })

  it('drops a garbage trim mode', () => {
    localStorage.setItem(
      'lsdj:v1',
      JSON.stringify({ decks: { a: { trimMode: 'loud' } } }),
    )
    expect(loadDeckSettings('a').trimMode).toBeUndefined()
  })

  it('round-trips app settings', () => {
    updateAppSettings({ beatView: 'top' })
    updateAppSettings({ mediaOpen: true })
    expect(loadAppSettings()).toEqual({
      beatView: 'top',
      mediaOpen: true,
    })
  })

  it('migrates legacy mixer settings ONCE, stripping the keys (ADR-0020 phase C)', () => {
    // A blob saved by a pre-inversion build: the whole mixer in localStorage.
    localStorage.setItem(
      'lsdj:v1',
      JSON.stringify({
        decks: {
          a: {
            volume: 0.6,
            eq: { low: 0, mid: 0.5, high: 2 },
            fx: { kind: 'dub_echo', amount: 0.4 },
            trim: { mode: 'manual', db: -40 },
            loopSeconds: 8,
          },
          b: { fx: { kind: 'megaverb', amount: 2 } },
        },
        app: { crossfade: 0.8, cueMix: -1, beatView: 'top' },
      }),
    )
    expect(takeLegacyMixerSettings()).toEqual({
      decks: {
        a: {
          volume: 0.6,
          eq: { low: 0, mid: 0.5, high: 1 }, // clamped
          fx: { kind: 'dub_echo', amount: 0.4 },
          trimDb: -12, // clamped to the trim range
        },
        // Deck B's malformed FX contributes nothing (and is stripped).
      },
      crossfade: 0.8,
      cueMix: 0, // clamped
    })
    // The legacy trim MODE survives under its new webview-owned key.
    expect(loadDeckSettings('a')).toEqual({ loopSeconds: 8, trimMode: 'manual' })
    // The surviving settings are untouched; a second take finds nothing.
    expect(loadAppSettings()).toEqual({ beatView: 'top' })
    expect(takeLegacyMixerSettings()).toBeNull()
  })

  it('migrates legacy device/folder settings ONCE, stripping the keys (ADR-0020 phase A)', () => {
    // A blob saved by a pre-inversion build: shell-owned settings still in
    // localStorage beside the surviving webview ones.
    localStorage.setItem(
      'lsdj:v1',
      JSON.stringify({
        app: {
          beatView: 'top',
          outputDevice: 'MacBook Speakers',
          cueDevice: 'DDJ-FLX4',
          recordingsFolder: '/Users/dj/Sets',
        },
      }),
    )
    expect(takeLegacyShellSettings()).toEqual({
      outputDevice: 'MacBook Speakers',
      cueDevice: 'DDJ-FLX4',
      recordingsFolder: '/Users/dj/Sets',
    })
    // The keys are gone; the surviving settings are untouched; a second take
    // finds nothing (the shell file owns them now).
    expect(loadAppSettings()).toEqual({ beatView: 'top' })
    expect(takeLegacyShellSettings()).toBeNull()
  })

  it('round-trips the beat view layout and drops garbage (M22)', () => {
    updateAppSettings({ beatView: 'top' })
    expect(loadAppSettings().beatView).toBe('top')
    updateAppSettings({ beatView: 'vertical' })
    expect(loadAppSettings().beatView).toBe('vertical')
    updateAppSettings({ beatView: 'sideways' as never })
    expect(loadAppSettings().beatView).toBeUndefined()
  })

  it('round-trips the media tray drawer state', () => {
    updateAppSettings({ mediaOpen: false, mediaHeight: 320 })
    expect(loadAppSettings().mediaOpen).toBe(false)
    expect(loadAppSettings().mediaHeight).toBe(320)
  })

  it('round-trips the Generate mode and drops garbage', () => {
    updateAppSettings({ generationMode: 'advanced' })
    expect(loadAppSettings().generationMode).toBe('advanced')
    updateAppSettings({ generationMode: 'expert' as never })
    expect(loadAppSettings().generationMode).toBeUndefined()
  })

  it('ignores garbage in the legacy shell-setting keys during migration', () => {
    localStorage.setItem(
      'lsdj:v1',
      JSON.stringify({ app: { recordingsFolder: 42, outputDevice: '' } }),
    )
    // Nothing valid to migrate — and nothing to migrate is null, not {}.
    expect(takeLegacyShellSettings()).toBeNull()
  })

  it('clamps the media height and drops a non-boolean open flag', () => {
    localStorage.setItem(
      'lsdj:v1',
      JSON.stringify({ app: { mediaOpen: 'yes', mediaHeight: 5000 } }),
    )
    const loaded = loadAppSettings()
    expect(loaded.mediaOpen).toBeUndefined() // not a boolean → dropped
    expect(loaded.mediaHeight).toBe(720) // clamped to MEDIA_MAX_HEIGHT
  })

  it('treats corrupt storage as absent', () => {
    localStorage.setItem('lsdj:v1', '{nope')
    expect(loadDeckSettings('a')).toEqual({})
    expect(loadAppSettings()).toEqual({})
  })

  it('drops malformed fields but keeps valid ones', () => {
    localStorage.setItem(
      'lsdj:v1',
      JSON.stringify({
        decks: {
          a: {
            volume: 'loud',
            loopSeconds: 8,
          },
        },
        app: { crossfade: 'middle' },
      }),
    )
    expect(loadDeckSettings('a')).toEqual({ loopSeconds: 8 })
    expect(loadAppSettings()).toEqual({})
  })

  it('migrates legacy deck-style layouts ONCE, stripping the keys (ADR-0020 phase B)', () => {
    // A blob saved by a pre-inversion build: the pad arrangement still in
    // localStorage beside the surviving webview deck settings.
    localStorage.setItem(
      'lsdj:v1',
      JSON.stringify({
        decks: {
          a: {
            targets: [{ text: 'funk', x: 0.5, y: 1.7 }],
            cursor: { x: 0.4, y: 0.6 },
            loopSeconds: 8,
          },
          b: { cursor: { x: 0.1, y: 0.1 } },
        },
      }),
    )
    // Deck A migrates (coordinates clamped); deck B has no targets, so its
    // orphan cursor is stripped without producing a migration entry.
    expect(takeLegacyDeckStyles()).toEqual({
      a: {
        targets: [{ text: 'funk', x: 0.5, y: 1 }],
        cursor: { x: 0.4, y: 0.6 },
      },
    })
    // The keys are gone; the surviving settings are untouched; a second take
    // finds nothing (the shell settings file owns the layout now).
    expect(loadDeckSettings('a')).toEqual({ loopSeconds: 8 })
    expect(takeLegacyDeckStyles()).toBeNull()
  })

  it('ignores malformed legacy style targets during migration', () => {
    localStorage.setItem(
      'lsdj:v1',
      JSON.stringify({
        decks: { a: { targets: [{ text: 42, x: 'left', y: 0 }] } },
      }),
    )
    // Nothing valid to migrate — and the garbage keys are still stripped.
    expect(takeLegacyDeckStyles()).toBeNull()
    expect(localStorage.getItem('lsdj:v1')).not.toContain('targets')
  })
})
