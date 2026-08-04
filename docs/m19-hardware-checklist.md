# M19 hardware checklist — track deck

Manual verification of the M19 exit criteria with the physical device
and ears. The measurable half is `verify_m19.mjs` (in `just
verify-ui`); this covers the last hop — the FLX4 driving a playback
deck, the track sitting in the mix like a live deck, and the by-ear
half no script can tick (ADR-0013).

## Setup

- [ ] DDJ-FLX4 connected, app open in Chromium, MIDI connected (green
      LED in the statusbar).
- [ ] **SA3 weights warm (one-time):** install Stable Audio 3 from the app's
      settings drawer (the model manager), which pre-warms all three DiTs — the
      medium track model is a 5.9 GB download that must never happen inside a
      request. Skip if already installed; the Magenta engine needs no pre-warm.
- [ ] Deck A streaming a style (e.g. `driving techno, four on the
      floor`); deck B idle.

## Compose and load

- [ ] Media Explorer → Generate: compose a 2:00 SA3 track from a
      prompt. The row shows "composing…", then the track; deck A never
      stutters. (The roadmap's reference is ~15 s wall on M4-Pro-class
      hardware — note the measured wall on this machine: ___ s.)
- [ ] Load it onto deck B: the style pad gives way to the track
      overview, status reads "Track — paused", the clock shows
      0:00 / 2:00, and the BPM stat shows the offline verdict (or an
      honest dash).
- [ ] Folder tab: choose a local folder of audio; the files list;
      loading one onto deck B works the same.

## Transport on the device

- [ ] PLAY/PAUSE (hardware deck 2) starts the track; press again —
      it parks where it is, and resumes from there.
- [ ] CUE (transport, hardware deck 2) returns the track to the top,
      parked.
- [ ] Click (or drag) on the track overview jumps the playhead; the
      audio follows seamlessly.
- [ ] The jog wheel seeks the track — clockwise forward, a slow turn
      rides the playhead, a spin jumps bars. Judge the feel: the rate
      is 0.5 s per tick (`JOG_SEEK_SECONDS`); if it's too touchy or
      too slow, note the better value. On a realtime deck the jog
      stays inert.
- [ ] Let the track run out: explicit silence, status "Track — ended",
      playhead parked at the end. PLAY restarts from the top.

## The track is a full citizen of the mix

- [ ] Channel fader 2, EQ kills, and Color FX act on the track exactly
      as on a live deck; the crossfader blends A↔B with no level
      surprise (the M17 staging holds).
- [ ] The synced dub echo on deck B follows the track's BPM readout.
- [ ] Headphone cue on channel 2 previews the track in the phones.
- [ ] Freeze pads: previously filled slots still play over/instead of
      the track; an EMPTY pad press is refused (no capture from a
      parked stream — slicing is deferred past M19).

## Hardware browse

- [ ] The browse rotary scrolls the *visible* explorer tab's list —
      crates, generated tracks, samples, or folder files — and the highlighted
      row stays visible after browsing past the tray's viewport.
- [ ] Pressing the rotary cycles the tabs: Crates → Generate → Samples → Folder
      and around. The byte (`0x96 41`) is interpolated from the
      DDJ-400 family — if the press does nothing, read the actual
      bytes off the monitor and correct `flx4.ts` +
      `midi-ddj-flx4.md`.
- [ ] LOAD 2 loads the highlighted item onto deck B: a track flips it
      to playback, a crate flips it back to realtime (style + FX land
      as in M16).
- [ ] The "Live stream → B" button returns deck B to its style pad;
      PLAY streams again without a reload.
- [ ] The deck's own "Back to live" button (next to the track title)
      does the same — the exit that works with zero saved crates.

## Hands-off run (the exit criterion)

- [ ] A 2–3 minute mini-set: deck A live, deck B on a composed track —
      bring the track in with fader and EQ, ride Color FX on it, jump
      its playhead, crossfade back to the live deck, and return B to
      its stream — every hardware move reflected live in the UI, no
      reload, no underruns on the live deck.

When every box ticks, flip M19's status in [`ROADMAP.md`](ROADMAP.md)
to ✅ done and ADR-0013 to Accepted.
