//! The playback buffer source (Phase 1, Slice 4a) — a deck playing a decoded
//! track instead of the live Magenta stream (M19 playback, M20 varispeed,
//! M21/M23 track loops, peaks; ADR-0013/0014/0015/0016).
//!
//! This is the native home of the Web Audio playback path
//! (`frontend/src/audio/engine.ts` `DeckChannel` track methods +
//! `frontend/src/audio/track.ts` pure transport math). The browser had to anchor
//! offsets against context time because an `AudioBufferSourceNode` can neither
//! pause nor report a playhead; here the playhead IS the state, advanced directly
//! by the RT [`BufferSource::pop_frame`] each output sample.
//!
//! # The mode handoff (ADR-0013)
//!
//! Loading decides the mode: [`DeckSource::Realtime`] (the per-deck
//! [`RingConsumer`], Slices 1–3) or [`DeckSource::Playback`] (a [`BufferSource`]).
//! `load_track` switches a deck to `Playback` and **parks** the live ring (kept,
//! not destroyed) so `unload_track` can restore it. `render`/`mix_frame` pulls
//! each deck's stereo frame from whichever source is active; the rest of the
//! channel (EQ/FX/volume/crossfade/limiter) is unchanged.
//!
//! # Varispeed = plain resampling (ADR-0014)
//!
//! Per output sample the fractional playhead advances by the rate; the output is
//! **linear-interpolated** between the two buffer samples bracketing it — exactly
//! `AudioBufferSourceNode.playbackRate`, so pitch shifts with rate. At rate 1.0
//! the playhead lands on integers and the buffer is read verbatim.
//!
//! # Loops (ADR-0015/0016)
//!
//! The loop region lives on the source in frames; the playhead **folds** through
//! `[start, end)` (a pure [`fold_into_loop`]). **Any seek exits the loop** — the
//! one rule, no dormant regions. The two moments the playhead can sit outside the
//! region on a *set* get a deterministic [`plan_loop_set`] decision (restart /
//! reanchor / park / apply / refuse) rather than relying on the wrap-on-reach
//! edge.

use crate::CHANNELS;

/// Interleaved-stereo samples per frame.
const SAMPLES_PER_FRAME: usize = CHANNELS as usize;

/// The widest varispeed envelope the performer can select (ADR-0014, amended
/// 2026-10-15): ±50 %. Mirrors the largest `TEMPO_RANGES` entry in
/// `frontend/src/audio/track.ts`; the webview clamps to the chosen range, and the
/// engine boundary holds this outer envelope rather than trusting callers.
pub const TRACK_RATE_RANGE: f64 = 0.5;

/// The shortest honest loop in frames (ADR-0015/0016): sub-quantum regions are
/// where implementations differ, and a near-zero loop is a buzz, not a loop.
/// Mirrors `MIN_TRACK_LOOP_SECONDS` (0.05 s) in `track.ts`, in frames at the
/// engine's fixed [`crate::SAMPLE_RATE`].
pub const MIN_TRACK_LOOP_FRAMES: u64 = (0.05 * crate::SAMPLE_RATE as f64) as u64;

/// Clamp a varispeed rate into the widest envelope (`clampRate` in `track.ts`). A
/// non-finite or non-positive rate falls back to 1.0 (as-recorded).
pub fn clamp_rate(rate: f64) -> f64 {
    if !rate.is_finite() || rate <= 0.0 {
        return 1.0;
    }
    rate.clamp(1.0 - TRACK_RATE_RANGE, 1.0 + TRACK_RATE_RANGE)
}

// --- Phase-nudge (platter bend) tuning, mirroring `bendPlan` in `track.ts` ---
//
// A jog-while-playing nudge (M20, ADR-0014) slips the playhead by a small amount
// via a *stepped rate bend* — a click-free catch-up, never a position jump. The
// backlog is consumed by briefly playing faster (push ahead) or slower (drag
// back) so the playhead only ever advances forward; only its phase moves.
//
/// Gentlest bend for a single tick (the bent rate is `rate * (1 ± fraction)`).
const NUDGE_BEND_FRACTION: f64 = 0.05;
/// Steepest bend a backlog may demand (beyond this the queue caps instead).
const MAX_BEND_FRACTION: f64 = 0.5;
/// Slip beyond what the gentlest bend clears in this time bends steeper, so a
/// platter spin answers in bounded time rather than grinding for seconds.
const BEND_CATCH_UP_SECONDS: f64 = 0.25;
/// The deepest slip backlog worth keeping (`MAX_PENDING_SLIP_SECONDS` = 1 s in
/// `track.ts`): a platter is a phase tool, not a seek.
const MAX_PENDING_SLIP_FRAMES: f64 = crate::SAMPLE_RATE as f64;

/// Clamp a seek target (in frames) into `[0, duration]` (`clampOffset` in
/// `track.ts`; garbage seeks to the top).
fn clamp_offset(frames: f64, duration_frames: u64) -> f64 {
    if !frames.is_finite() || frames < 0.0 {
        return 0.0;
    }
    frames.min(duration_frames as f64)
}

/// An active loop region in frames (`TrackLoop` in `track.ts`). Half-open
/// `[start, end)`: the playhead folds back to `start` on reaching `end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopRegion {
    pub start: u64,
    pub end: u64,
}

impl LoopRegion {
    fn len(self) -> u64 {
        self.end - self.start
    }
}

/// Fold a linearly-advanced playhead into an active loop (`foldIntoLoop` in
/// `track.ts`): linear until the region's end, then wrapping — the same path the
/// audio takes through the native loop, so every position consumer stays truthful
/// inside it. A playhead already past the end (a quantised OUT can land just
/// behind the press) wraps exactly like the source does on its next check.
pub fn fold_into_loop(frames: f64, loop_region: LoopRegion) -> f64 {
    let end = loop_region.end as f64;
    if frames < end {
        return frames;
    }
    let len = loop_region.len() as f64;
    loop_region.start as f64 + (frames - end) % len
}

/// What setting a loop does to the transport (`planLoopSet` in `track.ts`,
/// ADR-0015). Pure so the spec-edge branches are unit-tested and the source only
/// executes them. Frames replace `track.ts`'s seconds; the decisions are
/// identical.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LoopSetPlan {
    /// A region that cannot honestly loop, or a deck parked at its end.
    Refuse,
    /// Playing, playhead at/past the new end (a late OUT snapping backwards):
    /// restart inside the region at the folded offset rather than trusting the
    /// wrap-on-reach edge.
    Restart(f64),
    /// Playing, inside: pin the playhead on the audible position so the linear
    /// history can't fight the new fold.
    Reanchor(f64),
    /// Paused past the end: fold the parked offset in so a later resume starts
    /// deterministically inside the region.
    Park(f64),
    /// Paused, inside: just set it.
    Apply,
}

