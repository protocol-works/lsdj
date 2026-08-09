//! The live beat-analysis path (ADR-0025): one non-realtime thread per deck,
//! fed by a bounded channel off the sidecar reader's PCM tee, publishing only
//! the gated `{bpm, confidence, live beat, origin}` into the interface store
//! (ADR-0020) and the gated period to the engine's synced echo.
//!
//! The `cpal` callback never appears here: the threads talk to the engine
//! exclusively through the [`Host`] command channel, like every other control
//! path. Resets ride the same channel as PCM, so tracker, gates, and the
//! frame origin reset atomically in stream order (ADR-0025: estimates never
//! span streams, and the pair must never decohere).

use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use lsdj_engine::host::Host;
use lsdj_engine::{DECK_COUNT, SAMPLE_RATE};
use tauri::{AppHandle, Manager};

use super::beat::{AdaptiveBeatTracker, AnchorGate, BeatGate};
use crate::store::{AnalysisSnap, InterfaceStore, LiveBeatSnap};

/// One estimate through the gate per second of PUSHED audio — the corpus
/// cadence (data-driven, so the harness and the live path share it exactly;
/// the TS path used a wall-clock interval over the same real-time feed).
const ESTIMATE_FRAMES: usize = SAMPLE_RATE as usize;

/// Bounded feed depth. The worker paces ~1 s PCM chunks and the estimator
/// consumes them in microseconds, so the queue stays near-empty; the bound
/// exists so a wedged thread can never balloon memory or block `run_reader`
/// (a full queue drops the chunk — the estimator tolerates a gap).
const FEED_CAPACITY: usize = 32;

/// How long a blocking reset waits for the deck thread to drain and apply it.
/// The queue holds at most [`FEED_CAPACITY`] chunks at microseconds each, so
/// this is generous; on timeout the caller proceeds (the reset is queued and
/// will still apply — only the strict ordering guarantee is lost).
const RESET_ACK_TIMEOUT: Duration = Duration::from_millis(500);

enum Msg {
    /// Interleaved-stereo f32 samples off the wire.
    Pcm(Vec<f32>),
    /// Stream discontinuity: reset tracker + gates and re-anchor the origin.
    /// Carrying the origin in the message keeps the reset atomic — the thread
    /// applies both in stream order, after any already-queued PCM.
    Reset {
        origin_frames: f64,
        ack: Option<SyncSender<()>>,
    },
}

/// The pure per-deck analysis state — everything except the IPC. Split from
/// the thread so the cadence, gating, and reset semantics unit-test without
/// Tauri (the store/engine publishing is the thread's few lines).
pub struct DeckAnalysis {
    tracker: AdaptiveBeatTracker,
    gate: BeatGate,
    anchors: AnchorGate,
    origin_frames: f64,
    since_estimate: usize,
}

impl DeckAnalysis {
    pub fn new() -> Self {
        DeckAnalysis {
            tracker: AdaptiveBeatTracker::new(SAMPLE_RATE as f64),
            gate: BeatGate::new(),
            anchors: AnchorGate::new(SAMPLE_RATE as f64),
            origin_frames: 0.0,
            since_estimate: 0,
        }
    }

    /// Feed a wire chunk; returns one snapshot per estimate tick it crossed
    /// (normally zero or one — the worker paces ~1 s chunks).
    pub fn on_pcm(&mut self, samples: &[f32]) -> Vec<AnalysisSnap> {
        self.tracker.push(samples);
        self.since_estimate += samples.len() / 2;
        let mut out = Vec::new();
        while self.since_estimate >= ESTIMATE_FRAMES {
            self.since_estimate -= ESTIMATE_FRAMES;
            let reading = self.tracker.estimate();
            let estimate = reading.estimate;
            let displayed = self.gate.push_adaptive(reading);
            let live = self
                .anchors
                .push(displayed, estimate.and_then(|e| e.anchor_frame));
            out.push(AnalysisSnap {
                bpm: displayed,
                confidence: estimate.map_or(0.0, |e| e.confidence),
                live_beat: live.map(|l| LiveBeatSnap {
                    anchor_frame: l.anchor_frame,
                    bpm: l.bpm,
                }),
                origin_frames: self.origin_frames,
            });
        }
        out
    }

