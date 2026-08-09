//! Native MIDI I/O in the Rust shell (ADR-0031, superseding ADR-0005's
//! webview transport).
//!
//! This module owns everything `tauri-plugin-midi` + the webview `control/`
//! byte path used to: device enumeration and hot-plug (a 1 Hz rescan — the
//! same cadence the retired shim polled at), driver binding by port-name
//! fragment, the position-sync SysEx on a fresh bind, byte→intent
//! translation ([`translate`]), LED output ([`leds`]), and the raw-byte
//! monitor the byte-map doc calls the arbiter.
//!
//! Routing (the ADR-0031 split, at the current state of ADR-0020's
//! inversion): control-surface intents are forwarded to the webview over the
//! `midi://intent` event and dispatched onto the existing ControlBus — the
//! deck-control semantics they trigger (mode gating, persistence, auto-trim)
//! still live in React, so applying them natively would fork those rules.
//! The performance-note path ([`notes`], issue #48) never touches the
//! webview: KEYBOARD-bank pads and external keyboards steer generation
//! entirely in-process, beside the beat clock. As the store inversion
//! proceeds, intent kinds migrate from the forward list to native
//! application without touching the transport or the translator.
//!
//! Input callbacks run on the platform MIDI backend's threads: they translate, route, and
//! return — the heavy lifting (LED frames, beat math) lives on the painter
//! and scheduler threads. Nothing here goes near the cpal callback.

pub mod drivers;
pub mod leds;
pub mod notes;
pub mod translate;

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use midir::{Ignore, MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use lsdj_engine::host::Host;
use lsdj_engine::{DECK_COUNT, LOOP_SLOT_COUNT};

use crate::store::{InterfaceState, InterfaceStore};

use drivers::{driver_for_name, Driver};
use notes::NoteSteering;
use translate::{Flx4Translator, Translated};

/// The Tauri event carrying a translated control-surface intent to the
/// webview ControlBus (the payload IS a `ControlIntent`).
pub const INTENT_EVENT: &str = "midi://intent";
/// The Tauri event announcing a connection-status change (same payload as
/// the `midi_status` command).
pub const STATUS_EVENT: &str = "midi://status";

/// How often the scanner re-enumerates ports (the shim's hot-plug cadence).
const SCAN_INTERVAL: Duration = Duration::from_secs(1);
/// How long a full repaint waits before sending. The FLX4 clears its own pad
/// LEDs as part of a pad-mode switch; the native path is fast enough to land
/// the repaint BEFORE that clear, which then wipes it — pads stayed dark
/// after every mode switch (found on the device). The retired webview path
/// never raced this only because React's render cycle delayed its repaints.
const REPAINT_SETTLE: Duration = Duration::from_millis(50);
/// The monitor keeps the last few raw messages, like the webview ring it
/// replaces.
const MONITOR_SIZE: usize = 6;

/// The connection status the webview shows (statusbar + picker). The
/// browser-permission states of the Web MIDI era (idle / requesting /
/// denied / unsupported) are gone — native access needs no gesture.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiStatusDto {
    pub connected: bool,
    /// The bound controller's raw port name, `None` when nothing matched.
    pub device_name: Option<String>,
    /// The bound controller's driver id (`"flx4"` / `"ddj400"`).
    pub driver_id: Option<String>,
    /// Every matched controller currently connected (for the picker).
    pub devices: Vec<String>,
}

/// One raw message for the monitor (the firmware-verification loop).
#[derive(Debug, Clone, Serialize)]
pub struct MonitorEntryDto {
    pub id: u64,
    pub bytes: Vec<u8>,
}

/// A repaint trigger for the LED painter thread.
enum Paint {
    /// The store changed — recompute from this snapshot.
    Snapshot(Box<InterfaceState>),
    /// A fresh bind: clear the diff baseline AND forget the tracked pad
    /// banks — the device (re)powered on in HOT CUE, whatever was active
    /// before the unplug.
    Rebind,
    /// A pad-mode switch: adopt the deck's active selector note (the mode
    /// LEDs paint from it), then repaint — the device cleared its pad LEDs
    /// on the switch.
    Bank { deck: usize, note: u8 },
    /// Periodic tick: refresh the engine-owned inputs (loop slots).
    Tick,
}

