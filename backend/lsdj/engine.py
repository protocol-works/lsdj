"""DeckEngine: the only module that talks to magenta_rt.

The upstream API is young and may shift (see docs/spike-mrt2.md for the
measured facts this wrapper relies on); everything else in the backend
depends on this interface instead of magenta_rt directly.
"""

import importlib.metadata
import math

import numpy as np

SAMPLE_RATE = 48_000
CHANNELS = 2
FRAME_SECONDS = 0.04
FRAMES_PER_CHUNK = 25
CHUNK_SECONDS = FRAMES_PER_CHUNK * FRAME_SECONDS

# The ADR-0023 performance knob (issue #48): a deck whose performance surface
# is armed shrinks its chunk toward ~5 frames (200 ms) so steering lands at
# playable latency; disarmed decks keep the 1 s default. Bounded to the
# default — a larger chunk would only add latency.
MIN_CHUNK_FRAMES = 1
MAX_CHUNK_FRAMES = FRAMES_PER_CHUNK

# Note conditioning (ADR-0023): one slot per MIDI pitch, each holding a wire
# state (docs/spike-mrt2.md): -1 masked, 0 off, 1 sustain, 2 onset, 3 on with
# the model deciding attack vs continuation.
NOTE_SLOTS = 128
NOTE_STATES = frozenset((-1, 0, 1, 2, 3))
# Onset (2, "first time") is a one-chunk event: a held press decays to sustain
# (1) on the next chunk, or the model re-attacks the note every chunk. The two
# states the decay rule bridges (issue #46/#48).
NOTE_ONSET = 2
NOTE_SUSTAIN = 1

# Drum conditioning strength (issue #50): the `cfg_drums` classifier-free
# guidance scale, a float the model accepts in [-1.0, 7.0]. The measured useful
# range is ~3-5 (docs/spike-mrt2.md) — the library default of 1.0 barely bites
# on a hot stream, and very high values drift out of distribution.
MIN_DRUM_CFG = -1.0
MAX_DRUM_CFG = 7.0

# Live generation params (issue #84): the sampling/guidance operating point,
# exposed as per-deck UI controls threaded per generate() call like cfg_drums.
# The two CFG scales share the model's real [-1.0, 7.0] bound (the UI exposes
# [0, 5], `discretize_cfg`); top-k is a positive int. Temperature is clamped up
# to a small positive floor — the reference slider bottoms at 0, but a
# temperature of exactly 0 divides the sampling logits by zero (NaN), so the
# lowest value that still generates is the floor, not 0.
MIN_CFG = -1.0
MAX_CFG = 7.0
MIN_TEMPERATURE = 0.05
MIN_TOP_K = 1

# MRT2 sampling / guidance operating point (issue #50 reference audit). The
# library constructor defaults (temperature 1.3, top_k 40, cfg_musiccoca 3.0,
# cfg_notes 1.0, cfg_drums 1.0) differ from what every `magenta-realtime`
# example app ships; LSDJ adopts the reference app defaults
# (examples/common/react_ui/defaultParams.ts) so generation matches the tuned
# MRT2 experience rather than the raw library floor. CFG_DRUMS is the baseline
# for an UNSTEERED deck; the per-deck drum-sit control (the Rust store)
# overrides it per generate_chunk when a suppress/force flag is active
# (docs/spike-mrt2.md). TEMPERATURE/TOP_K/CFG_MUSICCOCA/CFG_NOTES are likewise
# the reset-to-default baseline for the per-deck generation controls exposed by
# issue #84 (set_generation overrides them per generate() call).
TEMPERATURE = 1.1
TOP_K = 50
CFG_MUSICCOCA = 1.6
CFG_NOTES = 2.4
CFG_DRUMS = 4.0

# The official models the in-app manager offers to download. This is the
# installable catalog, NOT a discovery gate: `available_models()` discovers any
# model folder on disk (drop-in models, finetunes), so this tuple only seeds the
# "what can I install?" list, not "what can I load?".
KNOWN_MODELS = ("mrt2_small", "mrt2_base")


def available_models() -> list[str]:
    """Every model folder actually on disk, discovered by its files.

    A loadable Magenta model is a `<name>/` dir holding `<name>.mlxfn` +
    `<name>_state.safetensors`; any such folder is offered (so drop-in models
    appear), regardless of KNOWN_MODELS. Note this scans a model's *own* files,
    not the shared `resources/` a load also needs — the model manager reports
    that separately (see the Rust `model_status`)."""
    from magenta_rt import paths

    models_dir = paths.models_dir()
    if not models_dir.is_dir():
        return []
    present = []
    for model_dir in models_dir.iterdir():
        name = model_dir.name
        if (model_dir / f"{name}.mlxfn").is_file() and (
            model_dir / f"{name}_state.safetensors"
        ).is_file():
            present.append(name)
    return sorted(present)