/// Decide what a `set_track_loop` does given the transport state and the audible
/// playhead (`planLoopSet` in `track.ts`). `playing` distinguishes the
/// playing/paused branches; `ended` parks refuse.
pub fn plan_loop_set(
    playing: bool,
    ended: bool,
    position: f64,
    start: u64,
    end: u64,
    duration_frames: u64,
) -> LoopSetPlan {
    if end > duration_frames || end.saturating_sub(start) < MIN_TRACK_LOOP_FRAMES {
        return LoopSetPlan::Refuse;
    }
    if ended {
        return LoopSetPlan::Refuse;
    }
    let region = LoopRegion { start, end };
    if playing {
        return if position >= end as f64 {
            LoopSetPlan::Restart(fold_into_loop(position, region))
        } else {
            LoopSetPlan::Reanchor(position)
        };
    }
    if position >= end as f64 {
        LoopSetPlan::Park(fold_into_loop(position, region))
    } else {
        LoopSetPlan::Apply
    }
}

/// A snapshot of a playback deck's transport, mirroring `getTrackStatus` in
/// `frontend/src/audio/engine.ts`. Positions are in frames (the native domain);
/// the caller converts to seconds where needed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackStatus {
    /// The audible playhead in frames, folded through an active loop.
    pub playhead: f64,
    /// Whether the transport is rolling.
    pub playing: bool,
    /// Track length in frames.
    pub duration_frames: u64,
    /// The base varispeed rate (M20); 1.0 = as recorded.
    pub rate: f64,
    /// Parked at the end with the playhead pinned (ADR-0013 explicit end).
    pub ended: bool,
    /// The active loop region, or `None` for linear playback.
    pub loop_region: Option<LoopRegion>,
    /// A clip is queued to take over at its switch frame (the loom's seam).
    pub queued: bool,
    /// Queued hand-overs since this track loaded; the webview adopts the queued
    /// clip's identity when it rises.
    pub swaps: u32,
}

/// A clip waiting to take over a playing deck at a frame (the loom's seamless
/// seam): its buffer, where to enter it, and the frame of the CURRENT track at
/// which it takes over. Not boxed: moving it out on the swap must not free on
/// the RT path.
struct QueuedTrack {
    samples: Vec<f32>,
    duration_frames: u64,
    start_frame: f64,
    switch_at: u64,
}

/// Keep whole interleaved frames only (a stray trailing mono sample would make
/// the last frame read its R from out of bounds); returns the frame count.
fn whole_frames(samples: &mut Vec<f32>) -> u64 {
    let frames = (samples.len() / SAMPLES_PER_FRAME) as u64;
    samples.truncate(frames as usize * SAMPLES_PER_FRAME);
    frames
}

/// A decoded track playing on a deck (ADR-0013). Holds the interleaved-stereo f32
/// buffer at [`crate::SAMPLE_RATE`] (the shell decodes + resamples in Phase 2;
/// this slice takes decoded 48 k f32), a fractional playhead, the transport
/// state, the varispeed rate, and an optional loop region.
///
/// All mutation is non-RT (`&mut`, like `set_eq`/`set_fx`). The RT path is
/// [`BufferSource::pop_frame`] alone: it only reads the buffer + linear-
/// interpolates + advances the playhead — no alloc, no lock, no syscall.
pub struct BufferSource {
    /// Interleaved stereo `[L, R, L, R, …]` at [`crate::SAMPLE_RATE`].
    samples: Vec<f32>,
    /// Length in frames (`samples.len() / 2`); cached so the RT path never
    /// divides.
    duration_frames: u64,
    /// The fractional playhead in frames. At rate 1.0 it lands on integers and
    /// the buffer reads verbatim.
    playhead: f64,
    /// Whether the transport is rolling. Paused outputs silence, playhead frozen.
    playing: bool,
    /// Parked at the end (ADR-0013): silence, playhead pinned at the end.
    ended: bool,
    /// Varispeed rate (ADR-0014); the playhead advances by this per output
    /// sample. Default 1.0.
    rate: f64,
    /// Pending phase-nudge backlog in frames (the platter bend, M20). Positive
    /// pushes the playhead ahead, negative drags it back; capped at
    /// [`MAX_PENDING_SLIP_FRAMES`]. Consumed as a per-sample rate bend in
    /// [`BufferSource::pop_frame`] — click-free, never a jump. Default 0.
    pending_slip: f64,
    /// The active loop region in frames (ADR-0015), or `None` for linear.
    loop_region: Option<LoopRegion>,
    /// The clip queued to take over at its switch frame, or `None`.
    next: Option<QueuedTrack>,
    /// The buffer the last swap replaced, parked so the RT path never frees it;
    /// the render thread drops it after each block ([`BufferSource::take_retired`]).
    retired: Option<Vec<f32>>,
    /// Queued hand-overs since load.
    swaps: u32,
}

impl BufferSource {
    /// Build a playback source from decoded interleaved-stereo f32 at 48 kHz.
    /// Starts paused at the top, rate 1.0, no loop — exactly the Web Audio
    /// `loadTrack` (`trackTransport = { paused, offset: 0 }`, `trackLoop = null`).
    ///
    /// Allocates (takes ownership of the buffer) — non-RT, like `create_deck`'s
    /// ring allocation. An odd-length input is truncated to whole frames.
    pub fn new(samples: Vec<f32>) -> Self {
        let mut samples = samples;
        let frames = whole_frames(&mut samples);
        BufferSource {
            samples,
            duration_frames: frames,
            playhead: 0.0,
            playing: false,
            ended: false,
            rate: 1.0,
            pending_slip: 0.0,
            loop_region: None,
            next: None,
            retired: None,
            swaps: 0,
        }
    }

    /// Queue `samples` (interleaved stereo at [`crate::SAMPLE_RATE`]) to take over
    /// when the playhead reaches `switch_at`, entering the clip at `start_frame`
    /// (clamped into it). A second queue replaces the first; an empty clip is
    /// refused. `seek`, `set_loop` and `clear_loop` keep the queue; a load
    /// discards it with the source. Non-RT (it may drop a replaced queue).
    pub fn queue(&mut self, samples: Vec<f32>, start_frame: f64, switch_at: u64) {
        let mut samples = samples;
        let frames = whole_frames(&mut samples);
        if frames == 0 {
            return;
        }
        self.next = Some(QueuedTrack {
            samples,
            duration_frames: frames,
            start_frame: clamp_offset(start_frame, frames),
            switch_at,
        });
    }

    /// Drop the queued clip, if any. Non-RT.
    pub fn clear_queued(&mut self) {
        self.next = None;
    }

    /// Hand the buffer the last swap replaced to the caller to drop off the RT
    /// path. `None` when nothing swapped since the last call.
    pub fn take_retired(&mut self) -> Option<Vec<f32>> {
        self.retired.take()
    }

    /// Start (or resume) playback. From the ended state it restarts at the top —
    /// the Web Audio `playTrack` rule (`offset = ended ? 0 : offset`). A no-op
    /// when already playing.
    pub fn play(&mut self) {
        if self.playing {
            return;
        }
        if self.ended {
            self.playhead = 0.0;
            self.ended = false;
        }
        self.playing = true;
    }

