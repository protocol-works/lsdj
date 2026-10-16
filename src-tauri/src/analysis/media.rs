//! BPM + key for the media libraries: each registry row carries a
//! [`TrackInfo`], filled by one background thread per library so neither a
//! list nor a record ever waits on a decode. A list or a record kicks the
//! thread; it analyses every row not yet at [`ANALYSIS_VERSION`] — new takes
//! and the backlog alike — writes each verdict back, and emits
//! `library://changed` so the Media Explorer re-lists.

use std::sync::mpsc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

/// Bump to re-analyse every row once (after a detector change).
pub const ANALYSIS_VERSION: u32 = 1;

/// The analysis half of a registry row, flattened into it (`bpm`, `key`,
/// `keyName`, `analyzed`), every field absent until known so old rows stay
/// readable and unanalysed rows serialise as before.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackInfo {
    /// The deck's own tempo verdict (grid-refined when a grid fits); `None`
    /// when the honesty bar refuses, e.g. beatless or drifting material.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bpm: Option<f64>,
    /// Camelot code, e.g. `"8A"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Conventional key name, e.g. `"A min"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_name: Option<String>,
    /// The [`ANALYSIS_VERSION`] that produced this verdict — `None` values
    /// included, so an atonal or undecodable file is not re-tried forever.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub analyzed: u32,
}

fn is_zero(version: &u32) -> bool {
    *version == 0
}

impl TrackInfo {
    pub fn is_current(&self) -> bool {
        self.analyzed >= ANALYSIS_VERSION
    }
}

/// Decode and analyse one file, the same way a deck load does (ADR-0030).
pub fn analyze(bytes: Vec<u8>, extension: Option<&str>) -> Result<TrackInfo, String> {
    let rate = lsdj_engine::SAMPLE_RATE as f64;
    let decoded = crate::decode::decode_to_48k(bytes, extension)?;
    let coarse = super::beat::track_bpm(&decoded.left, &decoded.right, rate);
    let grid = super::grid::track_beatgrid(&decoded.left, &decoded.right, rate, coarse);
    let key = super::key::track_key(&decoded.left, &decoded.right, rate);
    Ok(TrackInfo {
        // One decimal: what a DJ reads, and stable across re-analysis noise.
        bpm: grid
            .map(|g| g.bpm)
            .or(coarse)
            .map(|b| (b * 10.0).round() / 10.0),
        key_name: key.as_ref().map(|k| k.name.clone()),
        key: key.map(|k| k.camelot),
        analyzed: ANALYSIS_VERSION,
    })
}

/// What the worker needs from a library (songs or samples).
pub trait AnalyzedLibrary: Send + Sync + 'static {
    /// The `library://changed` name the webview re-lists on.
    const LIBRARY: &'static str;
    /// Hand the library its kick channel; it sends on list and record.
    fn set_analyzer(&self, kick: mpsc::Sender<()>);
    /// Every registry file whose [`TrackInfo`] is not current.
    fn pending_analysis(&self) -> Vec<String>;
    fn read(&self, name: &str) -> Result<Vec<u8>, String>;
    fn set_info(&self, name: &str, info: TrackInfo) -> Result<(), String>;
}

/// Start `L`'s analysis thread (call once, after the library is managed).
/// ponytail: one file at a time on one plain thread — a 3-minute take is
/// ~0.5 s of CPU, so a few-hundred-row backlog drains in minutes; lower the
/// thread priority if that ever competes with a live set.
pub fn spawn<L: AnalyzedLibrary>(app: AppHandle) {
    let (kick, rx) = mpsc::channel();
    app.state::<L>().set_analyzer(kick);
    let spawned = std::thread::Builder::new()
        .name(format!("lsdj-{}-analysis", L::LIBRARY))
        .spawn(move || {
            while rx.recv().is_ok() {
                // A burst of lists/records is one pass.
                while rx.try_recv().is_ok() {}
                let library = app.state::<L>();
                for file in library.pending_analysis() {
                    let extension = std::path::Path::new(&file)
                        .extension()
                        .and_then(|ext| ext.to_str());
                    let info = library
                        .read(&file)
                        .and_then(|bytes| analyze(bytes, extension))
                        .unwrap_or_else(|e| {
                            eprintln!("lsdj-app: cannot analyse {file}: {e}");
                            TrackInfo {
                                analyzed: ANALYSIS_VERSION,
                                ..TrackInfo::default()
                            }
                        });
                    // Only a landed write re-lists: a row that keeps failing to
                    // save must not loop list → kick → fail.
                    match library.set_info(&file, info) {
                        Ok(()) => crate::watcher::emit_library_changed(&app, L::LIBRARY),
                        Err(e) => eprintln!("lsdj-app: cannot save analysis for {file}: {e}"),
                    }
                }
            }
        });
    if let Err(e) = spawned {
        eprintln!("lsdj-app: {} analysis unavailable: {e}", L::LIBRARY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_without_analysis_read_and_write_unchanged() {
        let info: TrackInfo = serde_json::from_str("{}").unwrap();
        assert!(!info.is_current());
        assert_eq!(serde_json::to_string(&info).unwrap(), "{}");
        let done = TrackInfo {
            bpm: Some(124.0),
            key: Some("8A".into()),
            key_name: Some("A min".into()),
            analyzed: ANALYSIS_VERSION,
        };
        let json = serde_json::to_string(&done).unwrap();
        assert!(json.contains("\"keyName\":\"A min\""));
        assert_eq!(serde_json::from_str::<TrackInfo>(&json).unwrap(), done);
    }

    #[test]
    fn analyze_reads_a_wav_and_refuses_junk_with_an_error() {
        let mut cursor = std::io::Cursor::new(Vec::new());
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::new(&mut cursor, spec).unwrap();
        // 8 s of an A minor triad: a key, but no beat.
        for i in 0..48_000 * 8 {
            let t = i as f64 / 48_000.0;
            let s: f64 = [220.0, 261.63, 329.63]
                .iter()
                .map(|f| (2.0 * std::f64::consts::PI * f * t).sin() * 0.2)
                .sum();
            let sample = (s * i16::MAX as f64) as i16;
            writer.write_sample(sample).unwrap();
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
        let info = analyze(cursor.into_inner(), Some("wav")).unwrap();
        assert_eq!(info.key.as_deref(), Some("8A"));
        assert_eq!(info.bpm, None);
        assert!(info.is_current());
        assert!(analyze(b"not audio".to_vec(), None).is_err());
    }
}
