# Issue #94 — beat-reactive performance visuals checklist

Manual verification of the global smoked stage, gain/beat glows, and fixed-size
gain-reactive plus lattice in the packaged/native app. Pure signal math,
lifecycle ownership, App wiring, persistence, and accessibility are automated;
this checklist covers what only real speaker timing, WebKit compositing, a
resizable native window, and sustained runtime observation can establish.

Do not judge timing against the waveform or playhead. The native render clock is
speaker-aligned, so every timing check below uses audible beats or a click track
at the selected output device as the reference.

## Setup and evidence

- [ ] Build and launch the packaged macOS app from the issue-94 branch/commit;
      record the exact commit below.
- [ ] Record date, tester, Mac model, macOS version, display, output device,
      sample rate/buffer size, and whether Web Inspector is attached.
- [ ] In Settings → Appearance, confirm **Performance visuals** is on by default
      for a fresh profile.
- [ ] Prepare one clean rhythmic realtime prompt, one beatless realtime prompt,
      one gridded track with an audible click/kick, and one gridless track.

## Visual restraint and layout safety

- [ ] At silence, the booth remains matte/dark: there is no residual pulse,
      full-screen flash, error-like red state, or foreground contrast loss.
- [ ] With one deck audible, its color field stays behind the smoked booth and
      its glow bloom remains weighted to that deck's side and color; the plus
      marks remain small and fixed in size.
- [ ] Controls, labels, waveforms, menus, and focus rings remain readable through
      the brightest observed level; the glow never exceeds a restrained club-light
      effect.
- [ ] The underlay never intercepts pointer, wheel, keyboard, or drag gestures.
      Header/titlebar dragging, drawers, Media Explorer resizing, deck controls,
      and mixer controls all behave normally.
- [ ] No horizontal or vertical scrollbar is introduced by either oversized glow
      at the normal window size or the narrow responsive layout.
- [ ] Resize through practical minimum/maximum sizes, minimize, wait at least ten
      seconds, and restore. Layout and animation resume without a jump, overflow,
      stale flash, or detached background.

## Realtime source honesty

- [ ] Start deck A with a confidently detected, steady rhythm. After acquisition,
      its broad glow bloom lands with the beat heard at the speakers and does not
      lead by the generation/native buffer depth.
- [ ] Leave the realtime source running for at least five minutes. The pulse does
      not drift away from audible beats.
- [ ] Prime deck B while it generates and meters in cue. Its glow contribution
      remains zero on the master-facing visuals until PLAY makes it on-air.
- [ ] STOP the rolling deck. Its glow contribution disappears immediately even
      if the channel meter has a cached release tail; restarting waits for a
      fresh honest clock before beat pulses return.
- [ ] Run the beatless prompt for at least 30 seconds. A level wash may follow real
      master energy and the fixed grid may breathe with it, but no periodic glow
      bloom appears while beat confidence is absent.

## Playback source honesty

- [ ] Load and play the gridded click/kick track. Its pulse lands on the audible
      track beat at rates 0.92, 1.00, and 1.08 and re-anchors correctly after seek.
- [ ] Pause the track. Its glow contribution hard-zeros immediately. Resume
      restores it without a stale pre-pause beat.
- [ ] Load/play the gridless track. A level wash may appear, but there is no
      periodic beat-driven glow bloom.
- [ ] Switch a rolling deck from playback back to realtime. There is no pulse from
      the old track clock; visuals remain honest while live analysis reacquires.

## Crossfader contribution and two-deck phase

- [ ] With both decks loud and rolling, move fully to A: B contributes no wash or
      glow bloom. Move fully to B: A contributes none.
- [ ] Sweep slowly through center. Color/energy transfer is continuous under the
      equal-power law, without a brightness doubling, dark notch, or loop restart.
- [ ] At center with equal levels and aligned audible click tracks, both deck-colored
      glows bloom together and neither color dominates unexpectedly.
- [ ] Offset one deck by roughly half a beat. At center the two deck glows bloom
      independently at their own audible beats; the feature does not invent a
      shared downbeat or emphasize every fourth beat.
- [ ] Lower one deck's channel contribution while centered. The ambient color
      balance follows what reaches the master, not a binary active-deck flip.
- [ ] Drive both channels hot. The master-level response remains bounded and does
      not become a high-contrast full-window strobe.

## Appearance setting, accents, and layouts

- [ ] Toggle **Performance visuals** off while both decks play. The wash and fixed
      lattice disappear immediately, and the original fully matte panels return;
      leaving the app open shows no visual updates.
- [ ] Quit/relaunch with the toggle off: it remains off. Turn it on, relaunch, and
      confirm it remains on.
- [ ] While audio runs, switch Accent among Acid Lime, Violet, and Cyan. Both
      deck colors update immediately from theme tokens without reload, stale color,
      or animation restart.
- [ ] Repeat a representative two-deck case with Beat View set to Center,
      Vertical, Top bar, and Off. The stage/lattice stays correctly layered and
      no layout gains unwanted overflow.
- [ ] Open/close Settings and Media Explorer, resize the tray, and exercise a deck
      performance door. All surfaces remain above the backdrop and interactive.

## Reduced motion

- [ ] With the feature on, enable macOS **Reduce motion** while the app is open.
      Beat-driven glow bloom/scale stops immediately and no visual RAF remains.
- [ ] The remaining wash is static, restrained, and changes only after a direct
      crossfader or audibility action; live level/beat changes do not animate it.
- [ ] Disable **Reduce motion** live. Exactly one visual loop resumes and current
      speaker-clock phase is used; there is no catch-up burst.
- [ ] Relaunch with **Reduce motion** already enabled. No beat pulse or autonomous
      glow motion appears during startup.

## Runtime and performance inspection

- [ ] With Web Inspector attached, confirm the feature adds one RAF callback for
      both decks, not one per deck. Toggling it off or enabling reduced motion
      removes that callback; toggling back on restores exactly one.
- [ ] Inspect native/webview traffic while both decks run. Enabling the visuals
      adds no IPC/network requests; every frame reads existing cached snapshots.
- [ ] Use the Performance timeline for at least 60 seconds. The visual loop writes
      only the four `--performance-*` custom properties, performs no layout reads,
      and shows no repeating forced-layout/layout-thrash cycle.
- [ ] Compare Activity Monitor (or equivalent Web Inspector metrics) for at least
      two minutes enabled and two minutes disabled under the same two-deck load.
      Record CPU/GPU observations below; there is no obvious sustained regression,
      UI stall, or audio underrun increase.

## Result

- Date:
- Tester:
- Commit:
- Machine / macOS / display:
- Output device / sample rate / buffer:
- Realtime model and prompts:
- Playback tracks and grid status:
- Enabled CPU/GPU observation:
- Disabled CPU/GPU observation:
- Result: [ ] pass  [ ] fail
- Notes / final tuning changes:
