//! `device_run` — drive the LSDJ engine library against a real output
//! device for N seconds with synthetic per-deck producers. This is the
//! production replacement for the Spike A `rt_engine`: same behaviour (two decks,
//! per-deck rings, RT-safe callback, FTZ/DAZ, `assert_no_alloc`), now built ON
//! the library so the device path stays exercisable.
//!
//! Usage: `device_run [--duration SECS]` (default 30 s).
//!
//! Graceful no-device exit: in a sandbox / headless CI it prints a message and
//! exits 0 — it never hangs.

use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use lsdj_engine::device::{run_stream, DeviceError};
use lsdj_engine::{DeckHandle, Engine, CHANNELS, SAMPLE_RATE};

// Register the allocator guard so `assert_no_alloc` in the device callback is
// armed. `warn_release` (set in Cargo.toml) makes a callback alloc WARN, never
// abort — an alloc must not crash the stream.
#[global_allocator]
static GUARD: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

fn parse_duration() -> u64 {
    let args: Vec<String> = env::args().collect();
    let mut duration = 30u64;
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--duration" {
            if let Some(v) = args.get(i + 1).and_then(|s| s.parse().ok()) {
                duration = v;
            }
            i += 1;
        }
        i += 1;
    }
    duration
}

/// A producer for one deck: generates a distinct sine into its ring at worker
/// pace, staying ≤ 3 s ahead by checking free space before each chunk. The
/// producer thread is non-RT; it is the SOLE writer of the deck's ring via
/// `DeckHandle::post_pcm`.
fn spawn_producer(
    mut handle: DeckHandle,
    freq: f32,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let chunk_frames = SAMPLE_RATE as usize; // 1.0 s
        let chunk_samples = chunk_frames * CHANNELS as usize;
        let mut phase: f32 = 0.0;
        let dphase = 2.0 * std::f32::consts::PI * freq / SAMPLE_RATE as f32;
        // Allocate the reusable chunk ONCE, off any RT path.
        let mut chunk = vec![0.0f32; chunk_samples];
        // Stay ≤ 3 s ahead.
        let lead_limit_samples = 3 * SAMPLE_RATE as usize * CHANNELS as usize;

        while !stop.load(Ordering::Relaxed) {
            // Only generate when there is room for a chunk and we are not too far
            // ahead. `free_samples` is the ring's free space.
            let free = handle.free_samples();
            if free < chunk_samples || free > lead_limit_samples {
                // Either no room yet, or we're well ahead — yield briefly. (The
                // "too far ahead" check uses free space as a proxy for lead.)
                if free < chunk_samples {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                }
            }
            for f in 0..chunk_frames {
                let s = phase.sin() * 0.25;
                phase += dphase;
                if phase > std::f32::consts::TAU {
                    phase -= std::f32::consts::TAU;
                }
                chunk[2 * f] = s;
                chunk[2 * f + 1] = s;
            }
            let mut written = 0usize;
            while written < chunk.len() && !stop.load(Ordering::Relaxed) {
                let n = handle.post_pcm(&chunk[written..]);
                written += n;
                if n == 0 {
                    thread::sleep(Duration::from_micros(200));
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
    })
}

fn main() {
    let duration = parse_duration();

    let mut engine = Engine::new();
    let deck_a = engine.create_deck(0);
    let deck_b = engine.create_deck(1);
    let telemetry = engine.telemetry();

    // Spawn the non-RT producers BEFORE the stream so the rings prebuffer.
    let stop = Arc::new(AtomicBool::new(false));
    let h_a = spawn_producer(deck_a, 220.0, stop.clone());
    let h_b = spawn_producer(deck_b, 330.0, stop.clone());

    let stream = match run_stream(engine) {
        Ok(s) => s,
        Err(DeviceError::Unavailable(msg)) => {
            println!("device_run: {msg} — exiting cleanly (sandbox/headless).");
            stop.store(true, Ordering::Release);
            let _ = h_a.join();
            let _ = h_b.join();
            return;
        }
        Err(DeviceError::Stream(msg)) => {
            println!("device_run: {msg} — exiting cleanly.");
            stop.store(true, Ordering::Release);
            let _ = h_a.join();
            let _ = h_b.join();
            return;
        }
    };

    let info = stream.info();
    println!(
        "device_run: device='{}' channels={} rate={} buffer={:?} — RUNNING for {duration}s",
        info.device_name, info.device_channels, info.sample_rate, info.buffer_frames
    );

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(duration) {
        thread::sleep(Duration::from_millis(250));
    }

    drop(stream); // stop the callback before reading final telemetry
    stop.store(true, Ordering::Release);
    let _ = h_a.join();
    let _ = h_b.join();

    let underruns = telemetry.underruns();
    let blocks = telemetry.blocks();
    println!("\n===== device_run RESULT =====");
    println!("blocks rendered     : {blocks}");
    println!("underruns (post-pre): {underruns}");
    for d in 0..2 {
        println!(
            "deck {d} ring fill     : min={} max={} frames  primed={}",
            telemetry.ring_fill_min(d),
            telemetry.ring_fill_max(d),
            telemetry.primed(d),
        );
    }
    println!(
        "verdict             : {}",
        if underruns == 0 {
            "ZERO underruns"
        } else {
            "UNDERRUNS PRESENT"
        }
    );
}