/// State shared between the IPC commands, the scanner thread, the input
/// callbacks, and the painter.
struct Shared {
    app: AppHandle,
    /// The bound controller's output connection (the LED / SysEx path).
    output: Mutex<Option<MidiOutputConnection>>,
    /// The active controller's translator — per-device state (14-bit MSB
    /// cache, SHIFT), rebuilt on every rebind.
    translator: Mutex<Flx4Translator>,
    monitor: Mutex<(u64, Vec<MonitorEntryDto>)>,
    status: Mutex<MidiStatusDto>,
    /// The user's explicit device pick (a port name); `None` = first match
    /// by registry order. Survives rescans so a chosen device stays chosen.
    selected: Mutex<Option<String>>,
    paint_tx: Sender<Paint>,
}

impl Shared {
    /// Send raw bytes to the bound controller; a missing device is a no-op.
    fn send(&self, bytes: &[u8]) {
        let mut output = self.output.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(connection) = output.as_mut() {
            let _ = connection.send(bytes);
        }
    }

    /// Handle one message from the bound controller (CoreMIDI thread).
    fn on_controller_message(&self, bytes: &[u8]) {
        {
            let mut monitor = self.monitor.lock().unwrap_or_else(|p| p.into_inner());
            let id = monitor.0;
            monitor.0 += 1;
            let entries = &mut monitor.1;
            if entries.len() >= MONITOR_SIZE {
                entries.remove(0);
            }
            entries.push(MonitorEntryDto { id, bytes: bytes.to_vec() });
        }
        let translated = {
            let mut translator = self.translator.lock().unwrap_or_else(|p| p.into_inner());
            translator.translate(bytes)
        };
        match translated {
            Translated::None => {}
            Translated::Intent(intent) => {
                // SHIFT originates HERE, so the store learns it at the source
                // (ADR-0020 phase A); the intent still rides to the webview,
                // whose copy projects it until the jog-steering consolidates.
                if let translate::Intent::Shift { deck, held } = intent {
                    if let Some(store) = self.app.try_state::<InterfaceStore>() {
                        store.set_deck_shift(deck.index(), held);
                    }
                }
                let _ = self.app.emit(INTENT_EVENT, &intent);
            }
            Translated::PerformancePad { deck, pad, down } => {
                if let Some(notes) = self.app.try_state::<NoteSteering>() {
                    notes.pad_event(deck.index(), pad as usize, down);
                }
            }
            Translated::PadModeSwitch { deck, note } => {
                if let Some(notes) = self.app.try_state::<NoteSteering>() {
                    notes.pad_mode_selected(deck.index(), note == translate::KEYBOARD_MODE_NOTE);
                }
                // The device cleared its pad LEDs on the switch — hand the
                // painter the fresh bank (it lights the selector's LED) and
                // repaint.
                let _ = self.paint_tx.send(Paint::Bank { deck: deck.index(), note });
            }
        }
    }

    /// Handle one message from a non-controller input (an external MIDI
    /// keyboard): note on/off edges steer the armed deck(s).
    fn on_keyboard_message(&self, bytes: &[u8]) {
        let (status, pitch, velocity) = match bytes {
            [status, pitch, velocity, ..] => (*status, *pitch, *velocity),
            _ => return,
        };
        let kind = status & 0xf0;
        let down = kind == 0x90 && velocity > 0;
        let up = kind == 0x80 || (kind == 0x90 && velocity == 0);
        if !(down || up) {
            return;
        }
        if let Some(notes) = self.app.try_state::<NoteSteering>() {
            notes.keyboard_event(pitch, down);
        }
    }

    fn publish_status(&self, next: MidiStatusDto) {
        let changed = {
            let mut status = self.status.lock().unwrap_or_else(|p| p.into_inner());
            if *status == next {
                false
            } else {
                *status = next.clone();
                true
            }
        };
        if changed {
            let _ = self.app.emit(STATUS_EVENT, &next);
        }
    }
}

/// The managed MIDI service: owns the scanner/painter threads and answers
/// the IPC commands. Connections live on the scanner thread; everything
/// else reaches the device through [`Shared`].
pub struct MidiService {
    shared: Arc<Shared>,
    /// Wakes the scanner early (a device pick should not wait out the poll).
    rescan_tx: Sender<()>,
}

impl MidiService {
    /// Spawn the scanner, painter, and ticker threads. Call once in `setup`
    /// (which Tauri runs on the MAIN thread — load-bearing, see below); the
    /// service lives in managed state for the app's lifetime.
    pub fn start(app: AppHandle) -> Self {
        // CoreMIDI delivers device-list updates to the run loop of the thread
        // that created the process's FIRST MIDI client. The scanner lives on
        // a plain background thread with no run loop, so without this anchor
        // the process's device snapshot freezes at the first scan and a
        // controller plugged in after launch NEVER appears — found on the
        // device (the FLX4 hot-plugged after start stayed "no controller
        // found"). Creating one client here, on the main thread whose event
        // loop Tauri pumps, anchors notification delivery for the process.
        // Dropping the wrapper is fine: coremidi 0.9 never disposes clients
        // (its `Drop` is deliberately disabled upstream), so the underlying
        // client — and the delivery it anchors — lives as long as the app.
        #[cfg(target_os = "macos")]
        if let Err(e) = MidiInput::new("LSDJ hot-plug anchor") {
            eprintln!("lsdj-app: midi hot-plug anchor failed: {e}");
        }
        let (paint_tx, paint_rx) = channel();
        let (rescan_tx, rescan_rx) = channel();
        let shared = Arc::new(Shared {
            app,
            output: Mutex::new(None),
            translator: Mutex::new(Flx4Translator::default()),
            monitor: Mutex::new((0, Vec::new())),
            status: Mutex::new(MidiStatusDto::default()),
            selected: Mutex::new(None),
            paint_tx,
        });
        {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("midi-scan".into())
                .spawn(move || run_scanner(shared, rescan_rx))
                .expect("spawn midi scanner");
        }
        {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("midi-leds".into())
                .spawn(move || run_painter(shared, paint_rx))
                .expect("spawn midi painter");
        }
        {
            let paint_tx = shared.paint_tx.clone();
            std::thread::Builder::new()
                .name("midi-led-tick".into())
                .spawn(move || loop {
                    std::thread::sleep(SCAN_INTERVAL);
                    if paint_tx.send(Paint::Tick).is_err() {
                        return;
                    }
                })
                .expect("spawn midi led ticker");
        }
        MidiService { shared, rescan_tx }
    }

    /// Register the store watcher that feeds the painter. Separate from
    /// [`MidiService::start`] because the store is managed after the service
    /// in `setup`.
    pub fn watch_store(&self, store: &InterfaceStore) {
        let paint_tx = self.shared.paint_tx.clone();
        store.watch(move |state| {
            let _ = paint_tx.send(Paint::Snapshot(Box::new(state.clone())));
        });
    }

    pub fn status(&self) -> MidiStatusDto {
        self.shared.status.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn monitor(&self) -> Vec<MonitorEntryDto> {
        self.shared.monitor.lock().unwrap_or_else(|p| p.into_inner()).1.clone()
    }

    pub fn select(&self, name: String) {
        *self.shared.selected.lock().unwrap_or_else(|p| p.into_inner()) = Some(name);
        let _ = self.rescan_tx.send(());
    }
}

/// One matched controller port.
struct Match {
    name: String,
    driver: &'static Driver,
}

/// Pick the active controller: an explicit selection while it is present,
/// else the first match by REGISTRY order — deterministic regardless of OS
/// enumeration order. Compares drivers by id, never by address: `DRIVERS` is
/// a `const`, and a const materializes a fresh copy at each use site, so
/// pointer identity across call sites is meaningless (the bug that left a
/// matched FLX4 permanently unbound — it was in the picker list but never
/// chosen).
fn pick_active<'m>(matched: &'m [Match], selected: Option<&str>) -> Option<&'m Match> {
    matched
        .iter()
        .find(|m| Some(m.name.as_str()) == selected)
        .or_else(|| {
            drivers::DRIVERS
                .iter()
                .find_map(|driver| matched.iter().find(|m| m.driver.id == driver.id))
        })
}

