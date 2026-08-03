# Issue #54 — SA3 full generation surface: API/by-ear checklist

Issue #54 threads Stable Audio 3's existing audio-to-audio, inpainting,
negative CFG/APG, variation-strength, and fixed-seed controls through
`/api/generate` to the pinned MLX CLI. The normal JSON pad/sample/track calls
remain unchanged; init audio uses bounded multipart. There is deliberately no
app UI in this issue.

Unit tests cover the exact default/extended argv, init-file bytes, JSON and
multipart shapes, all parameter boundaries, WAV format, body limits, and
502/503 degradation. `scripts/verify_sa3_surface.py` covers the loaded model
through the real FastAPI route and writes clips for the facts only ears can
judge.

## Setup

- [x] Stable Audio 3 is **Ready** in the app's model manager.
- [x] Pick a generated SA3 sample from
      `~/Documents/LSDJ/generated_samples` that is at least four seconds long.
      Freeze WAVs are 48 kHz float and are intentionally rejected by this
      API-only surface; generated SA3 WAVs are native 44.1 kHz PCM16.
- [x] From `backend/`, run:

      ```sh
      uv run python -u scripts/verify_sa3_surface.py \
        "$HOME/Documents/LSDJ/generated_samples/Cat (3).wav"
      ```

      Outputs land in `/tmp/lsdj-issue54` unless `--out-dir` is supplied.

## Machine verdict

- [x] The verifier prints `PASS`.
- [x] `text-baseline.wav` and `seed-repeat.wav` have the same SHA-256 hash
      (fixed seed is byte-identical).
- [x] Variation differs from both the source and text-only baseline above the
      verifier's RMS floor.
- [x] Negative-CFG output differs from the same-seed text-only baseline.
- [x] Inpaint inside-range RMS is non-zero and at least 1.1x outside-range RMS
      after the 250 ms codec guard band.

Measured 2026-07-10 with `Cat (3).wav`: baseline/repeat SHA-256 prefix
`3ecff5e0e96b188a`; variation-vs-source RMS `0.09199938`;
variation-vs-baseline `0.03937942`; negative-vs-baseline `0.21119125`; inpaint
inside/outside RMS `0.08108330` / `0.00651554` (**12.445x** concentration).

## Listen

- [x] **Text baseline:** `text-baseline.wav` is a valid prompt-only generation.
- [x] **Audio-to-audio:** `audio-variation.wav` is audibly related to the source
      but is not the source and is not the text-only baseline.
- [x] **Inpaint:** `inpaint.wav` changes the middle half selected by the default
      range while the opening and ending remain recognisably the source.
- [x] **Negative CFG/APG:** `negative-cfg.wav` audibly steers away from the
      supplied negative prompt compared with `text-baseline.wav`.
- [x] **Seed:** `seed-repeat.wav` sounds identical to `text-baseline.wav`, in
      agreement with their byte-identical hashes.

## Existing app regression

- [x] Generate one SA3 SFX pad and one SA3 Music loop in the app: both load and
      play as before.
- [x] Compose one SA3 medium track in Media Explorer: it saves and loads as
      before.
- [x] With the SA3 checkout unavailable, `/api/generate` still returns 503 with
      the setup hint; a forced CLI failure still maps to 502 rather than crashing
      the generation server (covered by the controller regression tests).