    /// Atomic stream reset: tracker, both gates, the estimate cadence, and
    /// the frame origin move together; returns the blank snapshot to publish.
    pub fn on_reset(&mut self, origin_frames: f64) -> AnalysisSnap {
        self.tracker.reset();
        self.gate.reset();
        self.anchors.reset();
        self.since_estimate = 0;
        self.origin_frames = origin_frames;
        AnalysisSnap {
            bpm: None,
            confidence: 0.0,
            live_beat: None,
            origin_frames,
        }
    }
}

impl Default for DeckAnalysis {
    fn default() -> Self {
        Self::new()
    }
}

/// The handle the PCM tee and the IPC commands feed: per-deck bounded senders
/// into the analysis threads. Cloneable (an `Arc`), held in Tauri managed
/// state and captured by the sidecar tee closures.
#[derive(Clone)]
pub struct AnalysisFeed {
    senders: Arc<Vec<SyncSender<Msg>>>,
}

impl AnalysisFeed {
    /// A feed whose receivers are dropped — every send is a silent no-op. For
    /// tests that need the tee wiring without analysis threads (no `AppHandle`).
    #[cfg(test)]
    pub fn disconnected(deck_count: usize) -> Self {
        AnalysisFeed {
            senders: Arc::new((0..deck_count).map(|_| sync_channel(1).0).collect()),
        }
    }

    /// Feed raw interleaved-stereo f32 LE wire bytes (the tee's format).
    /// Called from the NON-RT sidecar reader thread; never blocks — a full
    /// queue drops the chunk (the estimator tolerates a gap; `run_reader`
    /// must never stall behind analysis).
    pub fn pcm_bytes(&self, deck: usize, bytes: &[u8]) {
        let Some(sender) = self.senders.get(deck) else {
            return;
        };
        let samples = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let _ = sender.try_send(Msg::Pcm(samples));
    }

    /// Queue a stream reset with the origin captured now (fire-and-forget —
    /// the transport paths, where nothing races the published period).
    pub fn reset(&self, deck: usize, origin_frames: f64) {
        if let Some(sender) = self.senders.get(deck) {
            let _ = sender.try_send(Msg::Reset {
                origin_frames,
                ack: None,
            });
        }
    }

    /// Queue a stream reset and WAIT until the deck thread has applied it —
    /// the track-load path uses this so its subsequent track-period write can
    /// never be overtaken by a stale blank from the analysis thread.
    pub fn reset_blocking(&self, deck: usize, origin_frames: f64) {
        let Some(sender) = self.senders.get(deck) else {
            return;
        };
        let (ack, applied) = sync_channel(1);
        if sender
            .send(Msg::Reset {
                origin_frames,
                ack: Some(ack),
            })
            .is_ok()
        {
            let _ = applied.recv_timeout(RESET_ACK_TIMEOUT);
        }
    }
}

/// Spawn one analysis thread per deck. Threads live for the app's lifetime
/// (their senders sit in managed state, so the channels never close) and
/// publish through `app`: the snapshot into the [`InterfaceStore`], the gated
/// period to the [`Host`] echo. `try_state` on both — the threads start
/// before `setup` manages them, and pre-boot chunks can't carry a beat.
pub fn spawn(app: AppHandle) -> AnalysisFeed {
    let senders = (0..DECK_COUNT)
        .map(|deck| {
            let (sender, receiver) = sync_channel::<Msg>(FEED_CAPACITY);
            let app = app.clone();
            thread::Builder::new()
                .name(format!("lsdj-analysis-{deck}"))
                .spawn(move || run_deck(deck, receiver, app))
                .expect("failed to spawn lsdj analysis thread");
            sender
        })
        .collect();
    AnalysisFeed {
        senders: Arc::new(senders),
    }
}

