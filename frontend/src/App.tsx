import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'

import {
  INITIAL_CROSSFADE,
  INITIAL_CUE_MIX,
  type DeckId,
  type TrackSource,
} from './audio/types'
import { uploadStyleSample } from './audio/styleSample'
import {
  FX_ARG,
  invoke,
  getMcpInfo,
  rotateMcpToken,
  setMcpPort,
  setRecordingsFolder,
  styleApplyPreset,
  subscribeLoadTrack,
  subscribeLoadSample,
  subscribeDeckCommand,
  type McpInfo,
} from './audio/nativeEngine'
import { useAudioEngine } from './audio/engineContext'
import { useInterfaceStore, useProjected } from './audio/interfaceStore'
import { applyAppIntent } from './control/appIntents'
import { useControlBus } from './control/busContext'
import { MidiControls } from './control/MidiControls'
import { useMidi } from './control/useMidi'
import { MediaExplorer } from './media/MediaExplorer'
import {
  MEDIA_DEFAULT_HEIGHT,
  clampMediaHeight,
} from './media/mediaTray'
import { DeckColumn } from './deck/DeckColumn'
import { useDeck } from './deck/useDeck'
import { BeatView } from './mixer/BeatView'
import { MixerStrip, type ChannelControls } from './mixer/MixerStrip'
import { RecordControl } from './mixer/RecordControl'
import { AccentPicker } from './ui/AccentPicker'
import { OutputDevicePicker } from './ui/OutputDevicePicker'
import { BeatViewPicker } from './ui/BeatViewPicker'
import { Switch } from './ui/Switch'
import { Select } from './ui/Select'
import {
  deletePreset,
  loadAppSettings,
  loadPresets,
  takeLegacyDeckStyles,
  takeLegacyMixerSettings,
  takeLegacyShellSettings,
  updateAppSettings,
  upsertPresets,
  type AccentTheme,
  type BeatViewLayout,
} from './persistence'
import { Logo } from './ui/Logo'
import { Drawer } from './ui/Drawer'
import { Button } from './ui/Button'
import { LoraProvider } from './models/LoraProvider'
import { ModelManager } from './models/ModelManager'
import type { StylePreset } from './presets'
import { combinedRamWarning } from './ramWarning'
import { phaseOffsetBeats } from './audio/track'
import { handleShortcutKey } from './shortcuts'
import { PerformanceVisuals } from './visuals/PerformanceVisuals'

/** The agent harnesses we tailor a connection snippet for. A `command` harness gets
 * a one-line CLI; a `config` harness gets a JSON block for its settings file (the
 * per-tool file path lives in the step copy). */
const MCP_HARNESSES: { id: string; kind: 'command' | 'config' }[] = [
  { id: 'claudeCode', kind: 'command' },
  { id: 'claudeDesktop', kind: 'config' },
  { id: 'cursor', kind: 'config' },
  { id: 'vscode', kind: 'config' },
]

/** Build the copy-paste connection snippet for one harness with the live endpoint +
 * token baked in. VS Code keys servers under `servers`; the others use `mcpServers`
 * (and Claude Code adds it via its CLI). */
function mcpSnippet(harness: string, endpoint: string, token: string): string {
  const headers = { Authorization: `Bearer ${token}` }
  switch (harness) {
    case 'claudeCode':
      return `claude mcp add --transport http lsdj ${endpoint} --header "Authorization: Bearer ${token}"`
    case 'vscode':
      return JSON.stringify(
        { servers: { lsdj: { type: 'http', url: endpoint, headers } } },
        null,
        2,
      )
    case 'cursor':
      return JSON.stringify({ mcpServers: { lsdj: { url: endpoint, headers } } }, null, 2)
    default:
      return JSON.stringify(
        { mcpServers: { lsdj: { type: 'http', url: endpoint, headers } } },
        null,
        2,
      )
  }
}

/** The "AI co-DJ (MCP)" Settings body (ADR-0020 Phase 2): pick your AI tool, copy a
 * tailored connection snippet (the live endpoint + bearer token baked in), with the
 * raw endpoint/token and a rotate control below. The server is always on; the
 * fallback hint shows only if the loopback bind failed. */