    /// Park the playhead where it is (`pauseTrack`). Pausing mid-loop parks inside
    /// the region because the playhead already folds there. Drops any pending
    /// phase bend — the platter gesture ends with the transport.
    pub fn pause(&mut self) {
        self.playing = false;
        self.pending_slip = 0.0;
    }

    /// Jump the playhead to `frames` (clamped into the track) and **exit any
    /// active loop** — the one rule (ADR-0015). Whether it was playing is
    /// preserved; landing exactly at the end leaves the deck rolling toward its
    /// natural end (it is not the `ended` park, which only the linear run sets).
    pub fn seek(&mut self, frames: f64) {
        self.loop_region = None;
        self.playhead = clamp_offset(frames, self.duration_frames);
        // A seek re-arms a parked-at-end deck: the playhead is now valid, so the
        // next render advances from here rather than staying parked.
        self.ended = false;
        // A jump invalidates any in-flight phase bend (it was relative to the old
        // position).
        self.pending_slip = 0.0;
    }

    /// Set the varispeed rate, clamped to the widest envelope (ADR-0014). Non-RT;
    /// the RT path reads it per sample. Mirrors `setTrackRate` holding the
    /// envelope at the boundary.
    pub fn set_rate(&mut self, rate: f64) {
        self.rate = clamp_rate(rate);
    }

    /// Slip the playhead by `frames` of phase via a stepped rate bend (the
    /// platter-drag metaphor, M20/ADR-0014) — the jog-while-playing nudge. Adds
    /// to a capped backlog ([`MAX_PENDING_SLIP_FRAMES`]) consumed click-free in
    /// [`pop_frame`](Self::pop_frame); positive pushes ahead, negative drags back.
    /// Playing only — a paused platter is a fine seek, not a bend (handled by the
    /// caller). Non-RT.
    pub fn nudge_phase(&mut self, frames: f64) {
        if !self.playing || !frames.is_finite() {
            return;
        }
        self.pending_slip =
            (self.pending_slip + frames).clamp(-MAX_PENDING_SLIP_FRAMES, MAX_PENDING_SLIP_FRAMES);
    }

    /// The extra per-sample advance for an in-progress phase bend, decrementing
    /// the backlog by what it consumes. Mirrors `bendPlan` in `track.ts`: the bend
    /// fraction ramps from [`NUDGE_BEND_FRACTION`] up to [`MAX_BEND_FRACTION`] as
    /// the backlog grows, so a spin catches up in bounded time. The bent advance
    /// `rate * (1 ± fraction)` stays positive (fraction ≤ 0.5), so the playhead
    /// only ever moves forward — a "back" nudge just advances slower, dropping
    /// phase. Click-free: a rate change, never a jump.
    fn consume_slip(&mut self) -> f64 {
        if self.pending_slip == 0.0 {
            return 0.0;
        }
        let direction = if self.pending_slip >= 0.0 { 1.0 } else { -1.0 };
        let slip_seconds = self.pending_slip.abs() / crate::SAMPLE_RATE as f64;
        let fraction = (slip_seconds / (self.rate * BEND_CATCH_UP_SECONDS))
            .clamp(NUDGE_BEND_FRACTION, MAX_BEND_FRACTION);
        let extra = direction * self.rate * fraction;
        // Never overshoot the remaining backlog (extra shares its sign).
        if extra.abs() >= self.pending_slip.abs() {
            let remaining = self.pending_slip;
            self.pending_slip = 0.0;
            remaining
        } else {
            self.pending_slip -= extra;
            extra
        }
    }

    /// Set the loop region (ADR-0015/0016), running the pure [`plan_loop_set`]
    /// decision. A refused region leaves the source untouched; restart/reanchor/
    /// park adjust the playhead deterministically so the playhead never sits
    /// outside an installed region.
    pub fn set_loop(&mut self, start: u64, end: u64) {
        let plan = plan_loop_set(
            self.playing,
            self.ended,
            self.playhead,
            start,
            end,
            self.duration_frames,
        );
        match plan {
            LoopSetPlan::Refuse => {}
            LoopSetPlan::Restart(offset) | LoopSetPlan::Reanchor(offset) | LoopSetPlan::Park(offset) => {
                self.playhead = offset;
                self.loop_region = Some(LoopRegion { start, end });
            }
            LoopSetPlan::Apply => {
                self.loop_region = Some(LoopRegion { start, end });
            }
        }
    }

    /// Clear the active loop (`clearTrackLoop`). Re-anchors the playhead on the
    /// folded position first, so from here it runs linear from the seam rather
    /// than jumping. A no-op with no loop active.
    pub fn clear_loop(&mut self) {
        if let Some(region) = self.loop_region {
            self.playhead = fold_into_loop(self.playhead, region);
            self.loop_region = None;
        }
    }

    /// A transport snapshot (`getTrackStatus`). The playhead folds through an
    /// active loop so every position consumer stays truthful inside it.
    pub fn status(&self) -> TrackStatus {
        TrackStatus {
            playhead: self.folded_playhead(),
            playing: self.playing,
            duration_frames: self.duration_frames,
            rate: self.rate,
            ended: self.ended,
            loop_region: self.loop_region,
            queued: self.next.is_some(),
            swaps: self.swaps,
        }
    }

    /// The audible playhead, folded through an active loop region.
    fn folded_playhead(&self) -> f64 {
        match self.loop_region {
            Some(region) => fold_into_loop(self.playhead, region),
            None => self.playhead,
        }
    }

    /// A style-sample window of the track (M15 on a Playback deck): `frames` of
    /// interleaved stereo ending at the audible playhead, or starting there when
    /// fewer than `min_frames` precede it (the top of the track). Clamped to the
    /// buffer; `None` when even that is shorter than `min_frames`. Non-RT (copies).
    pub fn sample_window(&self, frames: usize, min_frames: usize) -> Option<Vec<f32>> {
        let (start, end) =
            sample_window_bounds(self.folded_playhead() as usize, frames, min_frames, self.duration_frames as usize)?;
        Some(self.samples[start * SAMPLES_PER_FRAME..end * SAMPLES_PER_FRAME].to_vec())
    }

    /// Min/max overview of the buffer across both channels (`trackPeaks` in
    /// `track.ts`) — the static overview a decoded track can afford that the live
    /// stream cannot. Buckets cover the buffer evenly; a short final bucket still
    /// counts. Returns `(min, max)` of length `buckets`. Non-RT (allocates).
    pub fn peaks(&self, buckets: usize) -> (Vec<f32>, Vec<f32>) {
        let mut min = vec![0.0f32; buckets];
        let mut max = vec![0.0f32; buckets];
        let frames = self.duration_frames as usize;
        if frames == 0 || buckets == 0 {
            return (min, max);
        }
        for bucket in 0..buckets {
            let start = bucket * frames / buckets;
            // Each bucket spans at least one frame, so a bucket count above the
            // frame count still produces a sane (repeated-frame) overview rather
            // than an empty span.
            let end = ((bucket + 1) * frames / buckets).max(start + 1).min(frames);
            let mut lo = f32::INFINITY;
            let mut hi = f32::NEG_INFINITY;
            for f in start..end {
                let l = self.samples[2 * f];
                let r = self.samples[2 * f + 1];
                lo = lo.min(l).min(r);
                hi = hi.max(l).max(r);
            }
            min[bucket] = if lo.is_finite() { lo } else { 0.0 };
            max[bucket] = if hi.is_finite() { hi } else { 0.0 };
        }
        (min, max)
    }