/// The scanner: enumerate → bind the picked/first controller → attach
/// non-controller inputs as keyboard-note sources → publish status; repeat
/// every second (or immediately on a device pick). ONE long-lived client
/// does every enumeration: the port list is process-global state kept fresh
/// by the main-thread anchor, and coremidi never disposes clients, so a
/// fresh client per scan would leak one into the MIDI server every second.
fn run_scanner(shared: Arc<Shared>, rescan_rx: Receiver<()>) {
    let scan_client = match MidiInput::new("LSDJ") {
        Ok(input) => input,
        Err(e) => {
            eprintln!("lsdj-app: midi scan client failed: {e}");
            return;
        }
    };
    // The active controller's input connection + name (owned here — dropping
    // a connection closes it).
    let mut controller: Option<(String, MidiInputConnection<()>)> = None;
    let mut keyboards: HashMap<String, MidiInputConnection<()>> = HashMap::new();
    loop {
        scan_once(&shared, &scan_client, &mut controller, &mut keyboards);
        match rescan_rx.recv_timeout(SCAN_INTERVAL) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn scan_once(
    shared: &Arc<Shared>,
    input: &MidiInput,
    controller: &mut Option<(String, MidiInputConnection<()>)>,
    keyboards: &mut HashMap<String, MidiInputConnection<()>>,
) {
    let ports = input.ports();
    let mut matched: Vec<Match> = Vec::new();
    let mut others: Vec<(String, midir::MidiInputPort)> = Vec::new();
    for port in ports {
        let Ok(name) = input.port_name(&port) else { continue };
        match driver_for_name(&name) {
            Some(driver) => matched.push(Match { name, driver }),
            None => others.push((name, port)),
        }
    }

    let selected = shared.selected.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let active = pick_active(&matched, selected.as_deref()).map(|m| (m.name.clone(), m.driver));

    // Rebind only when the active device actually changed: the translator
    // state (14-bit MSB cache, SHIFT) must survive a steady-state rescan,
    // and re-sending the position query would clobber software mixer moves.
    let current = controller.as_ref().map(|(name, _)| name.clone());
    if active.as_ref().map(|(name, _)| name) != current.as_ref() {
        *controller = None; // drop the old connection first
        *shared.output.lock().unwrap_or_else(|p| p.into_inner()) = None;
        if let Some((name, driver)) = &active {
            match connect_controller(shared, name) {
                Ok(connection) => {
                    // Non-RT setup logging, like the audio-device line.
                    println!("lsdj-app: midi controller bound — '{name}' ({})", driver.id);
                    *controller = Some((name.clone(), connection));
                    *shared.translator.lock().unwrap_or_else(|p| p.into_inner()) =
                        Flx4Translator::default();
                    bind_output(shared, driver, name);
                    // A freshly bound device powered up dark / stale — and in
                    // HOT CUE, so a pre-unplug KEYBOARD arm (200 ms chunks,
                    // held steering) must not survive the replug.
                    if let Some(notes) = shared.app.try_state::<NoteSteering>() {
                        for deck in 0..DECK_COUNT {
                            notes.pad_mode_selected(deck, false);
                        }
                    }
                    let _ = shared.paint_tx.send(Paint::Rebind);
                }
                Err(e) => eprintln!("lsdj-app: midi connect '{name}' failed: {e}"),
            }
        }
    }

    // Non-controller inputs attach as keyboard-note sources (issue #48) —
    // drop vanished ports, connect new ones.
    let present: Vec<String> = others.iter().map(|(name, _)| name.clone()).collect();
    keyboards.retain(|name, _| present.contains(name));
    for (name, port) in others {
        if keyboards.contains_key(&name) {
            continue;
        }
        let mut keyboard_input = match MidiInput::new("LSDJ") {
            Ok(input) => input,
            Err(_) => continue,
        };
        keyboard_input.ignore(Ignore::None);
        let router = shared.clone();
        match keyboard_input.connect(
            &port,
            "lsdj-keyboard",
            move |_stamp, bytes, _| router.on_keyboard_message(bytes),
            (),
        ) {
            Ok(connection) => {
                keyboards.insert(name, connection);
            }
            Err(e) => eprintln!("lsdj-app: midi keyboard connect failed: {e}"),
        }
    }

    let (device_name, driver_id) = match &active {
        Some((name, driver)) => (Some(name.clone()), Some(driver.id.to_string())),
        None => (None, None),
    };
    shared.publish_status(MidiStatusDto {
        connected: active.is_some(),
        device_name,
        driver_id,
        devices: matched.iter().map(|m| m.name.clone()).collect(),
    });
}

fn connect_controller(
    shared: &Arc<Shared>,
    name: &str,
) -> Result<MidiInputConnection<()>, String> {
    let mut input = MidiInput::new("LSDJ").map_err(|e| e.to_string())?;
    input.ignore(Ignore::None);
    let port = input
        .ports()
        .into_iter()
        .find(|p| input.port_name(p).is_ok_and(|n| n == name))
        .ok_or_else(|| "port vanished".to_string())?;
    let router = shared.clone();
    input
        .connect(
            &port,
            "lsdj-controller",
            move |_stamp, bytes, _| router.on_controller_message(bytes),
            (),
        )
        .map_err(|e| e.to_string())
}

/// Bind the controller's output port and send the driver's position query —
/// the device answers by dumping every analog position as CC traffic, which
/// flows through the translator like any other move.
fn bind_output(shared: &Arc<Shared>, driver: &Driver, name: &str) {
    let output = match MidiOutput::new("LSDJ") {
        Ok(output) => output,
        Err(e) => {
            eprintln!("lsdj-app: midi output client failed: {e}");
            return;
        }
    };
    let port = output.ports().into_iter().find(|port| {
        output
            .port_name(port)
            .ok()
            .and_then(|name| driver_for_name(&name))
            .is_some_and(|candidate| candidate.id == driver.id)
    });
    let Some(port) = port else {
        eprintln!("lsdj-app: no midi output port for '{name}'");
        return;
    };
    match output.connect(&port, "lsdj-leds") {
        Ok(mut connection) => {
            let _ = connection.send(driver.init_sysex);
            *shared.output.lock().unwrap_or_else(|p| p.into_inner()) = Some(connection);
        }
        Err(e) => eprintln!("lsdj-app: midi output connect failed: {e}"),
    }
}

/// The LED painter: recompute the full frame on every trigger and send only
/// the groups whose bytes changed (a Rebind clears the diff baseline — the
/// device's pads were just wiped). Loop-slot fill comes from the engine (the
/// truth of the audio), everything else from the store snapshot.
fn run_painter(shared: Arc<Shared>, paint_rx: Receiver<Paint>) {
    let mut snapshot: Option<InterfaceState> = None;
    let mut last_frame: Vec<Vec<[u8; 3]>> = Vec::new();
    // Each deck's active pad bank (the selector's note) — the device powers
    // on in HOT CUE, and every switch flows through Paint::Bank.
    let mut banks = [translate::PAD_MODE_PAIRS[0].0; DECK_COUNT];
    while let Ok(paint) = paint_rx.recv() {
        match paint {
            Paint::Snapshot(state) => snapshot = Some(*state),
            Paint::Rebind => {
                // Wait out the device's own LED clear (see REPAINT_SETTLE) —
                // painting first means painting into the wipe. Blocking the
                // painter briefly is fine: queued paints just batch behind.
                std::thread::sleep(REPAINT_SETTLE);
                last_frame.clear();
                // The fresh device is in HOT CUE, not wherever the old one was.
                banks = [translate::PAD_MODE_PAIRS[0].0; DECK_COUNT];
            }
            Paint::Bank { deck, note } => {
                if let Some(bank) = banks.get_mut(deck) {
                    *bank = note;
                }
                // Same settle as a plain repaint: the switch wiped the pads.
                std::thread::sleep(REPAINT_SETTLE);
                last_frame.clear();
            }
            Paint::Tick => {}
        }
        // Without a store snapshot yet there is nothing truthful to paint.
        let Some(state) = &snapshot else { continue };
        if !shared.status.lock().unwrap_or_else(|p| p.into_inner()).connected {
            continue;
        }
        let loop_filled: Vec<Vec<bool>> = match shared.app.try_state::<Host>() {
            Some(host) => (0..DECK_COUNT)
                .map(|deck| host.loop_slots(deck).iter().map(|slot| slot.filled).collect())
                .collect(),
            None => vec![vec![false; LOOP_SLOT_COUNT]; DECK_COUNT],
        };
        let frame = leds::full_frame(state, &loop_filled, &banks);
        for (index, group) in frame.iter().enumerate() {
            if last_frame.get(index) == Some(group) {
                continue;
            }
            for message in group {
                shared.send(message);
            }
        }
        last_frame = frame;
    }
}

// --- IPC commands (registered in lib.rs) ---

/// The connection status for the statusbar (initial hydration; changes
/// arrive on `midi://status`).
#[tauri::command]
pub fn midi_status(state: tauri::State<'_, MidiService>) -> MidiStatusDto {
    state.status()
}

/// The last few raw messages for the hex monitor (polled, like the webview
/// ring it replaces — the firmware-verification loop of ADR-0005 carried
/// forward).
#[tauri::command]
pub fn midi_monitor(state: tauri::State<'_, MidiService>) -> Vec<MonitorEntryDto> {
    state.monitor()
}

/// Choose which matched controller drives the app (the picker); rebinds on
/// the immediate rescan this triggers.
#[tauri::command]
pub fn midi_select(state: tauri::State<'_, MidiService>, name: String) {
    state.select(name);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matched(name: &str) -> Match {
        Match {
            name: name.to_string(),
            driver: driver_for_name(name).expect("a controller name"),
        }
    }

    /// The regression for the bug the FLX4 hit on the device: a matched
    /// controller must actually be PICKED. The old code compared drivers by
    /// address, and `DRIVERS` being a const means every use site gets its
    /// own copy — the pointers never matched and nothing ever bound.
    #[test]
    fn a_single_matched_controller_is_picked() {
        let ports = [matched("DDJ-FLX4")];
        let active = pick_active(&ports, None).expect("the FLX4 binds");
        assert_eq!(active.driver.id, "flx4");
        assert_eq!(active.name, "DDJ-FLX4");
    }

    #[test]
    fn registry_order_beats_enumeration_order() {
        // The OS lists the DDJ-400 first; the registry prefers the FLX4.
        let ports = [matched("DDJ-400"), matched("DDJ-FLX4 MIDI 1")];
        let active = pick_active(&ports, None).expect("a controller binds");
        assert_eq!(active.driver.id, "flx4");
    }

    #[test]
    fn an_explicit_selection_wins_while_present_and_falls_back_when_gone() {
        let ports = [matched("DDJ-FLX4"), matched("DDJ-400")];
        let active = pick_active(&ports, Some("DDJ-400")).expect("the pick binds");
        assert_eq!(active.driver.id, "ddj400");
        // The picked device unplugged: fall back to registry order, not to
        // nothing.
        let remaining = [matched("DDJ-FLX4")];
        let active = pick_active(&remaining, Some("DDJ-400")).expect("fallback binds");
        assert_eq!(active.driver.id, "flx4");
    }

    #[test]
    fn no_matches_means_no_active_controller() {
        assert!(pick_active(&[], None).is_none());
        assert!(pick_active(&[], Some("DDJ-FLX4")).is_none());
    }
}