function McpSettings({
  info,
  onRotate,
  onSetPort,
}: {
  info: McpInfo | null
  onRotate: () => void
  onSetPort: (port: number) => Promise<void>
}) {
  const { t } = useTranslation()
  const [harness, setHarness] = useState('claudeCode')
  const [copied, setCopied] = useState(false)
  const [portDraft, setPortDraft] = useState('')
  const [portError, setPortError] = useState<string | null>(null)
  const [seededPort, setSeededPort] = useState<number | null>(null)
  // Seed / re-seed the port field from the live port (after a successful change) — the
  // "adjust state during render" pattern, so the input shows the current port without
  // a sync effect.
  if (info?.port != null && info.port !== seededPort) {
    setSeededPort(info.port)
    setPortDraft(String(info.port))
  }
  // Stable references for the memoised Select (its memo is load-bearing). Reset the
  // Copied flash whenever the tool changes.
  const harnessOptions = useMemo(
    () => MCP_HARNESSES.map(({ id }) => ({ value: id, label: t(`settings.mcpHarnesses.${id}`) })),
    [t],
  )
  const pickHarness = useCallback((value: string) => {
    setHarness(value)
    setCopied(false)
  }, [])

  if (!info?.port || !info.token) {
    return <p className="settings-mcp__hint">{t('settings.mcpDisabled')}</p>
  }

  const endpoint = `http://127.0.0.1:${info.port}/mcp`
  const applyPort = () => {
    const port = Number(portDraft)
    if (!Number.isInteger(port) || port < 1024 || port > 65535) {
      setPortError(t('settings.mcpPortRange'))
      return
    }
    setPortError(null)
    void onSetPort(port).catch((error) =>
      setPortError(t('settings.mcpPortError', { message: String(error) })),
    )
  }
  const kind = MCP_HARNESSES.find((entry) => entry.id === harness)?.kind ?? 'command'
  const snippet = mcpSnippet(harness, endpoint, info.token)
  const copySnippet = () => {
    void navigator.clipboard
      ?.writeText(snippet)
      .then(() => {
        setCopied(true)
        window.setTimeout(() => setCopied(false), 1500)
      })
      .catch(() => {})
  }

  return (
    <>
      <p className="settings-mcp__hint">{t('settings.mcpHint')}</p>
      <Select
        label={t('settings.mcpHarness')}
        value={harness}
        options={harnessOptions}
        onChange={pickHarness}
      />
      <div className="settings-mcp__snippet">
        <div className="settings-mcp__snippet-head">
          <span className="settings-mcp__action">
            {kind === 'command' ? t('settings.mcpRunLabel') : t('settings.mcpConfigLabel')}
          </span>
          <Button variant="primary" onClick={copySnippet}>
            {copied ? t('settings.mcpCopied') : t('settings.mcpCopy')}
          </Button>
        </div>
        <pre className="settings-mcp__snippet-body">{snippet}</pre>
      </div>
      <p className="settings-mcp__hint">{t(`settings.mcpStep.${harness}`)}</p>

      <div className="settings-mcp__divider" />

      <div className="settings-mcp__field">
        <span className="ui-field__label">{t('settings.mcpEndpoint')}</span>
        <code className="settings-mcp__value">{endpoint}</code>
      </div>
      <div className="settings-mcp__field">
        <span className="ui-field__label">{t('settings.mcpPort')}</span>
        <div className="settings-mcp__port-row">
          <input
            className="ui-field__input settings-mcp__port-input"
            type="number"
            min={1024}
            max={65535}
            value={portDraft}
            onChange={(event) => {
              setPortDraft(event.target.value)
              setPortError(null)
            }}
          />
          <Button onClick={applyPort} disabled={portDraft === String(info.port)}>
            {t('settings.mcpApplyPort')}
          </Button>
        </div>
        <span className="settings-mcp__note">
          {portError ?? t('settings.mcpPortHint')}
        </span>
      </div>
      <div className="settings-mcp__field">
        <span className="ui-field__label">{t('settings.mcpToken')}</span>
        <code className="settings-mcp__value">{info.token}</code>
        <Button onClick={onRotate}>{t('settings.mcpRotate')}</Button>
        <span className="settings-mcp__note">{t('settings.mcpTokenHint')}</span>
      </div>
    </>
  )
}

