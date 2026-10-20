# 0037. Audio-to-audio from library songs: the shell cuts the window

- **Status:** Proposed
- **Date:** 2026-10-15
- **Deciders:** Jake Hartnell

## Context

Many generated tracks have one strong section and one dead one. Re-rolling the
whole prompt lands about one time in three, at 4–8 minutes a render. Stable
Audio 3 can keep audio and regenerate only part of it: `/api/generate` already
takes a multipart `init_audio` WAV with `init_noise_level` and `inpaint_range`
(issue #54). Nothing reached it: the MCP `generate_track` tool and the Generate
tab both sent text only.

Measured CLI facts that shape the design: init audio is aligned at output time
0 and trimmed or zero-padded to `--seconds`; inpainting pastes the kept latents
back, so kept regions survive the model but are re-decoded (a live 5 s keep
correlated 0.86 with its source, not sample-identical); the server accepts
only 44.1 kHz 16-bit PCM.
The init cap was 16 MiB, about 95 s of stereo, so a 3:33 take could not be
sent whole and "replace this dead stretch" was impossible for a full track.

## Decision

- **The shell cuts the source.** `decode::source_clip` decodes any library
  file, cuts `start..end` seconds, and writes 44.1 kHz 16-bit stereo. A
  generated take round-trips bit-exactly, so the model sees the take itself,
  not a resampled copy. Both the MCP tool and the webview (the `song_source_clip` command)
  use it, so the caller never handles WAV formats.
- **Sources are library songs only**, by `file`. An MCP path argument would
  let a loopback client read any file on disk; dropping a WAV in the songs
  folder makes it a source without widening that boundary.
- **The init cap covers the longest track** (`TRACK_MAX_SECONDS` of stereo
  plus header slack, about 64 MiB). The body is bounded and loopback-only; the
  model already holds that much audio when it renders a track that long.
- **Three modes, one plan.** Continue keeps the window and inpaints from its
  end to `seconds`. Replace inpaints typed output seconds. Remix redraws the
  whole window at `init_noise_level`. Replace and remix default the output
  length to the window's. The MCP `source_plan` and the webview `withSource`
  implement the same rules; the server re-validates the numbers.
- **The recipe records the source**: file, mode, window, noise level, and
  inpaint range, so a take's lineage survives and Reuse settings restores it.

## Consequences

- One MCP call can grow a good opening into a full track or patch a dead
  stretch, and the job reports the saved `file`.
- A window over 380 s is refused before the job starts, naming the limit.
- A full-length init upload is now possible; memory per request rises to the
  cap, accepted for a single-user loopback server.
- Continue and replace render the whole output length, kept parts included;
  inpainting is not cheaper than a fresh render, only more likely to land.
- Kept parts come back re-decoded, close to the source but not identical. If
  that is audible, splicing the original audio back over the kept regions
  (with short crossfades at the seams) is the follow-up.

## Alternatives considered

- **Keep the 16 MiB cap and make callers window** — blocks replacing a section
  of any track over ~95 s, the most common case.
- **Mono init audio to halve the bytes** — the CLI duplicates mono to stereo,
  so the kept parts would lose their stereo image.
- **Arbitrary WAV paths over MCP** — a file-read primitive on a network
  endpoint for little gain over the songs folder.