fn run_deck(deck: usize, receiver: Receiver<Msg>, app: AppHandle) {
    let mut state = DeckAnalysis::new();
    // The gated readout this thread last pushed to the engine's echo clock.
    // Pushing only on a real transition keeps the render-thread channel quiet
    // AND scopes ownership: a blank push happens only when THIS thread had
    // set a period, so a reset can never stomp a playback deck's track clock.
    let mut period_pushed: Option<f64> = None;
    while let Ok(msg) = receiver.recv() {
        match msg {
            Msg::Pcm(samples) => {
                for snap in state.on_pcm(&samples) {
                    publish(&app, deck, snap, &mut period_pushed);
                }
            }
            Msg::Reset { origin_frames, ack } => {
                let snap = state.on_reset(origin_frames);
                publish(&app, deck, snap, &mut period_pushed);
                if let Some(ack) = ack {
                    let _ = ack.send(());
                }
            }
        }
    }
}

fn publish(app: &AppHandle, deck: usize, snap: AnalysisSnap, period_pushed: &mut Option<f64>) {
    if let Some(store) = app.try_state::<InterfaceStore>() {
        store.set_analysis(deck, snap);
    }
    if snap.bpm != *period_pushed {
        *period_pushed = snap.bpm;
        if let Some(host) = app.try_state::<Host>() {
            host.set_beat_period(deck, snap.bpm.map(|bpm| (60.0 / bpm) as f32));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::beat::fixtures::click_track;

    const SR: f64 = SAMPLE_RATE as f64;

    /// Feed `seconds` of a click train in ~1 s wire chunks (the worker's
    /// pacing), collecting every published snapshot.
    fn feed_clicks(
        state: &mut DeckAnalysis,
        bpm: f64,
        seconds: usize,
        seed: u32,
    ) -> Vec<AnalysisSnap> {
        let samples = click_track(bpm, seconds as f64, SR, seed);
        let chunk = SAMPLE_RATE as usize * 2;
        let mut snaps = Vec::new();
        for piece in samples.chunks(chunk) {
            snaps.extend(state.on_pcm(piece));
        }
        snaps
    }

    #[test]
    fn publishes_one_snapshot_per_pushed_second() {
        let mut state = DeckAnalysis::new();
        let snaps = feed_clicks(&mut state, 128.0, 16, 1);
        assert_eq!(snaps.len(), 16);
    }

    #[test]
    fn acquires_a_click_train_and_publishes_the_gated_set() {
        let mut state = DeckAnalysis::new();
        let snaps = feed_clicks(&mut state, 128.0, 16, 1);
        // Acquisition is gated: early snapshots are blank, the tail shows.
        assert!(snaps.first().expect("snapshots published").bpm.is_none());
        let last = snaps.last().expect("snapshots published");
        let shown = last.bpm.expect("gate acquired the click train");
        assert!((shown - 128.0).abs() / 128.0 < 0.02, "shown {shown}");
        assert!(last.confidence > 0.4);
        let live = last.live_beat.expect("anchor agreed");
        assert_eq!(live.bpm, shown);
        // The anchor sits on the click lattice (pushed-frame domain).
        let period_frames = (60.0 / shown) * SR;
        let phase = (live.anchor_frame % period_frames) / period_frames;
        assert!(phase.min(1.0 - phase) <= 0.12, "phase {phase}");
    }

    #[test]
    fn reset_blanks_atomically_and_re_anchors_the_origin() {
        let mut state = DeckAnalysis::new();
        let acquired = feed_clicks(&mut state, 128.0, 16, 1);
        assert!(acquired.last().expect("snapshots").bpm.is_some());
        let blank = state.on_reset(123_456.0);
        assert_eq!(blank.bpm, None);
        assert_eq!(blank.live_beat, None);
        assert_eq!(blank.origin_frames, 123_456.0);
        // The tracker restarted from zero: the next second estimates nothing
        // (6 s minimum window), and later snapshots carry the new origin.
        let after = feed_clicks(&mut state, 128.0, 3, 2);
        assert!(after.iter().all(|s| s.bpm.is_none()));
        assert!(after.iter().all(|s| s.origin_frames == 123_456.0));
    }
}