    /// **The RT path.** Produce one output stereo frame and advance the playhead
    /// by the varispeed rate, folding through an active loop. Paused or ended:
    /// silence, playhead frozen. Reads the buffer + linear-interpolates only — no
    /// alloc, no lock, no syscall.
    ///
    /// A queued clip takes over when the advance reaches its `switch_at`: this
    /// frame is still the old track's, the next is the clip's `start_frame` plus
    /// the overshoot. The check precedes the loop fold and the end park, so a
    /// seam at the loop end or the exact clip end swaps instead of wrapping or
    /// going silent. The swap only moves buffers; the old one waits in `retired`.
    ///
    /// At rate 1.0 the playhead lands on integers, the interpolation weight is 0,
    /// and the output is the buffer sample verbatim.
    #[inline]
    pub fn pop_frame(&mut self) -> (f32, f32) {
        if !self.playing || self.ended || self.duration_frames == 0 {
            return (0.0, 0.0);
        }

        let frame = self.sample_at(self.playhead);

        // Advance by the rate plus any in-flight phase bend, then fold (loop) or
        // detect the natural end. The bend only changes how fast we move forward.
        let next = self.playhead + self.rate + self.consume_slip();
        if self.next.as_ref().is_some_and(|queued| next >= queued.switch_at as f64) {
            if let Some(queued) = self.next.take() {
                // `retired` is always empty here: commands (and so queues) land
                // between blocks, and the render thread drops it after each one.
                self.retired = Some(std::mem::replace(&mut self.samples, queued.samples));
                self.duration_frames = queued.duration_frames;
                self.playhead = clamp_offset(
                    queued.start_frame + (next - queued.switch_at as f64),
                    queued.duration_frames,
                );
                self.loop_region = None;
                self.pending_slip = 0.0;
                self.swaps += 1;
            }
            return frame;
        }
        match self.loop_region {
            Some(region) => {
                self.playhead = fold_into_loop(next, region);
            }
            None => {
                if next >= self.duration_frames as f64 {
                    // Natural end (ADR-0013): explicit silence, playhead pinned.
                    self.playhead = self.duration_frames as f64;
                    self.ended = true;
                    self.playing = false;
                } else {
                    self.playhead = next;
                }
            }
        }

        frame
    }

    /// Linear-interpolate the stereo frame at a fractional playhead `p`
    /// (`AudioBufferSourceNode.playbackRate` semantics): the two samples
    /// bracketing `p` weighted by its fraction. Clamped to the buffer bounds so a
    /// playhead exactly at the last frame (rate-1.0 wrap edges) reads cleanly.
    #[inline]
    fn sample_at(&self, p: f64) -> (f32, f32) {
        let i = p.floor();
        let frac = (p - i) as f32;
        let i0 = i as usize;
        let last = self.duration_frames as usize - 1;

        let (l0, r0) = self.frame(i0.min(last));
        if frac == 0.0 {
            // Integer playhead (rate 1.0, or a loop start): verbatim, bit-exact.
            return (l0, r0);
        }
        let (l1, r1) = self.frame((i0 + 1).min(last));
        (l0 + (l1 - l0) * frac, r0 + (r1 - r0) * frac)
    }

    /// The raw interleaved stereo frame at integer index `i` (caller clamps `i`).
    #[inline]
    fn frame(&self, i: usize) -> (f32, f32) {
        (self.samples[2 * i], self.samples[2 * i + 1])
    }
}

/// The `[start, end)` frame bounds of a style-sample window (see
/// [`BufferSource::sample_window`]): `frames` ending at `playhead`, or starting at
/// it when fewer than `min_frames` precede it; clamped to `duration`. `None` when
/// the clamped window is shorter than `min_frames`.
pub fn sample_window_bounds(
    playhead: usize,
    frames: usize,
    min_frames: usize,
    duration: usize,
) -> Option<(usize, usize)> {
    let playhead = playhead.min(duration);
    let (start, end) = if playhead >= min_frames {
        (playhead.saturating_sub(frames), playhead)
    } else {
        (playhead, playhead.saturating_add(frames).min(duration))
    };
    (end - start >= min_frames).then_some((start, end))
}

/// A deck's active source (ADR-0013): the live Magenta stream's ring consumer
/// (Slices 1–3) or one decoded track. Loading decides the mode — there is no
/// toggle.
pub(crate) enum DeckSource {
    /// The per-deck SPSC ring consumer (the live stream).
    Realtime(crate::ring::RingConsumer),
    /// One decoded track playing as a buffer source.
    Playback(BufferSource),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The style-sample window ends at the playhead, starts there near the top of
    /// the track, clamps to the buffer, and refuses a window under the floor.
    #[test]
    fn sample_window_bounds_follow_the_playhead() {
        // Mid-track: the 10 frames ending at the playhead.
        assert_eq!(sample_window_bounds(50, 10, 3, 100), Some((40, 50)));
        // Fewer than `frames` but at least the floor precede it: take what is there.
        assert_eq!(sample_window_bounds(5, 10, 3, 100), Some((0, 5)));
        // Under the floor precede it: the window starting at the playhead instead.
        assert_eq!(sample_window_bounds(2, 10, 3, 100), Some((2, 12)));
        // ...clamped to the track end.
        assert_eq!(sample_window_bounds(0, 10, 3, 6), Some((0, 6)));
        // Too short either way: refused.
        assert_eq!(sample_window_bounds(1, 10, 3, 3), None);
        // A playhead past the end clamps to it.
        assert_eq!(sample_window_bounds(500, 10, 3, 100), Some((90, 100)));
        // The slice is the exact frames in that window.
        let mut src = BufferSource::new(ramp_buffer(100));
        src.seek(50.0);
        let window = src.sample_window(10, 3).expect("a window");
        assert_eq!(window.len(), 20);
        assert_eq!((window[0], window[19]), (40.0, -49.0));
    }

    /// Build an interleaved-stereo buffer whose L channel is a ramp `0, 1, 2, …`
    /// and R is its negation, so a frame's identity is unambiguous (frame `i` is
    /// `(i, -i)`). Sample-accurate readback can then assert the exact source frame.
    fn ramp_buffer(frames: usize) -> Vec<f32> {
        let mut buf = vec![0.0f32; frames * SAMPLES_PER_FRAME];
        for f in 0..frames {
            buf[2 * f] = f as f32;
            buf[2 * f + 1] = -(f as f32);
        }
        buf
    }

