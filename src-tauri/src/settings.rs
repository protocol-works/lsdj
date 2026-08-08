//! Shell-persisted app settings (ADR-0020 phase A).
//!
//! Settings the SHELL consumes — the output devices and the recordings
//! folder — used to live in webview localStorage, replayed into the engine
//! at boot by App.tsx. Persistence follows ownership: they now persist here
//! (a JSON file under the host-resolved config dir, beside the MCP token), hydrate into
//! the engine and the interface store during `setup` — before the webview
//! exists — and mutate through the same commands every controller uses. The
//! webview's pickers become projections. Presentation-only preferences
//! (accent, beat view) deliberately stay webview-side: the shell never
//! consumes them.

use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::store::{GenerationSnap, InterfaceState, InterfaceStore};

/// The persisted shape. `#[serde(default)]` keeps old files readable as
/// fields are added; empty strings are the defaults ("system default"
/// device, "same as main" cue, "Downloads" folder).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ShellSettings {
    pub main_device: String,
    pub cue_device: String,
    pub recordings_folder: String,
    /// Per-deck style-pad arrangements (ADR-0020 phase B), indexed by deck.
    /// Text targets + cursor only — sampled chips are session-only (ADR-0011).
    pub deck_styles: Vec<DeckStyleSetting>,
    /// Per-deck mixer state (ADR-0020 phase C), indexed by deck. Cue (PFL)
    /// deliberately never persists.
    pub deck_mixers: Vec<DeckMixerSetting>,
    /// Per-deck live generation params (issue #84), indexed by deck — the
    /// tunable sampling/guidance operating point, so a performer's tuning
    /// survives a restart. Reuses the store snapshot shape (like `deck_mixers`
    /// reuses `EqSnap`).
    pub deck_generations: Vec<GenerationSnap>,
    pub crossfade: f32,
    pub cue_mix: f32,
}

impl Default for ShellSettings {
    fn default() -> Self {
        ShellSettings {
            main_device: String::new(),
            cue_device: String::new(),
            recordings_folder: String::new(),
            deck_styles: Vec::new(),
            deck_mixers: Vec::new(),
            deck_generations: Vec::new(),
            crossfade: 0.5,
            cue_mix: 0.5,
        }
    }
}

/// One deck's persisted style-pad arrangement. The file is user-editable, so
/// hydration sanitises through [`crate::style::sanitize_preset_targets`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DeckStyleSetting {
    pub targets: Vec<crate::store::StyleTargetSnap>,
    pub cursor: crate::store::PadPointSnap,
}

impl Default for DeckStyleSetting {
    fn default() -> Self {
        DeckStyleSetting {
            targets: Vec::new(),
            cursor: crate::store::PadPointSnap { x: 0.5, y: 0.5 },
        }
    }
}

/// One deck's persisted mixer state (ADR-0020 phase C). The defaults are the
/// shipped boot values — Rust owns them now, so a fresh install hydrates the
/// engine and the store to exactly this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DeckMixerSetting {
    pub volume: f32,
    pub eq: crate::store::EqSnap,
    pub fx_kind: Option<crate::store::FxKindSnap>,
    pub fx_amount: f32,
    pub trim_db: f32,
}

impl Default for DeckMixerSetting {
    fn default() -> Self {
        DeckMixerSetting {
            volume: 0.8,
            eq: crate::store::EqSnap { low: 0.5, mid: 0.5, high: 0.5 },
            fx_kind: None,
            fx_amount: 0.0,
            trim_db: 0.0,
        }
    }
}

fn settings_file() -> PathBuf {
    crate::platform_paths::get().config().join("settings.json")
}