function App() {
  const { t } = useTranslation()
  const engine = useAudioEngine()
  const appRef = useRef<HTMLElement>(null)
  // The authoritative interface-state store (ADR-0020): the webview projects it.
  const store = useInterfaceStore()
  const deckA = useDeck('a')
  const deckB = useDeck('b')
  // Crossfade / cue-mix are projections of the store, rendered optimistically
  // during a drag and reconciled to the store (a MIDI move arrives the same
  // way). The shell hydrates its persisted values into engine + store before
  // the webview exists (ADR-0020 phase C), so the initials only cover the
  // frames before the first snapshot.
  const [crossfade, setCrossfade] = useProjected(
    store?.crossfade,
    INITIAL_CROSSFADE,
    (position) => engine.setCrossfade(position),
  )
  const [cueMix, setCueMix] = useProjected(
    store?.cueMix,
    INITIAL_CUE_MIX,
    (position) => engine.setCueMix(position),
  )
  // Stable per-deck model-option arrays so the memoised Settings <Select> isn't
  // re-committed — and dismissed by WKWebView — on App's ~10 Hz re-render churn.
  // The fallback (a deck with no available list yet) must not be rebuilt each render.
  const deckAModelOptions = useMemo(
    () =>
      deckA.state.availableModels.length
        ? deckA.state.availableModels
        : [deckA.state.model ?? ''],
    [deckA.state.availableModels, deckA.state.model],
  )
  const deckBModelOptions = useMemo(
    () =>
      deckB.state.availableModels.length
        ? deckB.state.availableModels
        : [deckB.state.model ?? ''],
    [deckB.state.availableModels, deckB.state.model],
  )
  // The chosen native MAIN output device by name (empty = system default;
  // master → its ch 1/2) and the headphone CUE device (empty = "same as main",
  // the FLX4 phones on ch 3/4; a different name routes cue to a second device).
  // Shell-persisted store settings (ADR-0020 phase A): the pickers project the
  // snapshot; a successful switch records + persists Rust-side, and the shell
  // re-applies them at boot — no localStorage, no webview replay.
  const mainDevice = store?.mainDevice ?? ''
  const cueDevice = store?.cueDevice ?? ''
  // The beat view's home (M22): centre stacked, top bar, or off.
  const [beatView, setBeatView] = useState<BeatViewLayout>(
    () => loadAppSettings().beatView ?? 'center',
  )
  const handleBeatView = useCallback((layout: BeatViewLayout) => {
    setBeatView(layout)
    updateAppSettings({ beatView: layout })
  }, [])

  // The media tray's drawer state (open + height): App owns it so the in-panel
  // toggle and the Cmd/Ctrl+M shortcut share one source of truth, and both
  // persist across reloads.
  const [mediaOpen, setMediaOpen] = useState(
    () => loadAppSettings().mediaOpen ?? true,
  )
  const [mediaHeight, setMediaHeight] = useState(
    () => loadAppSettings().mediaHeight ?? MEDIA_DEFAULT_HEIGHT,
  )
  const handleMediaToggle = useCallback(() => {
    setMediaOpen((open) => {
      const next = !open
      updateAppSettings({ mediaOpen: next })
      return next
    })
  }, [])
  // Live during a resize drag (state only); `commit` persists once on release.
  const handleMediaResize = useCallback((height: number, commit: boolean) => {
    const clamped = clampMediaHeight(height)
    setMediaHeight(clamped)
    if (commit) updateAppSettings({ mediaHeight: clamped })
  }, [])

  // Master accent (LSDJai): the chosen hue rides on <html data-accent>,
  // where the theme blocks in tokens.css pick it up. Persisted like the
  // other app settings; default Acid Lime.
  const [accent, setAccent] = useState<AccentTheme>(
    () => loadAppSettings().accent ?? 'cyan',
  )
  useEffect(() => {
    document.documentElement.dataset.accent = accent
  }, [accent])
  const handleAccent = useCallback((value: AccentTheme) => {
    setAccent(value)
    updateAppSettings({ accent: value })
  }, [])
  const [performanceVisuals, setPerformanceVisuals] = useState(
    () => loadAppSettings().performanceVisuals ?? true,
  )
  const handlePerformanceVisuals = useCallback(() => {
    setPerformanceVisuals((enabled) => {
      const next = !enabled
      updateAppSettings({ performanceVisuals: next })
      return next
    })
  }, [])

  // Where master-bus recordings are saved (empty = the OS Downloads folder,
  // the default). A shell-persisted store setting (ADR-0020 phase A):
  // RecordControl reads the projection, the command persists Rust-side.
  const recordingsFolder = store?.recordingsFolder ?? ''
  const [recordingsFolderError, setRecordingsFolderError] = useState<string | null>(
    null,
  )
  const handleRecordingsFolder = useCallback((path: string) => {
    setRecordingsFolder(path)
  }, [])
  const chooseRecordingsFolder = useCallback(async () => {
    setRecordingsFolderError(null)
    // The native folder picker (dialog plugin); WKWebView has no File System
    // Access API, so the chosen path comes back from Rust.
    try {
      const dir = await invoke<string | null>('plugin:dialog|open', {
        options: { directory: true, multiple: false },
      })
      if (dir) handleRecordingsFolder(dir) // null = the user dismissed it
    } catch (error) {
      setRecordingsFolderError(error instanceof Error ? error.message : String(error))
    }
  }, [handleRecordingsFolder])
  const openRecordingsFolder = useCallback(async () => {
    setRecordingsFolderError(null)
    try {
      await invoke('open_recordings_folder', { folder: recordingsFolder })
    } catch (error) {
      setRecordingsFolderError(error instanceof Error ? error.message : String(error))
    }
  }, [recordingsFolder])

  // The settings drawer (issue #43): the appearance pickers + the model manager.
  const [settingsOpen, setSettingsOpen] = useState(false)
  // The native MCP server's endpoint + token (ADR-0020 Phase 2), shown in Settings
  // so a Claude Desktop / Code client can connect. Fetched once; null until app_info
  // resolves (and `port` stays null only if the loopback bind failed).
  const [mcpInfo, setMcpInfo] = useState<McpInfo | null>(null)
  useEffect(() => {
    void getMcpInfo().then(setMcpInfo)
  }, [])
  // Rotate the MCP bearer token: mint + persist a new one and show it (the old one
  // stops working at once). A no-op surfaced as nothing if the server is off.
  const handleRotateMcp = useCallback(async () => {
    try {
      const token = await rotateMcpToken()
      setMcpInfo((info) => (info ? { ...info, token } : info))
    } catch {
      // The server isn't running — leave the displayed token as-is.
    }
  }, [])
  // Set + persist the MCP port and restart the server on it; reflect the new port (so
  // the endpoint + snippets update). Rejects to the caller (McpSettings shows the
  // error) if the port can't be bound, leaving the running server untouched.
  const handleSetMcpPort = useCallback(async (port: number) => {
    const bound = await setMcpPort(port)
    setMcpInfo((info) => (info ? { ...info, port: bound } : info))
  }, [])

  // No mix-position boot replay: the SHELL hydrates its persisted crossfade,
  // cue mix, and per-deck mixer into the engine and the store before the
  // webview exists (ADR-0020 phase C), like the devices in phase A.

  // One-time migration: device/folder choices saved by pre-inversion builds
  // live in localStorage; once the store hydrates empty (a fresh shell
  // settings file), push the legacy values through the same commands a picker
  // uses — they persist shell-side — and drop the localStorage keys.
  const migratedShellSettingsRef = useRef(false)
  useEffect(() => {
    if (!store || migratedShellSettingsRef.current) return
    migratedShellSettingsRef.current = true
    const legacy = takeLegacyShellSettings()
    if (legacy) {
      if (legacy.outputDevice && !store.mainDevice) {
        void engine.setMainDevice(legacy.outputDevice).catch(() => {})
      }
      if (legacy.cueDevice && !store.cueDevice) {
        void engine.setCueDevice(legacy.cueDevice).catch(() => {})
      }
      if (legacy.recordingsFolder && !store.recordingsFolder) {
        setRecordingsFolder(legacy.recordingsFolder)
      }
    }
    // The style-pad arrangements moved shell-side in phase B: replay a
    // pre-inversion localStorage layout through the preset intent — only
    // onto a deck the shell hydrated empty, so the settings file (once
    // written) always wins.
    const legacyStyles = takeLegacyDeckStyles()
    if (legacyStyles) {
      for (const deckId of ['a', 'b'] as const) {
        const style = legacyStyles[deckId]
        const deckIndex = deckId === 'a' ? 0 : 1
        if (style && store.decks[deckIndex]?.styleTargets.length === 0) {
          styleApplyPreset(deckIndex, style.targets, style.cursor)
        }
      }
    }
    // The mixer moved shell-side in phase C: push a pre-inversion layout
    // through the same commands a fader uses (they write engine + store; the
    // settings watcher persists). Unconditional: the strip fires exactly
    // once per profile, on the first post-upgrade boot — a boot on which the
    // shell can only have hydrated defaults (the settings file gains mixer
    // values on this very run).
    const legacyMixer = takeLegacyMixerSettings()
    if (legacyMixer) {
      for (const deckId of ['a', 'b'] as const) {
        const mixer = legacyMixer.decks[deckId]
        if (!mixer) continue
        const deck = deckId === 'a' ? 0 : 1
        if (mixer.volume !== undefined) {
          void invoke('set_volume', { deck, gain: mixer.volume }).catch(() => {})
        }
        if (mixer.eq) {
          for (const band of ['low', 'mid', 'high'] as const) {
            void invoke('set_eq', { deck, band, value: mixer.eq[band] }).catch(() => {})
          }
        }
        if (mixer.fx?.kind) {
          void invoke('set_fx', { deck, kind: FX_ARG[mixer.fx.kind] }).catch(() => {})
          void invoke('set_fx_amount', { deck, amount: mixer.fx.amount }).catch(() => {})
        }
        if (mixer.trimDb !== undefined) {
          void invoke('set_trim', { deck, db: mixer.trimDb }).catch(() => {})
        }
      }
      if (legacyMixer.crossfade !== undefined) setCrossfade(legacyMixer.crossfade)
      if (legacyMixer.cueMix !== undefined) setCueMix(legacyMixer.cueMix)
    }
    // setCrossfade/setCueMix are stable projections; the effect is one-shot.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [store, engine])

  useEffect(() => {
    window.addEventListener('keydown', handleShortcutKey)
    return () => window.removeEventListener('keydown', handleShortcutKey)
  }, [])

  // Cmd/Ctrl+M toggles the media tray. Separate from handleShortcutKey, which
  // is a bare-letter focus router that bails on modifiers. preventDefault also
  // suppresses the macOS Cmd+M window-minimize default.
  useEffect(() => {
    function onKey(event: KeyboardEvent) {
      if (
        (event.metaKey || event.ctrlKey) &&
        !event.altKey &&
        !event.shiftKey &&
        event.key.toLowerCase() === 'm'
      ) {
        event.preventDefault()
        handleMediaToggle()
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [handleMediaToggle])

  // The one place a crossfade move is defined: audio bus + state. Every
  // source — slider, keyboard, hardware — lands here; the store change it
  // records is what the shell persists (ADR-0020 phase C).
  const handleCrossfade = useCallback(
    (position: number) => {
      // The projected setter renders optimistically and emits engine.setCrossfade,
      // which records the move into the store (the single source of truth).
      setCrossfade(position)
    },
    [setCrossfade],
  )

  // The one place a cue-mix move is defined, mirroring handleCrossfade.
  const handleCueMix = useCallback(
    (position: number) => {
      setCueMix(position)
    },
    [setCueMix],
  )

  // Deck-to-deck style sampling (M15): capture the OTHER deck's tail,
  // register the embedding on the target deck's worker, hand the new
  // pad target back to the column. Ids are session-unique; embeddings
  // are session-only (ADR-0011).
  const sampleCounter = useRef(0)
  const sampleFromOtherDeck = useCallback(
    async (target: DeckId) => {
      const sourceId: DeckId = target === 'a' ? 'b' : 'a'
      const source = sourceId === 'a' ? deckA : deckB
      const samples = await source.captureStyleSample()
      if (!samples) return null
      const count = ++sampleCounter.current
      const sample = `sample:${sourceId}:${count}`
      await uploadStyleSample(target, sample, samples)
      return {
        label: t('deck.style.sampleLabel', {
          deck: sourceId.toUpperCase(),
          n: count,
        }),
        sample,
      }
    },
    [deckA, deckB, t],
  )
  const handleSampleForA = useCallback(
    () => sampleFromOtherDeck('a'),
    [sampleFromOtherDeck],
  )
  const handleSampleForB = useCallback(
    () => sampleFromOtherDeck('b'),
    [sampleFromOtherDeck],
  )

  // Crates (M16): the preset list is App state so the browser, the
  // per-deck save buttons, and the hardware intents all see one truth.
  const [presets, setPresets] = useState<StylePreset[]>(loadPresets)
  const handleSavePreset = useCallback((preset: StylePreset) => {
    setPresets(upsertPresets([preset]))
  }, [])
  const handleImportPresets = useCallback((imported: StylePreset[]) => {
    setPresets(upsertPresets(imported))
  }, [])
  const handleDeletePreset = useCallback((name: string) => {
    setPresets(deletePreset(name))
  }, [])

  // Hardware intents (ADR-0005) for the state this component owns.
  // Resubscribes every render so the handler always reads current deck
  // state; the bus itself is a stable singleton.
  const bus = useControlBus()
  // Which deck's SHIFT is down, for the cross-deck SHIFT+jog cursor steering
  // (the steered deck takes one axis from each jog). Both down → deck A wins.
  const [shiftHeld, setShiftHeld] = useState<Record<DeckId, boolean>>({
    a: false,
    b: false,
  })
  const shiftedDeck: DeckId | null = shiftHeld.a ? 'a' : shiftHeld.b ? 'b' : null
  useEffect(() =>
    bus.subscribe((intent) => {
      if (intent.kind === 'shift') {
        setShiftHeld((previous) =>
          previous[intent.deck] === intent.held
            ? previous
            : { ...previous, [intent.deck]: intent.held },
        )
        return
      }
      applyAppIntent(
        intent,
        { a: deckA, b: deckB },
        { onCrossfade: handleCrossfade, onCueMix: handleCueMix },
        shiftedDeck,
      )
    }),
  )

  // Loading a preset: this component owns the FX half (via the deck
  // controls); the pad half rides the bus to the owning DeckColumn,
  // which applies targets + cursor and sends the style. A crate is a
  // realtime item, so loading one exits playback mode (ADR-0013).
  const handleLoadPreset = useCallback(
    (deck: DeckId, preset: StylePreset) => {
      const controls = deck === 'a' ? deckA : deckB
      controls.leavePlayback()
      controls.setFx(preset.fx.kind)
      controls.setFxAmount(preset.fx.amount)
      bus.publish({ kind: 'preset_load', deck, preset })
    },
    [deckA, deckB, bus],
  )

  // Track items flip the deck to playback; the way back lives on the deck
  // itself ("Back to live", ADR-0013: loading decides the mode).
  const handleLoadTrack = useCallback(
    (deck: DeckId, source: TrackSource, title: string) =>
      (deck === 'a' ? deckA : deckB).loadTrack(source, title),
    [deckA, deckB],
  )
  // Load a saved sample into a deck's loop-slot bank (ADR-0022) — the Samples-tab
  // counterpart of handleLoadTrack, routed to the deck's `loadSampleToSlot`.
  const handleLoadSample = useCallback(
    (deck: DeckId, wav: ArrayBuffer, oneShot: boolean, label: string) =>
      (deck === 'a' ? deckA : deckB).loadSampleToSlot(wav, oneShot, label),
    [deckA, deckB],
  )
  // Preview a library item in the phones before committing it to a deck
  // (ADR-0027): the engine routes it to the cue feed only, never the master.
  const handlePreview = useCallback(
    (wav: ArrayBuffer) => engine.auditionPlay(wav),
    [engine],
  )
  const handleStopPreview = useCallback(() => engine.auditionStop(), [engine])

  // An MCP agent's load_track / load_sample (Rust emits the event): run the deck's
  // load flow — the same path the Media Explorer takes, so the deck reflects the
  // load (playback mode + overview + cues, or the pad slot). A track loads by
  // library reference (the shell decodes, ADR-0030); a sample still reads bytes.
  // The MCP load subscriptions must register ONCE. The handlers churn (a fresh
  // useDeck object every render), and listenTo's async listen/unlisten would race
  // into duplicate live listeners on every re-subscribe — one load_sample then runs
  // the handler several times and fills every pad instead of one. Read the latest
  // handlers from a ref so the listeners stay stable for the app's lifetime.
  const loadLatestRef = useRef({ handleLoadTrack, handleLoadSample })
  useEffect(() => {
    loadLatestRef.current = { handleLoadTrack, handleLoadSample }
  })
  useEffect(() => {
    const toDeck = (n: number): DeckId => (n === 0 ? 'a' : 'b')
    const unTrack = subscribeLoadTrack(({ deck, file, title }) => {
      void loadLatestRef.current
        .handleLoadTrack(toDeck(deck), { kind: 'song', name: file }, title)
        .catch(() => {})
    })
    const unSample = subscribeLoadSample(({ deck, file, oneShot, label }) => {
      void invoke<ArrayBuffer>('read_generated_sample', { name: file })
        .then((wav) =>
          loadLatestRef.current.handleLoadSample(toDeck(deck), wav, oneShot, label),
        )
        .catch(() => {})
    })
    return () => {
      unTrack()
      unSample()
    }
  }, [])

  // Beat-matching (M20, ADR-0014): SYNC matches a track deck to the
  // other deck's effective tempo — gated stream BPM, or grid BPM ×
  // rate when the other side is a track too. Phase is read for the
  // meter from whichever clock each deck honestly has.
  const effectiveBpm = useCallback(
    (deck: typeof deckA) =>
      deck.mode === 'playback'
        ? deck.track?.bpm != null
          ? deck.track.bpm * deck.track.rate
          : null
        : deck.bpm,
    [],
  )
  const handleSyncA = useCallback(
    () => deckA.syncTrack(effectiveBpm(deckB)),
    [deckA, deckB, effectiveBpm],
  )
  const handleSyncB = useCallback(
    () => deckB.syncTrack(effectiveBpm(deckA)),
    [deckA, deckB, effectiveBpm],
  )

  // An MCP agent's transport / on-air gesture (Rust emits mcp://deck-command): run the
  // deck's own method so its reducer state and the UI follow (seek reflects via the
  // position poll; rate/loop/sync and on-air/prime are webview-owned). The load-flow
  // pattern — on-air routes to play()/prime() so the primed status + cue LED follow.
  // Register the MCP transport subscription ONCE too — same churn / duplicate-listener
  // hazard as the load subscriptions above; read the latest decks + sync handlers from
  // a ref so a single mcp://deck-command runs the gesture exactly once.
  const commandLatestRef = useRef({ deckA, deckB, handleSyncA, handleSyncB })
  useEffect(() => {
    commandLatestRef.current = { deckA, deckB, handleSyncA, handleSyncB }
  })
  useEffect(() => {
    return subscribeDeckCommand(({ deck, command, value }) => {
      const { deckA, deckB, handleSyncA, handleSyncB } = commandLatestRef.current
      const controls = deck === 0 ? deckA : deckB
      switch (command) {
        case 'seek':
          if (value != null) controls.seekTrack(value)
          break
        case 'rate':
          if (value != null) controls.setTrackRate(value)
          break
        case 'beatloop':
          if (value != null) controls.beatLoop(value)
          break
        case 'sync':
          ;(deck === 0 ? handleSyncA : handleSyncB)()
          break
        case 'onair':
          void controls.play()
          break
        case 'offair':
          void controls.prime()
          break
      }
    })
  }, [])
  const getPhaseOffset = useCallback(() => {
    const aPlayback = deckA.mode === 'playback'
    const bPlayback = deckB.mode === 'playback'
    if (!aPlayback && !bPlayback) return null
    const clockOf = (deck: typeof deckA) =>
      deck.mode === 'playback' ? deck.getTrackBeat() : deck.getLiveBeat()
    const a = clockOf(deckA)
    const b = clockOf(deckB)
    if (!a || !b) return null
    // The track side reads against the other deck; A wins ties.
    return aPlayback ? phaseOffsetBeats(a, b) : phaseOffsetBeats(b, a)
  }, [deckA, deckB])

  // Performance visuals use the same speaker-clock selection rule as the phase
  // meter. Audibility is separate and primitive so primed/paused sources are
  // hard-gated even if their cached pre-crossfader channel level remains hot.
  const deckAMode = deckA.mode
  const deckBMode = deckB.mode
  const getTrackBeatA = deckA.getTrackBeat
  const getTrackBeatB = deckB.getTrackBeat
  const getLiveBeatA = deckA.getLiveBeat
  const getLiveBeatB = deckB.getLiveBeat
  const getPerformanceBeatA = useCallback(
    () => (deckAMode === 'playback' ? getTrackBeatA() : getLiveBeatA()),
    [deckAMode, getLiveBeatA, getTrackBeatA],
  )
  const getPerformanceBeatB = useCallback(
    () => (deckBMode === 'playback' ? getTrackBeatB() : getLiveBeatB()),
    [deckBMode, getLiveBeatB, getTrackBeatB],
  )
  const performanceAudibleA =
    deckAMode === 'playback'
      ? deckA.track?.playing === true
      : deckA.state.playing && !deckA.primed
  const performanceAudibleB =
    deckBMode === 'playback'
      ? deckB.track?.playing === true
      : deckB.state.playing && !deckB.primed

  // The MIDI hook is now a projection + intent bridge (ADR-0031): the shell
  // owns the transport and paints the LEDs from the store, so App's old LED
  // effects are gone. The LED inputs React still owns mirror into the store
  // where that state lives: the net selection rides DeckColumn's atomic
  // style mirror, the primed flag rides useDeck's.
  const midi = useMidi()

  const ramWarning = combinedRamWarning(
    { a: deckA.state.model, b: deckB.state.model },
    deckA.state.ramInfo ?? deckB.state.ramInfo,
  )

  const channels: Record<'a' | 'b', ChannelControls> = {
    a: {
      volume: deckA.volume,
      eq: deckA.eq,
      cue: deckA.cue,
      trim: deckA.trim,
      onSetVolume: deckA.setVolume,
      onSetEqBand: deckA.setEqBand,
      onSetCue: deckA.setCue,
      onSetTrimDb: deckA.setTrimDb,
      onEnableAutoTrim: deckA.enableAutoTrim,
      getLevel: deckA.getChannelLevel,
    },
    b: {
      volume: deckB.volume,
      eq: deckB.eq,
      cue: deckB.cue,
      trim: deckB.trim,
      onSetVolume: deckB.setVolume,
      onSetEqBand: deckB.setEqBand,
      onSetCue: deckB.setCue,
      onSetTrimDb: deckB.setTrimDb,
      onEnableAutoTrim: deckB.enableAutoTrim,
      getLevel: deckB.getChannelLevel,
    },
  }

  return (
    <LoraProvider>
    <main
      ref={appRef}
      className="app"
      data-performance-visuals={performanceVisuals ? 'on' : 'off'}
    >
      {/* The frameless title-bar strip behind the macOS traffic lights. With
          titleBarStyle Overlay the webview covers the native title bar, so that
          top strip is webview content and needs its OWN drag region — an empty,
          transparent surface over the top inset. */}
      <div className="app__titlebar" data-tauri-drag-region aria-hidden="true" />
      <PerformanceVisuals
        enabled={performanceVisuals}
        rootRef={appRef}
        crossfade={crossfade}
        getContextTime={engine.getContextTime}
        getMasterLevel={engine.getMasterLevel}
        decks={{
          a: {
            audible: performanceAudibleA,
            getLevel: deckA.getChannelLevel,
            getBeat: getPerformanceBeatA,
          },
          b: {
            audible: performanceAudibleB,
            getLevel: deckB.getChannelLevel,
            getBeat: getPerformanceBeatB,
          },
        }}
      />
      {/* Drag the window by the header too. `deep` makes the whole subtree a drag
          surface (logo, gaps, status text); Tauri auto-excludes clickable
          elements (the native selects, the MIDI button) so they stay clickable. */}
      <header className="app__statusbar" data-tauri-drag-region="deep">
        <Logo />
        <div className="app__statusbar-right">
          {ramWarning && (
            <p className="app__warning" role="status">
              {t('app.ramWarning', ramWarning)}
            </p>
          )}
          <MidiControls
            connected={midi.connected}
            deviceName={midi.deviceName}
            devices={midi.devices}
            onSelectDevice={midi.selectDevice}
          />
          <RecordControl recordingsFolder={recordingsFolder} />
          <Button onClick={() => setSettingsOpen(true)}>{t('settings.open')}</Button>
        </div>
      </header>
      <Drawer
        open={settingsOpen}
        onClose={() => setSettingsOpen(false)}
        title={t('settings.title')}
        closeLabel={t('settings.close')}
      >
        <section className="modelmgr__section">
          <h3 className="modelmgr__heading">{t('settings.appearance')}</h3>
          <div className="settings-appearance">
            <BeatViewPicker
              label={t('beatview.layout')}
              value={beatView}
              options={(['center', 'vertical', 'top', 'off'] as const).map((layout) => ({
                value: layout,
                label: t(`beatview.layouts.${layout}`),
              }))}
              onChange={handleBeatView}
            />
            <AccentPicker
              label={t('accent.label')}
              value={accent}
              options={(['lime', 'violet', 'cyan'] as const).map((option) => ({
                value: option,
                label: t(`accent.options.${option}`),
              }))}
              onChange={handleAccent}
            />
            <Switch
              label={t('settings.performanceVisuals')}
              on={performanceVisuals}
              onClick={handlePerformanceVisuals}
            />
          </div>
        </section>
        <section className="modelmgr__section">
          <h3 className="modelmgr__heading">{t('settings.audio')}</h3>
          <div className="settings-audio">
            <OutputDevicePicker mode="main" value={mainDevice} />
            <OutputDevicePicker
              mode="cue"
              value={cueDevice}
              mainDeviceName={mainDevice}
            />
          </div>
        </section>
        {/* Where master-bus recordings are saved. Empty = the OS Downloads
            folder (the default); choosing a folder routes takes there. */}
        <section className="modelmgr__section">
          <h3 className="modelmgr__heading">{t('settings.recording')}</h3>
          <div className="settings-recording">
            <div className="settings-recording__folder">
              <span className="settings-recording__label">
                {t('settings.recordingFolder')}
              </span>
              <span
                className="settings-recording__path"
                title={recordingsFolder || undefined}
              >
                {recordingsFolder || t('settings.recordingFolderDefault')}
              </span>
            </div>
            <div className="settings-recording__actions">
              <Button onClick={() => void chooseRecordingsFolder()}>
                {t('media.folder.choose')}
              </Button>
              {recordingsFolder && (
                <Button onClick={() => handleRecordingsFolder('')}>
                  {t('settings.useDownloads')}
                </Button>
              )}
              <Button onClick={() => void openRecordingsFolder()}>
                {t('modelManager.openFolder')}
              </Button>
            </div>
            {recordingsFolderError && (
              <p className="settings-recording__error" role="alert">
                {t('settings.recordingFolderError', { message: recordingsFolderError })}
              </p>
            )}
          </div>
        </section>
        {/* Which model each deck runs live — a once-per-session setup choice,
            moved out of the deck column so it stops competing with the style pad
            for height. A crashed worker still offers its own picker in the
            recovery block (the "switch to a model that fits" path). */}
        <section className="modelmgr__section">
          <h3 className="modelmgr__heading">{t('settings.models')}</h3>
          <div className="settings-models">
            {([
              { id: 'a' as const, deck: deckA, modelOptions: deckAModelOptions },
              { id: 'b' as const, deck: deckB, modelOptions: deckBModelOptions },
            ]).map(({ id, deck, modelOptions }) => (
              <Select
                key={id}
                label={t('settings.modelDeck', { id: id.toUpperCase() })}
                value={deck.state.model ?? ''}
                options={modelOptions}
                disabled={deck.state.connection !== 'open' || deck.state.switchingModel}
                onChange={deck.setModel}
              />
            ))}
          </div>
        </section>
        {/* The model library: install / manage the realtime (Magenta) and
            generation (Stable Audio 3) weights on disk. The umbrella section
            keeps the install families grouped under one heading and restores the
            inter-section rhythm across the ModelManager boundary. */}
        <section className="modelmgr__section settings-model-library">
          <h3 className="modelmgr__heading">{t('settings.modelLibrary')}</h3>
          <ModelManager />
        </section>
        {/* The native MCP server (ADR-0020 Phase 2): last in the list so the
            copy-paste connection snippets don't push the everyday controls down. */}
        <section className="modelmgr__section">
          <h3 className="modelmgr__heading">{t('settings.mcp')}</h3>
          <div className="settings-mcp">
            <McpSettings
              info={mcpInfo}
              onRotate={handleRotateMcp}
              onSetPort={handleSetMcpPort}
            />
          </div>
        </section>
      </Drawer>
      {beatView === 'top' && (
        <BeatView
          getSourceA={deckA.getZoomSource}
          getSourceB={deckB.getZoomSource}
        />
      )}
      <div className="app__booth">
        <DeckColumn
          deckId="a"
          state={deckA.state}
          onPlay={() => void deckA.play()}
          onStop={deckA.stop}
          onSetModel={deckA.setModel}
          onRestart={deckA.restartWorker}
          shiftedDeck={shiftedDeck}
          primed={deckA.primed}
          fx={deckA.fx}
          onSetFx={deckA.setFx}
          onSetFxAmount={deckA.setFxAmount}
          loop={deckA.loop}
          onLoopPad={deckA.toggleLoopPad}
          onClearLoopPad={deckA.clearLoopPad}
          onSetLoopSeconds={deckA.setLoopSeconds}
          onGenerateToPad={deckA.generateToPad}
          generateError={deckA.generateError}
          bpm={deckA.bpm}
          onSampleOtherDeck={handleSampleForA}
          canSample={deckB.state.playing}
          onSavePreset={handleSavePreset}
          mode={deckA.mode}
          track={deckA.track}
          onLeavePlayback={deckA.leavePlayback}
          onSeekTrack={deckA.seekTrack}
          onSetTrackRate={deckA.setTrackRate}
          onSyncTrack={handleSyncA}
          onHotCuePad={deckA.hotCuePad}
          onClearHotCue={deckA.clearHotCue}
          onLoopIn={deckA.loopIn}
          onLoopOut={deckA.loopOut}
          onLoopExit={deckA.loopExit}
          onBeatLoop={deckA.beatLoop}
          onHalveLoop={deckA.halveLoop}
          onDoubleLoop={deckA.doubleLoop}
          getTrackPeaks={deckA.getTrackPeaks}
        />
        <div className="app__center">
          {(beatView === 'center' || beatView === 'vertical') && (
            <BeatView
              vertical={beatView === 'vertical'}
              getSourceA={deckA.getZoomSource}
              getSourceB={deckB.getZoomSource}
            />
          )}
          <MixerStrip
            channels={channels}
            crossfade={crossfade}
            onCrossfadeChange={handleCrossfade}
            cueMix={cueMix}
            onCueMixChange={handleCueMix}
            getPhaseOffset={getPhaseOffset}
          />
        </div>
        <DeckColumn
          deckId="b"
          state={deckB.state}
          onPlay={() => void deckB.play()}
          onStop={deckB.stop}
          onSetModel={deckB.setModel}
          onRestart={deckB.restartWorker}
          shiftedDeck={shiftedDeck}
          primed={deckB.primed}
          fx={deckB.fx}
          onSetFx={deckB.setFx}
          onSetFxAmount={deckB.setFxAmount}
          loop={deckB.loop}
          onLoopPad={deckB.toggleLoopPad}
          onClearLoopPad={deckB.clearLoopPad}
          onSetLoopSeconds={deckB.setLoopSeconds}
          onGenerateToPad={deckB.generateToPad}
          generateError={deckB.generateError}
          bpm={deckB.bpm}
          onSampleOtherDeck={handleSampleForB}
          canSample={deckA.state.playing}
          onSavePreset={handleSavePreset}
          mode={deckB.mode}
          track={deckB.track}
          onLeavePlayback={deckB.leavePlayback}
          onSeekTrack={deckB.seekTrack}
          onSetTrackRate={deckB.setTrackRate}
          onSyncTrack={handleSyncB}
          onHotCuePad={deckB.hotCuePad}
          onClearHotCue={deckB.clearHotCue}
          onLoopIn={deckB.loopIn}
          onLoopOut={deckB.loopOut}
          onLoopExit={deckB.loopExit}
          onBeatLoop={deckB.beatLoop}
          onHalveLoop={deckB.halveLoop}
          onDoubleLoop={deckB.doubleLoop}
          getTrackPeaks={deckB.getTrackPeaks}
        />
      </div>
      <MediaExplorer
        presets={presets}
        onLoadPreset={handleLoadPreset}
        onDeletePreset={handleDeletePreset}
        onImportPresets={handleImportPresets}
        onLoadTrack={handleLoadTrack}
        onLoadSample={handleLoadSample}
        onPreview={handlePreview}
        onStopPreview={handleStopPreview}
        open={mediaOpen}
        onToggle={handleMediaToggle}
        height={mediaHeight}
        onResize={handleMediaResize}
      />
    </main>
    </LoraProvider>
  )
}

export default App