    /// A pure sine in both channels at `freq` (cycles/sample as a fraction of the
    /// rate), for the varispeed pitch-doubling check.
    fn sine_buffer(frames: usize, cycles_per_sample: f64) -> Vec<f32> {
        let mut buf = vec![0.0f32; frames * SAMPLES_PER_FRAME];
        for f in 0..frames {
            let s = (2.0 * std::f64::consts::PI * cycles_per_sample * f as f64).sin() as f32;
            buf[2 * f] = s;
            buf[2 * f + 1] = s;
        }
        buf
    }

    /// (T1) Rate 1.0 plays the buffer back verbatim: the playhead lands on
    /// integers, the interpolation weight is 0, and every output frame is the
    /// source frame bit-for-bit.
    #[test]
    fn rate_one_plays_buffer_verbatim() {
        let mut src = BufferSource::new(ramp_buffer(64));
        src.play();
        for f in 0..64 {
            let (l, r) = src.pop_frame();
            assert_eq!(l.to_bits(), (f as f32).to_bits(), "frame {f} L verbatim");
            assert_eq!(r.to_bits(), (-(f as f32)).to_bits(), "frame {f} R verbatim");
        }
        // The 64th advance crosses the end → parked, silent.
        let (l, r) = src.pop_frame();
        assert_eq!((l, r), (0.0, 0.0), "past the end is silence");
        assert!(src.status().ended, "linear run parks at the end");
    }

    /// (T2) Varispeed is plain resampling (ADR-0014): the output is linear-
    /// interpolated between the two samples bracketing the fractional playhead, so
    /// pitch shifts with rate. Rate 2.0 consumes two input samples per output
    /// (half-speed time, double pitch); rate 0.5 consumes half an input per output
    /// (double time... — i.e. half-speed playback re-reads each input ~twice).
    ///
    /// The public `set_rate` holds the ±8 % varispeed envelope (M20/SYNC policy,
    /// tested in `clamp_rate_holds_the_envelope`), which excludes 2.0/0.5; the
    /// resampling MECHANISM under test is the pure interpolation + advance-by-rate,
    /// driven here at the raw step it would see if the envelope were wider.
    #[test]
    fn varispeed_is_plain_resampling() {
        // Rate 2.0 on a ramp: the playhead lands on frames 0, 2, 4, … (two input
        // samples consumed per output), read verbatim at the integer steps.
        let ramp = BufferSource::new(ramp_buffer(64));
        for k in 0..30 {
            let p = (2 * k) as f64;
            let (l, _r) = ramp.sample_at(p);
            assert_eq!(l.to_bits(), (p as f32).to_bits(), "rate-2 step {k} reads frame {p}");
        }
        // Rate 0.5 on a ramp: the playhead lands on 0, 0.5, 1.0, 1.5, … and the
        // half-integer steps interpolate the midpoint (each input re-read ~twice).
        for k in 0..30 {
            let p = k as f64 * 0.5;
            let (l, _r) = ramp.sample_at(p);
            assert!((l as f64 - p).abs() < 1e-5, "rate-0.5 step {k} interpolates frame {p}, got {l}");
        }

        // Pitch shifts with rate: a sine sampled at rate 2.0 has half the period
        // of rate 1.0 — its first downward zero crossing comes at half the output
        // index.
        let cps = 1.0 / 16.0; // 16-sample period at rate 1.0
        let tone = BufferSource::new(sine_buffer(128, cps));
        let cross_rate1 = first_down_crossing(&tone, 1.0);
        let cross_rate2 = first_down_crossing(&tone, 2.0);
        assert!(
            (cross_rate2 - cross_rate1 / 2.0).abs() < 1.0,
            "rate 2.0 should halve the period: r1 cross {cross_rate1}, r2 cross {cross_rate2}"
        );
    }

    /// (T2b) Within-envelope varispeed runs end-to-end through `pop_frame`: at the
    /// envelope edge (+8 %) the playhead advances by exactly that rate per output.
    #[test]
    fn in_envelope_varispeed_advances_through_pop_frame() {
        let mut src = BufferSource::new(ramp_buffer(10_000));
        src.set_rate(1.0 + TRACK_RATE_RANGE);
        src.play();
        for _ in 0..1000 {
            src.pop_frame();
        }
        let expected = 1000.0 * (1.0 + TRACK_RATE_RANGE);
        assert!(
            (src.status().playhead - expected).abs() < 1e-6,
            "playhead should advance at the clamped rate: got {}, want {expected}",
            src.status().playhead
        );
    }

    /// Helper: the output sample index where a tone first crosses from positive to
    /// negative, sampled at `rate` (linear interpolation), as a fractional index.
    fn first_down_crossing(src: &BufferSource, rate: f64) -> f64 {
        let mut prev = src.sample_at(0.0).0;
        let mut p = rate;
        let mut out_index = 1.0;
        while p < 60.0 {
            let s = src.sample_at(p).0;
            if prev > 0.0 && s <= 0.0 {
                return out_index;
            }
            prev = s;
            p += rate;
            out_index += 1.0;
        }
        f64::INFINITY
    }

    /// (T3) A seek sets the playhead and EXITS an active loop — the one rule
    /// (ADR-0015).
    #[test]
    fn seek_sets_playhead_and_exits_loop() {
        let mut src = BufferSource::new(ramp_buffer(20_000));
        src.play();
        // Region length must clear MIN_TRACK_LOOP_FRAMES (0.05 s ≈ 2400 frames).
        src.set_loop(3_000, 8_000);
        assert!(src.status().loop_region.is_some(), "loop installed");
        src.seek(15_000.0);
        assert_eq!(src.status().loop_region, None, "any seek exits the loop");
        assert_eq!(src.status().playhead, 15_000.0, "seek sets the playhead");
    }

    /// (T4) The loop folds the playhead through `[start, end)` over multiple wraps
    /// with a continuous seam: at the end frame the next output is the loop's
    /// start frame, no gap, no double.
    #[test]
    fn loop_folds_with_continuous_seam() {
        // A region of LEN frames starting at START, both clearing the honest
        // minimum, looped at rate 1.0 so the L ramp cycles START..START+LEN.
        const START: i64 = 3_000;
        const LEN: i64 = 2_500; // > MIN_TRACK_LOOP_FRAMES (≈ 2400)
        let mut src = BufferSource::new(ramp_buffer(20_000));
        src.play();
        // Seek INSIDE the region first (seek exits any loop), THEN set the loop so
        // the playhead is inside it.
        src.seek(START as f64);
        src.set_loop(START as u64, (START + LEN) as u64);
        // Read two-and-a-bit full laps; the L channel must cycle START..START+LEN
        // with no gap or repeat at the seam.
        let reads = (LEN * 2 + 17) as usize;
        let mut seen = Vec::with_capacity(reads);
        for _ in 0..reads {
            let (l, _r) = src.pop_frame();
            seen.push(l as i64);
        }
        let expected: Vec<i64> = (0..reads as i64).map(|k| START + (k % LEN)).collect();
        assert_eq!(seen, expected, "the loop must fold continuously across the seam");
        // The status playhead stays inside the region across the wraps.
        let p = src.status().playhead;
        assert!((START as f64..(START + LEN) as f64).contains(&p), "folded playhead stays in region, got {p}");
    }