# Embeddings are reused across pad-cursor moves; least-recently-used texts
# are evicted, so active pad targets stay cached through a long session.
EMBED_CACHE_SIZE = 32

# Captured-audio styles (M15, ADR-0011): a pad holds at most
# MAX_STYLE_PROMPTS targets, so this only needs to cover a full pad of
# samples. Embeddings die with the worker — the clip is not retained.
SAMPLE_CACHE_SIZE = 8
MIN_SAMPLE_SECONDS = 3
MAX_SAMPLE_SECONDS = 12


class DeckEngine:
    """One model instance generating a continuous stream in 1-second chunks."""

    def __init__(self, model: str = "mrt2_small"):
        # Deferred import: this module is imported by the controller for the
        # constants above, but the heavy magenta_rt stack must only load in
        # the worker process.
        from magenta_rt.mlx import system

        # Reference-aligned sampling/guidance operating point (see the constants
        # above): match the magenta-realtime apps, not the raw library floor.
        # cfg_drums is the baseline here; the drum-sit control overrides it per
        # generate_chunk when a suppress/force flag is active (issue #50).
        self._system = system.MagentaRT2SystemMlxfn(
            size=model,
            temperature=TEMPERATURE,
            top_k=TOP_K,
            cfg_musiccoca=CFG_MUSICCOCA,
            cfg_notes=CFG_NOTES,
            cfg_drums=CFG_DRUMS,
        )
        self._model = model
        self._state = None
        self._style = None
        self._notes: list[int] | None = None
        self._drums: int | None = None
        self._drums_cfg: float | None = None
        # Live generation params (issue #84): initialised to the reference
        # baseline (the constructor values above), overridden per deck by
        # set_generation and applied to every generate() call.
        self._temperature = TEMPERATURE
        self._top_k = TOP_K
        self._cfg_musiccoca = CFG_MUSICCOCA
        self._cfg_notes = CFG_NOTES
        self._chunk_frames = FRAMES_PER_CHUNK
        self._embed_cache: dict[str, np.ndarray] = {}
        self._samples: dict[str, np.ndarray] = {}

    def diagnostics(self) -> dict[str, object]:
        """Runtime facts matching the cross-platform worker ready contract."""

        try:
            version = importlib.metadata.version("magenta-rt")
        except importlib.metadata.PackageNotFoundError:
            version = "unknown"
        return {
            "runtime": "mlx",
            "accelerator": "metal",
            "model": self._model,
            "magenta_rt_version": version,
            "hardware_qualified": True,
            "capabilities": {
                "weighted_prompts": True,
                "audio_style": True,
                "notes": True,
                "drums": True,
                "negative_prompt": False,
                "explicit_seed": False,
            },
        }

    def _embed_cached(self, text: str) -> np.ndarray:
        if text in self._embed_cache:
            # Refresh recency: dict order is the LRU order.
            self._embed_cache[text] = self._embed_cache.pop(text)
        else:
            if len(self._embed_cache) >= EMBED_CACHE_SIZE:
                self._embed_cache.pop(next(iter(self._embed_cache)))
            self._embed_cache[text] = self._system.embed_style(text)
        return self._embed_cache[text]

    def set_prompt(self, prompt: str) -> None:
        """Embed a text prompt; takes effect on the next generate_chunk()."""
        self.set_style([(prompt, 1.0)])

    def embed_sample(self, sample_id: str, pcm: bytes) -> None:
        """Embed captured deck audio as a reusable style (M15, ADR-0011).

        `pcm` is the wire format (interleaved stereo float32 LE at
        SAMPLE_RATE). The embedding is cached under `sample_id`, so the
        clip itself is dropped after this call; the FIFO command queue
        guarantees a set_style referencing the id arrives afterwards.
        """
        samples = np.frombuffer(pcm, dtype="<f4")
        if samples.size == 0 or samples.size % CHANNELS:
            raise ValueError("sample PCM must be whole interleaved stereo frames")
        seconds = samples.size / CHANNELS / SAMPLE_RATE
        if not MIN_SAMPLE_SECONDS <= seconds <= MAX_SAMPLE_SECONDS:
            raise ValueError(
                f"sample must be {MIN_SAMPLE_SECONDS}-{MAX_SAMPLE_SECONDS}s, "
                f"got {seconds:.1f}s"
            )
        from magenta_rt import audio

        waveform = audio.Waveform(
            samples=samples.reshape(-1, CHANNELS).astype(np.float32),
            sample_rate=SAMPLE_RATE,
        )
        # Embed before evicting: a failed embed must not cost an
        # unrelated cached entry.
        embedding = self._system.embed_style(waveform)
        if sample_id not in self._samples and len(self._samples) >= SAMPLE_CACHE_SIZE:
            self._samples.pop(next(iter(self._samples)))
        self._samples[sample_id] = embedding

    def set_style(
        self,
        prompts: list[tuple[str, float]],
        sample_keys: frozenset[str] = frozenset(),
    ) -> None:
        """Blend weighted prompt embeddings into the active style.

        MusicCoCa embeddings are plain 768-dim vectors (docs/spike-mrt2.md),
        so a morph between prompts is their weighted average. Keys in
        `sample_keys` resolve from the captured-audio cache (M15) instead
        of the text embedder. Takes effect on the next generate_chunk().
        Tempo is emergent from style — there is deliberately no tempo
        parameter (docs/spike-bpm.md).
        """
        weighted = [(text, weight) for text, weight in prompts if weight > 0]
        if not weighted:
            raise ValueError("set_style needs at least one prompt with weight > 0")
        total = sum(weight for _, weight in weighted)
        blend = np.zeros(0)
        for key, weight in weighted:
            if key in sample_keys:
                if key not in self._samples:
                    # The embedding died with a previous worker (restart /
                    # model switch); the clip is gone, so re-sampling is
                    # the only recovery.
                    raise ValueError(f"unknown sample {key!r} — re-sample the deck")
                # Refresh recency (dict order is the LRU order, like the
                # text cache): a sample still on the pad is touched by
                # every style send, so it can never be the eviction
                # victim while it is live.
                embedding = self._samples.pop(key)
                self._samples[key] = embedding
            else:
                embedding = self._embed_cached(key).astype(np.float32)
            term = (weight / total) * embedding
            blend = term if blend.size == 0 else blend + term
        self._style = blend

    def set_notes(self, notes: list[int] | None) -> None:
        """Replace the held note conditioning wholesale (ADR-0023).

        `notes` is the full current NOTE_SLOTS multihot (idempotent
        full-state, never a delta) or None to return to masked — the model
        plays freely. Takes effect on the next generate_chunk() and persists
        until changed.
        """
        if notes is not None:
            if len(notes) != NOTE_SLOTS:
                raise ValueError(
                    f"notes must hold {NOTE_SLOTS} slots, got {len(notes)}"
                )
            if any(state not in NOTE_STATES for state in notes):
                raise ValueError("note states must be -1, 0, 1, 2, or 3")
        self._notes = None if notes is None else list(notes)

    def set_drums(self, flag: int | None, cfg: float | None = None) -> None:
        """Set the drum conditioning (ADR-0023): flag 0 suppresses drums,
        1 forces them, None returns to masked — the model decides.

        `cfg` is the classifier-free-guidance strength (issue #50): how hard
        the model binds to the drum conditioning, a float in
        [MIN_DRUM_CFG, MAX_DRUM_CFG] (None falls back to the constructor
        baseline). It guides every generate_chunk() regardless of the flag
        (like the reference — see generate_chunk), not only while suppressing.
        Takes effect on the next generate_chunk() and persists until changed."""
        if flag is not None and flag not in (0, 1):
            raise ValueError("drum flag must be 0, 1, or None")
        if cfg is not None and not MIN_DRUM_CFG <= cfg <= MAX_DRUM_CFG:
            raise ValueError(
                f"drum cfg must be in [{MIN_DRUM_CFG}, {MAX_DRUM_CFG}] or None"
            )
        self._drums = flag
        self._drums_cfg = cfg

    def set_generation(
        self,
        temperature: float,
        top_k: int,
        cfg_musiccoca: float,
        cfg_notes: float,
    ) -> None:
        """Set the live generation operating point (issue #84).

        `temperature` (sampling randomness) is clamped up to MIN_TEMPERATURE — a
        temperature of exactly 0 divides the sampling logits by zero. `top_k`
        (an int >= MIN_TOP_K) is the sampling top-k. `cfg_musiccoca` (prompt
        adherence, all generation) and `cfg_notes` (note adherence, bites only
        while note-steering) are guidance scales in [MIN_CFG, MAX_CFG]. Applies
        to the next generate_chunk() / render_clip() and persists until changed.
        """
        if not isinstance(top_k, int) or isinstance(top_k, bool) or top_k < MIN_TOP_K:
            raise ValueError(f"top_k must be an int >= {MIN_TOP_K}")
        for name, value in (("cfg_musiccoca", cfg_musiccoca), ("cfg_notes", cfg_notes)):
            if not MIN_CFG <= value <= MAX_CFG:
                raise ValueError(f"{name} must be in [{MIN_CFG}, {MAX_CFG}]")
        self._temperature = max(MIN_TEMPERATURE, temperature)
        self._top_k = top_k
        self._cfg_musiccoca = cfg_musiccoca
        self._cfg_notes = cfg_notes

    def set_chunk_frames(self, frames: int) -> None:
        """Set the per-chunk frame count (the ADR-0023 performance knob).

        Takes effect on the next generate_chunk(). Unlike note/drum
        steering this is a MODE, not conditioning — it survives play/stop
        (the arm state that drives it lives shell-side)."""
        if (
            not isinstance(frames, int)
            or isinstance(frames, bool)
            or not MIN_CHUNK_FRAMES <= frames <= MAX_CHUNK_FRAMES
        ):
            raise ValueError(
                f"chunk frames must be an int in "
                f"[{MIN_CHUNK_FRAMES}, {MAX_CHUNK_FRAMES}]"
            )
        self._chunk_frames = frames

    @property
    def chunk_seconds(self) -> float:
        """Seconds of audio the next generate_chunk() will produce — the
        worker's pacing unit."""
        return self._chunk_frames * FRAME_SECONDS

    def generate_chunk(self) -> bytes:
        """Generate chunk_seconds of audio, continuous with the previous call.

        Returns interleaved stereo float32 little-endian PCM at SAMPLE_RATE
        (the WebSocket wire format). With no prompt set the model runs
        unconditioned. Held note/drum conditioning (ADR-0023) applies to
        every chunk until changed; the chunk length follows the held
        performance knob (set_chunk_frames).
        """
        waveform, self._state = self._system.generate(
            style=self._style,
            notes=self._notes,
            drums=None if self._drums is None else [self._drums],
            # The Drums Adherence scale always guides the drum conditioning
            # (like the reference), independent of the suppress flag; `None`
            # (its unset default) falls back to the constructor baseline
            # (issue #50).
            cfg_drums=self._drums_cfg,
            # The live generation params (issue #84): the deck's tuned
            # sampling/guidance operating point, applied every chunk.
            temperature=self._temperature,
            top_k=self._top_k,
            cfg_musiccoca=self._cfg_musiccoca,
            cfg_notes=self._cfg_notes,
            frames=self._chunk_frames,
            state=self._state,
        )
        # Onset is a one-chunk event (ADR-0023): a held press marked "first
        # time" (state 2) has now sounded its attack, so decay it to sustain
        # (1) for the next chunk. Without this a held note re-attacks every
        # chunk (~5 Hz on an armed deck) instead of ringing. Chord-follow's
        # state 3 is NOT an onset — it stays, the model re-decides the attack
        # each chunk by design.
        if self._notes is not None:
            self._notes = [
                NOTE_SUSTAIN if state == NOTE_ONSET else state for state in self._notes
            ]
        return waveform.samples.astype(np.float32).tobytes()

    def render_clip(self, prompt: str, seconds: float) -> bytes:
        """Render a standalone clip from a text prompt (M18, ADR-0012).

        Deliberately independent of the live stream: fresh generation
        state and its own style blend, so `self._state`/`self._style`
        — the stream's continuity — are never touched. The worker only
        runs this while the deck is stopped, so render time cannot
        stall pacing. Returns wire-format PCM trimmed to `seconds`.
        """
        style = self._embed_cached(prompt).astype(np.float32)
        state = None
        pieces = []
        for _ in range(math.ceil(seconds / CHUNK_SECONDS)):
            waveform, state = self._system.generate(
                # The deck's tuned sampling/guidance (issue #84) shapes its pad
                # renders too — the character carries into rendered clips.
                # cfg_notes has no effect here (a clip render has no held notes).
                style=style,
                temperature=self._temperature,
                top_k=self._top_k,
                cfg_musiccoca=self._cfg_musiccoca,
                cfg_notes=self._cfg_notes,
                frames=FRAMES_PER_CHUNK,
                state=state,
            )
            pieces.append(waveform.samples.astype(np.float32))
        samples = np.concatenate(pieces)[: round(seconds * SAMPLE_RATE)]
        return samples.tobytes()
