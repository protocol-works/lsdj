# Issue #59 — SA3 Advanced generation checklist

Use a real installed SA3 medium model. Keep engine, duration, LoRAs, seed, CFG,
and APG identical within each comparison; change only the named variable. Record
the song titles and observations beside each checkbox.

On 2026-07-29 a controlled seven-clip set was generated with Medium/SAME-L,
8 steps, 12 seconds, and seed `59059`. All endpoint runs completed. The exact
repeat was byte-identical; the `drums` negative take reduced HPSS percussive ratio
and onset strength, which is useful corroboration but does not replace listening.

## Negative prompting by ear

- [x] Generate a baseline from a prompt that clearly requests a drum-led track,
  using Advanced, CFG 3.0, APG 1.0, and a fixed seed.
- [x] Repeat with the same recipe plus Avoid = `drums`. The second take is
  materially drum-lighter without collapsing into silence or obvious artifacts.
- [x] Repeat one comparison with free text and one with the **No drums** chip;
  both send the canonical concept `drums` and sound equivalent at the same seed.

## Guidance and reproducibility by ear

- [x] With one fixed seed and an empty Avoid field, compare Guidance Off against
  On at CFG 3.0/APG 1.0. Off retains the former Basic behavior; On is more literal
  to the prompt and its longer generation cost is visible in pending state.
- [x] Compare fixed-seed CFG 1.1 and 4.0. The low value permits more variation;
  the high value adheres more literally without numerical or audio failure.
- [x] Compare APG 0.0 and 1.0 at the same high CFG and seed. Both complete; the
  stabilized take should reduce the over-driven/over-saturated guidance effect.
- [x] Compose twice with one fixed seed and unchanged controls; confirm the
  stochastic input is reproducible. Switch to Random each take and confirm the
  saved recipes contain different explicit seeds.

Human review on 2026-07-29 accepted Avoid = `drums`, Guidance Off/On, and APG
0.0/1.0. It rejected the original CFG 7.0 endpoint as clipping “super hard.” The
WAV confirms this is source clipping: 18.1% of samples are within 1% of full
scale. An identical CFG sweep measured 0.8% at 4.0, 4.5% at 5.0, and 8.1% at
6.0, so the curated UI ceiling is now 4.0. Human review accepted the replacement
endpoint as sounding fine and much stronger than CFG 1.1.

## Recipe persistence

- [x] Compose an Advanced Track with Avoid, CFG, APG, a LoRA stack, and Random
  each take. Quit and relaunch the app.
- [x] Click **Reuse settings**. Prompt, engine, duration, effective LoRAs, Avoid,
  CFG/APG, and the actual used seed return; mode is Advanced and seed is Fixed.
- [x] Reuse a Basic SA3 take. It opens in Advanced with Guidance off and the
  take's actual seed fixed, ready for further steering without losing the take.
- [x] Confirm Title is unchanged and recall neither generates nor loads a deck.
- [x] Remove one saved LoRA, recall again, and confirm its name is reported while
  all remaining recipe fields and adapters still load.
- [x] Confirm a legacy/imported song remains previewable/loadable/deletable and
  has no **Reuse settings** action.

These restart and recall checks are covered at the actual Rust filesystem/
registry boundary plus the Generate-form integration boundary; the shell test
constructs a fresh `SongLibrary` instance after recording the full recipe.

## Scope and layout

- [x] Basic omits Avoid/CFG/APG, sends and saves an explicit random seed, and
  remembers an Advanced draft when toggled away and back.
- [x] Magenta shows the unavailable explanation and its `/api/render` body has no
  SA3 or LoRA fields.
- [x] Samples and both deck generation panels have no Basic/Advanced control and
  retain their existing payloads.
- [x] In the smallest supported media-drawer size, chips wrap, Advanced controls
  stack without clipping, keyboard arrow keys move the segmented choice, focus is
  visible, and every control has a usable accessible name.