    /// (T4b) A fractional rate inside a loop folds the FRACTIONAL playhead through
    /// the seam (no integer-only assumption): the playhead never escapes
    /// `[start, end)`.
    #[test]
    fn loop_folds_fractional_playhead() {
        let mut src = BufferSource::new(ramp_buffer(20_000));
        src.set_rate(1.0 + TRACK_RATE_RANGE); // +50 %, a non-integer step
        src.play();
        src.seek(2_000.0);
        src.set_loop(2_000, 5_000);
        for _ in 0..50_000 {
            src.pop_frame();
            let p = src.status().playhead;
            assert!((2_000.0..5_000.0).contains(&p), "fractional fold escaped: {p}");
        }
    }

    /// (T5) Pause freezes the playhead and outputs silence; resume continues from
    /// the frozen spot.
    #[test]
    fn pause_freezes_playhead_and_silences() {
        let mut src = BufferSource::new(ramp_buffer(64));
        src.play();
        src.pop_frame();
        src.pop_frame(); // playhead now at 2
        let frozen = src.status().playhead;
        src.pause();
        for _ in 0..10 {
            assert_eq!(src.pop_frame(), (0.0, 0.0), "paused outputs silence");
        }
        assert_eq!(src.status().playhead, frozen, "paused freezes the playhead");
        src.play();
        let (l, _r) = src.pop_frame();
        assert_eq!(l, frozen as f32, "resume continues from the frozen frame");
    }

    /// (T6) Peaks return a sane overview: one bucket spans the whole buffer and
    /// captures the global min/max; bucket counts divide the buffer evenly and
    /// stay in range.
    #[test]
    fn peaks_give_a_sane_overview() {
        // A ramp 0..64 on L, negated on R → global min -63 (R at frame 63), max 63.
        let src = BufferSource::new(ramp_buffer(64));
        let (min1, max1) = src.peaks(1);
        assert_eq!(min1.len(), 1);
        assert_eq!(max1.len(), 1);
        assert_eq!(min1[0], -63.0, "one bucket sees the global min");
        assert_eq!(max1[0], 63.0, "one bucket sees the global max");

        // Eight buckets: monotone ramp → each bucket's max rises, min falls
        // (R is the negated ramp), and the union covers the whole range.
        let (min8, max8) = src.peaks(8);
        assert_eq!(max8.len(), 8);
        for w in max8.windows(2) {
            assert!(w[1] >= w[0], "ramp bucket maxima must be non-decreasing");
        }
        assert_eq!(*max8.last().unwrap(), 63.0, "last bucket holds the peak");
        assert_eq!(min8[0], -7.0, "first bucket's min is the negated ramp tail of frames 0..8");

        // Empty buffer: zero-filled, no panic.
        let empty = BufferSource::new(Vec::new());
        let (mn, mx) = empty.peaks(4);
        assert_eq!(mn, vec![0.0; 4]);
        assert_eq!(mx, vec![0.0; 4]);
    }

    /// (T7) `plan_loop_set` covers every spec edge (ADR-0015): refuse a too-short
    /// or out-of-range region and an ended deck; restart when playing past the
    /// end; reanchor when playing inside; park when paused past the end; apply
    /// when paused inside.
    #[test]
    fn plan_loop_set_covers_the_spec_edges() {
        let dur = 20_000u64;
        // A valid region: 3000..6000 (3000 frames > MIN_TRACK_LOOP_FRAMES ≈ 2400).
        let (s, e) = (3_000u64, 6_000u64);
        // Too short (< MIN_TRACK_LOOP_FRAMES) → refuse.
        assert_eq!(plan_loop_set(true, false, 0.0, 3_000, 3_001, dur), LoopSetPlan::Refuse);
        // End past duration → refuse.
        assert_eq!(plan_loop_set(true, false, 0.0, s, dur + 1, dur), LoopSetPlan::Refuse);
        // Ended deck → refuse even for a valid region.
        assert_eq!(plan_loop_set(false, true, 0.0, s, e, dur), LoopSetPlan::Refuse);
        // Playing, inside → reanchor on the audible position.
        assert_eq!(
            plan_loop_set(true, false, 4_500.0, s, e, dur),
            LoopSetPlan::Reanchor(4_500.0)
        );
        // Playing, past the end (late OUT) → restart at the folded offset.
        match plan_loop_set(true, false, 6_500.0, s, e, dur) {
            LoopSetPlan::Restart(off) => assert!((s as f64..e as f64).contains(&off)),
            other => panic!("expected restart, got {other:?}"),
        }
        // Paused, inside → apply.
        assert_eq!(plan_loop_set(false, false, 4_500.0, s, e, dur), LoopSetPlan::Apply);
        // Paused, past the end → park at the folded offset.
        match plan_loop_set(false, false, 6_500.0, s, e, dur) {
            LoopSetPlan::Park(off) => assert!((s as f64..e as f64).contains(&off)),
            other => panic!("expected park, got {other:?}"),
        }
    }

    /// (T7b) A late-OUT `set_loop` (playhead already past the new end) RESTARTS the
    /// source inside the region — the deterministic rule, not the wrap-on-reach
    /// edge (ADR-0015).
    #[test]
    fn set_loop_past_end_restarts_inside_region() {
        let mut src = BufferSource::new(ramp_buffer(20_000));
        src.play();
        src.seek(9_000.0); // playhead past the region we are about to set
        src.set_loop(3_000, 6_000);
        let p = src.status().playhead;
        assert!((3_000.0..6_000.0).contains(&p), "late OUT restarts inside the region, got {p}");
        assert!(src.status().loop_region.is_some(), "the loop is installed");
    }

    /// (T8) `clear_loop` re-anchors on the folded position, then runs linear: the
    /// next output is the folded frame, advancing forward out of the old region.
    #[test]
    fn clear_loop_reanchors_on_the_seam() {
        let mut src = BufferSource::new(ramp_buffer(20_000));
        src.play();
        src.seek(3_000.0);
        src.set_loop(3_000, 6_000);
        for _ in 0..7_000 {
            src.pop_frame(); // wrap twice over; playhead lands inside the region
        }
        let folded = src.status().playhead;
        assert!((3_000.0..6_000.0).contains(&folded), "playhead inside the region before clear");
        src.clear_loop();
        assert_eq!(src.status().loop_region, None);
        assert_eq!(src.status().playhead, folded, "clear re-anchors on the folded seam");
        // From here it runs linear (the playhead climbs past the old region end).
        for _ in 0..4_000 {
            src.pop_frame();
        }
        assert!(src.status().playhead > 6_000.0, "after clear the playhead runs linear past the region");
    }

    /// (T9) Status reports correctly across loads / transport: a fresh source is
    /// paused at 0, rate 1.0; play/seek/rate are reflected; duration is the frame
    /// count.
    #[test]
    fn status_reports_transport_state() {
        let mut src = BufferSource::new(ramp_buffer(500));
        let s = src.status();
        assert_eq!(s.playhead, 0.0);
        assert!(!s.playing);
        assert!(!s.ended);
        assert_eq!(s.duration_frames, 500);
        assert_eq!(s.rate, 1.0);
        assert_eq!(s.loop_region, None);

        src.play();
        src.set_rate(1.05);
        for _ in 0..50 {
            src.pop_frame();
        }
        let s = src.status();
        assert!(s.playing);
        assert!((s.rate - 1.05).abs() < 1e-9, "rate reflected");
        assert!((s.playhead - 52.5).abs() < 0.5, "playhead advanced at the rate, got {}", s.playhead);
    }

