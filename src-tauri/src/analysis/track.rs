//! Per-deck offline track analysis held shell-side (ADR-0030): the loaded
//! track's tempo (the echo clock follows it through varispeed) and its band
//! profile (the webview fetches it once per load, the `track_peaks`
//! pattern). Filled by the load commands, cleared on unload. A queued clip's
//! analysis waits as the deck's *pending* one until the webview sees the
//! engine swap and promotes it.

use std::sync::Mutex;

use super::bands::TrackBands;

/// One loaded track's analysis.
pub struct DeckTrack {
    /// The grid-refined tempo when a grid exists, else the coarse verdict —
    /// `None` for a track that cleared neither honesty bar.
    pub bpm: Option<f64>,
    pub bands: TrackBands,
}

/// One deck's analyses: the playing track's, and the queued clip's.
#[derive(Default)]
struct DeckSlot {
    loaded: Option<DeckTrack>,
    pending: Option<DeckTrack>,
}

pub struct TrackAnalysis {
    decks: Vec<Mutex<DeckSlot>>,
}

impl TrackAnalysis {
    pub fn new(deck_count: usize) -> Self {
        TrackAnalysis {
            decks: (0..deck_count).map(|_| Mutex::new(DeckSlot::default())).collect(),
        }
    }

    fn slot(&self, deck: usize) -> Option<std::sync::MutexGuard<'_, DeckSlot>> {
        self.decks
            .get(deck)
            .map(|slot| slot.lock().unwrap_or_else(|p| p.into_inner()))
    }

    /// A load: the track's analysis takes over and any pending one goes (the
    /// engine's load discarded the queue it belonged to).
    pub fn set(&self, deck: usize, track: DeckTrack) {
        if let Some(mut slot) = self.slot(deck) {
            *slot = DeckSlot {
                loaded: Some(track),
                pending: None,
            };
        }
    }

    pub fn clear(&self, deck: usize) {
        if let Some(mut slot) = self.slot(deck) {
            *slot = DeckSlot::default();
        }
    }

    /// Park a queued clip's analysis until its swap is promoted; replaces any
    /// earlier pending one, like the engine queue.
    pub fn set_pending(&self, deck: usize, track: DeckTrack) {
        if let Some(mut slot) = self.slot(deck) {
            slot.pending = Some(track);
        }
    }

    pub fn clear_pending(&self, deck: usize) {
        if let Some(mut slot) = self.slot(deck) {
            slot.pending = None;
        }
    }

    /// The swap happened: the pending analysis becomes the loaded one. `false`
    /// (and nothing changes) when nothing is pending.
    pub fn promote(&self, deck: usize) -> bool {
        let Some(mut slot) = self.slot(deck) else {
            return false;
        };
        match slot.pending.take() {
            Some(track) => {
                slot.loaded = Some(track);
                true
            }
            None => false,
        }
    }

    /// The echo period for a varispeed change: `None` = no track loaded
    /// (leave the echo clock alone — a live deck owns it); `Some(inner)` = a
    /// track is loaded and the clock follows `60 / (bpm × rate)`, free-running
    /// (`inner = None`) when the track never cleared the honesty bar.
    pub fn beat_period_at_rate(&self, deck: usize, rate: f64) -> Option<Option<f32>> {
        let slot = self.slot(deck)?;
        let track = slot.loaded.as_ref()?;
        Some(
            track
                .bpm
                .filter(|_| rate > 0.0)
                .map(|bpm| (60.0 / (bpm * rate)) as f32),
        )
    }

    /// The loaded track's band profile framed for binary IPC:
    /// `[u32 LE hop count][low f32 LE…][mid…][high…]`. `None` with no track.
    pub fn bands_payload(&self, deck: usize) -> Option<Vec<u8>> {
        let slot = self.slot(deck)?;
        let track = slot.loaded.as_ref()?;
        let bands = &track.bands;
        let hops = bands.hops();
        let mut payload = Vec::with_capacity(4 + hops * 3 * 4);
        payload.extend_from_slice(&(hops as u32).to_le_bytes());
        for lane in [&bands.low, &bands.mid, &bands.high] {
            for value in lane.iter() {
                payload.extend_from_slice(&value.to_le_bytes());
            }
        }
        Some(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(bpm: Option<f64>, hops: usize) -> DeckTrack {
        DeckTrack {
            bpm,
            bands: TrackBands {
                low: vec![0.1; hops],
                mid: vec![0.2; hops],
                high: vec![0.3; hops],
            },
        }
    }

    #[test]
    fn varispeed_period_follows_the_loaded_tempo() {
        let tracks = TrackAnalysis::new(2);
        // No track: leave the echo clock alone.
        assert_eq!(tracks.beat_period_at_rate(0, 1.0), None);
        tracks.set(0, track(Some(120.0), 4));
        let period = tracks.beat_period_at_rate(0, 1.0).expect("track loaded");
        assert!((period.expect("has tempo") - 0.5).abs() < 1e-6);
        // Varispeed scales the clock (the M14 consumer rule).
        let faster = tracks.beat_period_at_rate(0, 1.05).unwrap().unwrap();
        assert!((faster - (60.0 / 126.0) as f32).abs() < 1e-6);
        // A gridless track free-runs; unload leaves the clock alone again.
        tracks.set(0, track(None, 4));
        assert_eq!(tracks.beat_period_at_rate(0, 1.0), Some(None));
        tracks.clear(0);
        assert_eq!(tracks.beat_period_at_rate(0, 1.0), None);
    }

    #[test]
    fn bands_payload_frames_hops_then_three_lanes() {
        let tracks = TrackAnalysis::new(2);
        assert!(tracks.bands_payload(0).is_none());
        tracks.set(0, track(Some(120.0), 3));
        let payload = tracks.bands_payload(0).expect("track loaded");
        assert_eq!(payload.len(), 4 + 3 * 3 * 4);
        assert_eq!(u32::from_le_bytes(payload[..4].try_into().unwrap()), 3);
        let first_low = f32::from_le_bytes(payload[4..8].try_into().unwrap());
        let first_mid = f32::from_le_bytes(payload[16..20].try_into().unwrap());
        let first_high = f32::from_le_bytes(payload[28..32].try_into().unwrap());
        assert_eq!((first_low, first_mid, first_high), (0.1, 0.2, 0.3));
    }

    #[test]
    fn a_pending_analysis_waits_for_its_promotion() {
        let tracks = TrackAnalysis::new(2);
        tracks.set(0, track(Some(120.0), 3));
        tracks.set_pending(0, track(Some(60.0), 5));
        // Until the swap is promoted the playing track's analysis stands.
        assert_eq!(tracks.beat_period_at_rate(0, 1.0), Some(Some(0.5)));
        assert!(tracks.promote(0), "something was pending");
        assert_eq!(tracks.beat_period_at_rate(0, 1.0), Some(Some(1.0)));
        assert_eq!(u32::from_le_bytes(tracks.bands_payload(0).unwrap()[..4].try_into().unwrap()), 5);
        assert!(!tracks.promote(0), "nothing left to promote");
        assert_eq!(tracks.beat_period_at_rate(0, 1.0), Some(Some(1.0)), "a no-op promote keeps the track");
    }

    #[test]
    fn a_load_or_clear_drops_the_pending_analysis() {
        let tracks = TrackAnalysis::new(2);
        tracks.set_pending(0, track(Some(100.0), 5));
        tracks.set(0, track(Some(120.0), 3));
        assert!(!tracks.promote(0), "the load discarded the queue");
        tracks.set_pending(0, track(Some(100.0), 5));
        tracks.clear_pending(0);
        assert!(!tracks.promote(0), "cleared");
        tracks.set_pending(0, track(Some(100.0), 5));
        tracks.clear(0);
        assert!(!tracks.promote(0), "unload clears both");
        assert_eq!(tracks.beat_period_at_rate(0, 1.0), None);
    }

    #[test]
    fn out_of_range_decks_are_silent_no_ops() {
        let tracks = TrackAnalysis::new(2);
        tracks.set(9, track(Some(120.0), 1));
        tracks.set_pending(9, track(Some(120.0), 1));
        assert!(!tracks.promote(9));
        tracks.clear(9);
        assert_eq!(tracks.beat_period_at_rate(9, 1.0), None);
        assert!(tracks.bands_payload(9).is_none());
    }
}