/// Load from a concrete path (the testable core): a missing or unreadable
/// file is the defaults — settings must never block boot.
pub fn load_from(path: &Path) -> ShellSettings {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

/// Save to a concrete path, creating the parent dir; best-effort by design
/// (a read-only disk must not break a device switch). Written to a sibling
/// temp file, fsynced, and renamed into place: a process crash OR a power
/// cut mid-write leaves the old settings.json intact instead of a truncated
/// one (which `load_from` would silently read as "reset everything to
/// defaults"). A failed attempt removes its temp file rather than stranding
/// it.
pub fn save_to(path: &Path, settings: &ShellSettings) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(json) = serde_json::to_string_pretty(settings) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    let written = std::fs::File::create(&tmp)
        .and_then(|mut file| {
            use std::io::Write;
            file.write_all(json.as_bytes())?;
            file.sync_all()
        })
        .is_ok();
    if !written || std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

pub fn load(_app: &AppHandle) -> ShellSettings {
    load_from(&settings_file())
}

/// One writer at a time: `update` runs from the main thread (the device /
/// folder commands) AND the persistence debounce thread, and an unguarded
/// load→mutate→save pair interleaved across threads saves its own stale
/// read — a device pick landing under a mixer flush would vanish from the
/// file until the next pick. The lock covers the whole read-modify-write.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Read-modify-write at a concrete path (the testable core), serialised
/// through [`WRITE_LOCK`].
pub fn update_at(path: &Path, mutate: impl FnOnce(&mut ShellSettings)) -> ShellSettings {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut settings = load_from(path);
    mutate(&mut settings);
    save_to(path, &settings);
    settings
}

/// Read-modify-write one field; the single mutation path the commands use.
pub fn update(_app: &AppHandle, mutate: impl FnOnce(&mut ShellSettings)) -> ShellSettings {
    update_at(&settings_file(), mutate)
}

/// The settings-write debounce: a fader ride or a pad drag settles before it
/// hits the disk.
const PERSIST_DEBOUNCE: Duration = Duration::from_millis(1000);

/// What the store persists (ADR-0020 phases B+C): the pad arrangements, the
/// per-deck mixer, and the master blends — one comparable value so unrelated
/// store churn (analysis ticks) never holds the debounce open.
#[derive(Debug, Clone, PartialEq)]
struct PersistedInterface {
    styles: Vec<DeckStyleSetting>,
    mixers: Vec<DeckMixerSetting>,
    generations: Vec<GenerationSnap>,
    crossfade: f32,
    cue_mix: f32,
}

/// Project a state's persistable slice. Sampled style chips stay out — their
/// embeddings are session-only (ADR-0011), a persisted chip would be a dead
/// reference on next boot. Cue (PFL) deliberately never persists.
fn persistable(state: &InterfaceState) -> PersistedInterface {
    PersistedInterface {
        styles: state
            .decks
            .iter()
            .map(|deck| DeckStyleSetting {
                targets: deck
                    .style_targets
                    .iter()
                    .filter(|t| t.sample.is_none())
                    .cloned()
                    .collect(),
                cursor: deck.cursor,
            })
            .collect(),
        mixers: state
            .decks
            .iter()
            .map(|deck| DeckMixerSetting {
                volume: deck.volume,
                eq: deck.eq,
                fx_kind: deck.fx.kind,
                fx_amount: deck.fx.amount,
                trim_db: deck.trim_db,
            })
            .collect(),
        generations: state.decks.iter().map(|deck| deck.generation).collect(),
        crossfade: state.crossfade,
        cue_mix: state.cue_mix,
    }
}

/// Persist the store's settings slice into the shell settings file, debounced.
pub fn watch_persistence(app: AppHandle, store: &InterfaceStore) {
    let (tx, rx) = mpsc::channel::<PersistedInterface>();
    let last = Mutex::new(None::<PersistedInterface>);
    store.watch(move |state| {
        let slice = persistable(state);
        // Dedup before the channel: unrelated store churn (analysis ticks)
        // must not hold the debounce open forever.
        let mut last = last.lock().unwrap_or_else(|p| p.into_inner());
        if last.as_ref() != Some(&slice) {
            *last = Some(slice.clone());
            let _ = tx.send(slice);
        }
    });
    std::thread::spawn(move || {
        let write = |app: &AppHandle, slice: PersistedInterface| {
            update(app, |s| {
                s.deck_styles = slice.styles;
                s.deck_mixers = slice.mixers;
                s.deck_generations = slice.generations;
                s.crossfade = slice.crossfade;
                s.cue_mix = slice.cue_mix;
            });
        };
        let mut pending: Option<PersistedInterface> = None;
        loop {
            let next = if pending.is_some() {
                match rx.recv_timeout(PERSIST_DEBOUNCE) {
                    Ok(slice) => Some(slice),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if let Some(slice) = pending.take() {
                            write(&app, slice);
                        }
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => None,
                }
            } else {
                rx.recv().ok()
            };
            match next {
                Some(slice) => pending = Some(slice),
                None => {
                    // Store dropped (shutdown): flush what's pending and stop.
                    if let Some(slice) = pending.take() {
                        write(&app, slice);
                    }
                    return;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_defaults_when_missing_or_corrupt() {
        let dir = std::env::temp_dir().join(format!("lsdj-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        // Missing file → defaults.
        assert_eq!(load_from(&path), ShellSettings::default());
        // Round trip.
        let settings = ShellSettings {
            main_device: "DDJ-FLX4".into(),
            cue_device: "".into(),
            recordings_folder: "/tmp/takes".into(),
            deck_styles: vec![DeckStyleSetting {
                targets: vec![crate::store::StyleTargetSnap {
                    x: 0.2,
                    y: 0.8,
                    text: "dub".into(),
                    sample: None,
                }],
                cursor: crate::store::PadPointSnap { x: 0.3, y: 0.4 },
            }],
            deck_mixers: vec![DeckMixerSetting {
                volume: 0.6,
                eq: crate::store::EqSnap { low: 0.2, mid: 0.5, high: 0.9 },
                fx_kind: Some(crate::store::FxKindSnap::DubEcho),
                fx_amount: 0.4,
                trim_db: -3.0,
            }],
            deck_generations: vec![GenerationSnap {
                temperature: 0.7,
                top_k: 20,
                cfg_musiccoca: 3.0,
                cfg_notes: 1.0,
            }],
            crossfade: 0.25,
            cue_mix: 0.75,
        };
        save_to(&path, &settings);
        assert_eq!(load_from(&path), settings);
        // Corrupt file → defaults, not a crash.
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(load_from(&path), ShellSettings::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two writers hammering different fields through `update_at` must both
    /// land: the WRITE_LOCK serialises the read-modify-write, so a device
    /// pick can no longer vanish under a persistence flush. (Without the
    /// lock this interleaves and flakes — the regression it pins.)
    #[test]
    fn concurrent_updates_lose_neither_writers_field() {
        let dir = std::env::temp_dir().join(format!("lsdj-settings-race-{}", std::process::id()));
        let path = dir.join("settings.json");
        const ROUNDS: usize = 50;
        std::thread::scope(|s| {
            let device = &path;
            let folder = &path;
            s.spawn(move || {
                for i in 0..ROUNDS {
                    update_at(device, |settings| settings.main_device = format!("device-{i}"));
                }
            });
            s.spawn(move || {
                for i in 0..ROUNDS {
                    update_at(folder, |settings| {
                        settings.recordings_folder = format!("folder-{i}")
                    });
                }
            });
        });
        let settled = load_from(&path);
        assert_eq!(settled.main_device, format!("device-{}", ROUNDS - 1));
        assert_eq!(settled.recordings_folder, format!("folder-{}", ROUNDS - 1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_files_stay_readable_as_fields_grow() {
        let dir = std::env::temp_dir().join(format!("lsdj-settings-old-{}", std::process::id()));
        let path = dir.join("settings.json");
        std::fs::create_dir_all(&dir).unwrap();
        // A file from a build that only knew mainDevice.
        std::fs::write(&path, r#"{ "mainDevice": "Speakers" }"#).unwrap();
        let settings = load_from(&path);
        assert_eq!(settings.main_device, "Speakers");
        assert_eq!(settings.cue_device, "");
        assert_eq!(settings.recordings_folder, "");
        assert!(settings.deck_styles.is_empty());
        assert!(settings.deck_mixers.is_empty());
        assert!(settings.deck_generations.is_empty());
        // The blend defaults are centred, not zeroed.
        assert_eq!(settings.crossfade, 0.5);
        assert_eq!(settings.cue_mix, 0.5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_persistable_slice_carries_mixer_and_styles_but_no_sampled_chips() {
        let mut state = InterfaceState::default();
        state.style_add_target(0, "dub");
        state.style_add_sample_target(0, "Deck B sample 1", "sample:b:1");
        state.set_cursor(0, crate::store::PadPointSnap { x: 0.2, y: 0.8 });
        state.set_volume(0, 0.6);
        state.set_eq(0, lsdj_engine::EqBand::High, 0.9);
        state.set_trim(1, -3.0);
        state.set_fx(0, lsdj_engine::FxKind::DubEcho);
        state.set_fx_amount(0, 0.4);
        state.set_generation(
            0,
            GenerationSnap {
                temperature: 0.7,
                top_k: 20,
                cfg_musiccoca: 3.0,
                cfg_notes: 1.0,
            },
        );
        state.set_crossfade(0.25);
        state.set_cue_mix(0.75);
        // Cue never persists — flipping it must not change the slice.
        let before_cue = persistable(&state);
        state.set_cue(0, true);
        assert_eq!(persistable(&state), before_cue);

        let slice = persistable(&state);
        assert_eq!(slice.styles[0].targets.len(), 1);
        assert_eq!(slice.styles[0].targets[0].text, "dub");
        assert_eq!(slice.styles[0].cursor, crate::store::PadPointSnap { x: 0.2, y: 0.8 });
        assert_eq!(slice.mixers[0].volume, 0.6);
        assert_eq!(slice.mixers[0].eq.high, 0.9);
        assert_eq!(slice.mixers[0].fx_kind, Some(crate::store::FxKindSnap::DubEcho));
        assert_eq!(slice.mixers[0].fx_amount, 0.4);
        assert_eq!(slice.mixers[1].trim_db, -3.0);
        // The generation slice carries the deck's tuning; deck 1 keeps baseline.
        assert_eq!(slice.generations[0].temperature, 0.7);
        assert_eq!(slice.generations[0].top_k, 20);
        assert_eq!(slice.generations[1], GenerationSnap::default());
        assert_eq!(slice.crossfade, 0.25);
        assert_eq!(slice.cue_mix, 0.75);
    }
}