    /// (T10) `clamp_rate` holds the outer envelope and falls back to 1.0 on garbage.
    #[test]
    fn clamp_rate_holds_the_envelope() {
        assert_eq!(clamp_rate(1.0), 1.0);
        assert_eq!(clamp_rate(2.0), 1.0 + TRACK_RATE_RANGE);
        assert_eq!(clamp_rate(0.0), 1.0);
        assert_eq!(clamp_rate(-1.0), 1.0);
        assert_eq!(clamp_rate(f64::NAN), 1.0);
        assert_eq!(clamp_rate(0.5), 1.0 - TRACK_RATE_RANGE);
    }

    /// (T11) Phase nudge (the jog-while-playing platter bend, M20): slips the
    /// playhead by the requested amount via a rate bend, NEVER a jump. A forward
    /// nudge leaves the playhead exactly that far ahead of an un-nudged twin and a
    /// backward nudge symmetrically behind; the per-sample advance stays bounded
    /// (no click) and the backlog drains.
    #[test]
    fn phase_nudge_slips_the_playhead_without_a_jump() {
        for &slip in &[480.0f64, -480.0] {
            let mut base = BufferSource::new(ramp_buffer(60_000));
            let mut bent = BufferSource::new(ramp_buffer(60_000));
            base.play();
            bent.play();
            for _ in 0..1_000 {
                base.pop_frame();
                bent.pop_frame();
            }

            bent.nudge_phase(slip);

            // Settle the bend (the gentlest 5 % bend clears 480 frames in ~9600
            // samples); track the largest per-sample step to prove no jump.
            let mut prev = bent.playhead;
            let mut max_step = 0.0f64;
            for _ in 0..20_000 {
                base.pop_frame();
                bent.pop_frame();
                max_step = max_step.max((bent.playhead - prev).abs());
                prev = bent.playhead;
            }

            // The bent twin sits `slip` frames from the un-nudged one.
            assert!(
                (bent.playhead - base.playhead - slip).abs() < 1e-3,
                "slip {slip}: expected a {slip}-frame offset, got {}",
                bent.playhead - base.playhead
            );
            // No click: the playhead only ever advances forward, by at most the
            // steepest-bend rate (1 + MAX_BEND_FRACTION) — never a position jump.
            assert!(
                max_step > 0.0 && max_step <= 1.0 + MAX_BEND_FRACTION + 1e-9,
                "slip {slip}: a bend must glide, not jump — max step {max_step}"
            );
            // The backlog is fully consumed once settled.
            assert_eq!(bent.pending_slip, 0.0, "slip {slip}: backlog drained");
        }
    }

    /// (T12) A paused deck banks no phase slip — the caller routes a paused jog to
    /// a fine seek, so `nudge_phase` ignores it and playback stays verbatim.
    #[test]
    fn phase_nudge_is_a_no_op_while_paused() {
        let mut src = BufferSource::new(ramp_buffer(1_000));
        src.nudge_phase(480.0);
        assert_eq!(src.pending_slip, 0.0, "a paused nudge banks nothing");
        src.play();
        for f in 0..200 {
            let (l, _r) = src.pop_frame();
            assert_eq!(l.to_bits(), (f as f32).to_bits(), "frame {f} verbatim — no banked slip");
        }
    }

    /// (T13) The backlog caps at one second (`MAX_PENDING_SLIP_FRAMES`): a platter
    /// is a phase tool, not a seek — a hard spin does not pile up minutes of bend.
    #[test]
    fn phase_nudge_caps_the_backlog() {
        let mut src = BufferSource::new(ramp_buffer(60_000));
        src.play();
        for _ in 0..100 {
            src.nudge_phase(48_000.0); // each well past the 1 s cap
        }
        assert_eq!(src.pending_slip, MAX_PENDING_SLIP_FRAMES, "backlog caps at 1 s");
    }

    /// (T14) A seek cancels an in-flight bend: the jump invalidates a slip that was
    /// relative to the old position, so playback resumes verbatim from the target.
    #[test]
    fn seek_cancels_a_pending_phase_bend() {
        let mut src = BufferSource::new(ramp_buffer(60_000));
        src.play();
        for _ in 0..500 {
            src.pop_frame();
        }
        src.nudge_phase(480.0);
        assert!(src.pending_slip > 0.0, "the nudge banked a slip");
        src.seek(500.0);
        assert_eq!(src.pending_slip, 0.0, "a seek cancels the in-flight bend");
        let first = src.pop_frame().0 as f64;
        let second = src.pop_frame().0 as f64;
        assert!((first - 500.0).abs() < 1e-4, "seek landed at 500, got {first}");
        assert!(
            (second - first - 1.0).abs() < 1e-4,
            "verbatim advance after seek, got step {}",
            second - first
        );
    }

    // --- The queued swap (the loom's seamless seam, architecture §3) ---

    /// A ramp like [`ramp_buffer`] whose L starts at `first`, so a queued clip's
    /// frames can never be mistaken for the current track's.
    fn ramp_from(frames: usize, first: f32) -> Vec<f32> {
        let mut buf = ramp_buffer(frames);
        for f in 0..frames {
            buf[2 * f] += first;
            buf[2 * f + 1] -= first;
        }
        buf
    }

    /// Counts this thread's heap allocations and frees, so a test can prove a
    /// stretch of RT code never touches the allocator (thread-local: tests on
    /// other threads do not disturb the count).
    struct CountingAlloc;

    thread_local! {
        static ALLOCATOR_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    unsafe impl std::alloc::GlobalAlloc for CountingAlloc {
        unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
            let _ = ALLOCATOR_CALLS.try_with(|calls| calls.set(calls.get() + 1));
            unsafe { std::alloc::System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
            let _ = ALLOCATOR_CALLS.try_with(|calls| calls.set(calls.get() + 1));
            unsafe { std::alloc::System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static COUNTING_ALLOC: CountingAlloc = CountingAlloc;

    fn allocator_calls() -> usize {
        ALLOCATOR_CALLS.with(|calls| calls.get())
    }

    /// (T15, R1) The hand-over is sample-continuous: every old frame up to
    /// `switch_at`, then the clip's `start_frame` on the very next output — no
    /// zero frame, no repeat — the advance continues inside the clip, and the
    /// swap never touches the allocator.
    #[test]
    fn queued_clip_takes_over_at_switch_at() {
        let mut src = BufferSource::new(ramp_buffer(1_000));
        src.queue(ramp_from(2_000, 5_000.0), 100.0, 600);
        src.play();
        src.seek(590.0);
        let before = allocator_calls();
        let mut seen = [0.0f32; 20];
        for frame in seen.iter_mut() {
            *frame = src.pop_frame().0;
        }
        assert_eq!(allocator_calls(), before, "the swap is a move: no alloc or free on the RT path");
        let expected: Vec<f32> =
            (590..600).map(|f| f as f32).chain((100..110).map(|f| 5_000.0 + f as f32)).collect();
        assert_eq!(seen.to_vec(), expected, "old frames up to switch_at, then the clip from start_frame");
        let s = src.status();
        assert_eq!(s.duration_frames, 2_000, "the clip's length takes over");
        assert_eq!(s.playhead, 110.0);
        assert!(s.playing && !s.ended && !s.queued);
        assert_eq!(s.swaps, 1);
    }

    /// (T16) At a fractional rate the advance past `switch_at` carries into the
    /// clip, and the next frame interpolates the NEW buffer there.
    #[test]
    fn queued_swap_carries_the_overshoot() {
        let mut src = BufferSource::new(ramp_buffer(1_000));
        src.set_rate(1.5);
        src.queue(ramp_from(1_000, 5_000.0), 200.0, 10);
        src.play();
        // Reads at 0, 1.5, … 9.0; the seventh advance lands on 10.5.
        for _ in 0..7 {
            src.pop_frame();
        }
        assert_eq!(src.status().playhead, 200.5, "0.5 frames past switch_at carry into the clip");
        let (l, _r) = src.pop_frame();
        assert!((l - 5_200.5).abs() < 1e-3, "the next frame reads the new buffer, got {l}");
    }

    /// (T17) A seam at the clip's exact end swaps instead of parking silent.
    #[test]
    fn switch_at_the_clip_end_swaps_instead_of_parking() {
        let mut src = BufferSource::new(ramp_buffer(100));
        src.queue(ramp_from(500, 5_000.0), 0.0, 100);
        src.play();
        src.seek(90.0);
        for f in 90..100 {
            assert_eq!(src.pop_frame().0, f as f32);
        }
        assert_eq!(src.pop_frame().0, 5_000.0, "the clip's first frame follows the old last frame");
        let s = src.status();
        assert!(s.playing && !s.ended, "the deck never parked");
        assert_eq!(s.swaps, 1);
    }

    /// (T18) A loop ending at `switch_at` (the hold under a tie) hands over at
    /// its end instead of folding back, and the loop goes with the old track.
    #[test]
    fn swap_wins_over_the_loop_fold() {
        let mut src = BufferSource::new(ramp_buffer(20_000));
        src.play();
        src.seek(3_000.0);
        src.set_loop(3_000, 6_000);
        src.queue(ramp_from(1_000, 50_000.0), 0.0, 6_000);
        for f in 3_000..6_000 {
            assert_eq!(src.pop_frame().0, f as f32);
        }
        assert_eq!(src.pop_frame().0, 50_000.0, "the swap, not the fold back to 3000");
        assert_eq!(src.status().loop_region, None, "the loop left with the old track");
    }

    /// (T19) A paused deck never swaps, even with its playhead past `switch_at`;
    /// it hands over on the first advance after play.
    #[test]
    fn a_paused_deck_does_not_swap() {
        let mut src = BufferSource::new(ramp_buffer(1_000));
        src.queue(ramp_from(1_000, 5_000.0), 0.0, 10);
        src.seek(20.0);
        for _ in 0..100 {
            assert_eq!(src.pop_frame(), (0.0, 0.0), "paused is silent");
        }
        let s = src.status();
        assert!(s.queued, "the queue waits");
        assert_eq!(s.swaps, 0);
        src.play();
        src.pop_frame();
        assert_eq!(src.status().swaps, 1, "the first advance after play swaps");
    }

    /// (T20) A seek keeps the queue; past `switch_at` the swap fires on the next
    /// frame with the overshoot carried into the clip.
    #[test]
    fn a_seek_past_switch_at_swaps_on_the_next_frame() {
        let mut src = BufferSource::new(ramp_buffer(1_000));
        src.play();
        src.queue(ramp_from(1_000, 5_000.0), 100.0, 500);
        src.seek(800.0);
        assert!(src.status().queued, "a seek keeps the queue");
        assert_eq!(src.pop_frame().0, 800.0, "the frame at the seek target still sounds");
        assert_eq!(src.status().swaps, 1);
        assert_eq!(src.pop_frame().0, 5_401.0, "start_frame plus the 301-frame overshoot");
    }

    /// (T21) A second queue replaces the first: only the latest clip and seam
    /// count.
    #[test]
    fn a_second_queue_replaces_the_first() {
        let mut src = BufferSource::new(ramp_buffer(1_000));
        src.queue(ramp_from(1_000, 5_000.0), 0.0, 20);
        src.queue(ramp_from(1_000, 9_000.0), 0.0, 50);
        src.play();
        for f in 0..50 {
            assert_eq!(src.pop_frame().0, f as f32, "the replaced seam at 20 never fires");
        }
        assert_eq!(src.pop_frame().0, 9_000.0, "the second clip takes over at its own seam");
        assert_eq!(src.status().swaps, 1);
    }

    /// (T22) An empty clip is refused (the previous queue stands) and a cleared
    /// queue never fires.
    #[test]
    fn empty_and_cleared_queues_never_swap() {
        let mut src = BufferSource::new(ramp_buffer(1_000));
        src.queue(ramp_from(1_000, 5_000.0), 0.0, 10);
        src.queue(Vec::new(), 0.0, 5);
        src.play();
        for _ in 0..11 {
            src.pop_frame();
        }
        assert_eq!(src.pop_frame().0, 5_001.0, "the empty clip left the first queue in place");

        src.queue(ramp_from(1_000, 9_000.0), 0.0, 20);
        src.clear_queued();
        assert!(!src.status().queued);
        for _ in 0..100 {
            assert!(src.pop_frame().0 < 9_000.0, "a cleared queue never takes over");
        }
        assert_eq!(src.status().swaps, 1);
    }

    /// (T23) The replaced buffer waits in `retired` until taken (the render
    /// thread drops it), and `swaps` counts each hand-over exactly once.
    #[test]
    fn swaps_count_hand_overs_and_retire_the_old_buffer() {
        let mut src = BufferSource::new(ramp_buffer(100));
        assert!(src.take_retired().is_none(), "nothing retired before a swap");
        src.queue(ramp_from(300, 5_000.0), 0.0, 10);
        src.play();
        for _ in 0..50 {
            src.pop_frame();
        }
        assert_eq!(src.status().swaps, 1, "frames after the swap do not count again");
        assert_eq!(src.take_retired().map(|buf| buf.len()), Some(200), "the old 100-frame buffer");
        assert!(src.take_retired().is_none(), "take empties it");

        src.queue(ramp_from(300, 9_000.0), 0.0, 60);
        for _ in 0..20 {
            src.pop_frame();
        }
        assert_eq!(src.status().swaps, 2, "the second hand-over counts once");
        assert_eq!(src.take_retired().map(|buf| buf.len()), Some(600), "the first clip retires in turn");
    }
}
