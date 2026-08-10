# Prompting the realtime decks (Magenta RealTime 2)

How steering actually works, and what three rounds of in-repo measurement
say it can and cannot do. Sources: ADR-0004, `docs/spike-mrt2.md`,
`docs/spike-bpm.md`; upstream is google/magenta-realtime on GitHub.

## The mechanism: a weighted blend of style vectors

A deck's style is 1–8 weighted prompts. Each prompt text is embedded once
(MusicCoCa, a 768-dim vector) and the deck plays the *weighted average* of
the vectors. The 2D style pad is just a weight surface: the cursor's
distance to each target sets that target's weight. `set_prompt` is sugar
for a single-target style.

Consequences worth exploiting:

- **Morphs are real and continuous.** Moving the cursor between two targets
  interpolates in embedding space — a genuine style morph, not a crossfade
  of two renders. Slow cursor drags are a legitimate performance gesture.
- **Blends average, not layer.** Two prompts don't stack instruments; they
  land *between* the styles. "jazz" + "techno" ≈ something jazz-techno-ish,
  not a jazz band over a techno beat. If you want a specific combination,
  say it in ONE prompt ("jazz chords over a techno beat").
- **Contrast makes the pad playable.** Author pad corners with genuinely
  different styles (energy, texture, era) so the cursor has somewhere to
  go. Four near-synonyms make the whole pad sound the same.

## What a prompt should look like

The embedding is a *style* vector, not an instruction follower. Descriptive
noun phrases beat imperative sentences:

- Good: `warm disco funk`, `dark minimal techno, rolling bassline`,
  `ambient dub, tape delay, spacious`, `breakbeat jungle, chopped amens`
- Weak: `play something that builds energy over time` (temporal
  instructions don't exist in a style vector), very long prose (dilutes
  into mush).

Keep prompts to a genre core plus 1–3 texture/instrumentation qualifiers.
Changes land at the next chunk boundary and are heard once the deck's
buffered audio drains (a few seconds) — plan steering moves a phrase ahead,
the audio stays continuous through the change.

## Measured limits — do not fight these

- **Tempo is not steerable. Never promise a BPM on a realtime deck.**
  Three spike rounds measured it (ADR-0004): BPM text in the prompt is
  unreliable and sometimes pushes tempo the *wrong way*; injected clock
  pulses are not interpreted as a clock; the exported model's sampling is
  deterministic, so these are stable attractors, not noise the next try
  fixes. When the set needs a tempo, generate an SA3 track (which honors
  prompt BPM well) and beat-match playback decks instead.
- **Drum conditioning is a density/feel knob, not a rate input.**
  `set_drums 'suppress'` keeps drums out so a realtime deck can sit beside
  a playing deck; it does not slow or speed anything. It sticks across
  play/stop/model switches until you hand back `'auto'`.
- **Prompt adherence is deliberately soft.** The worker runs a lowered
  guidance (1.6 vs the 3.0 default) because harder adherence sounded worse
  live. Expect the deck to *lean toward* the prompt, not obey it. Steer by
  iterating: nudge the blend, listen, nudge again.

## Note steering

`set_notes(deck, pitches, mode?)` conditions harmony with held MIDI
pitches (0–127). `chord` (default) hands articulation to the model; `onset`
marks the pitches as fresh attacks (you own the timing). Empty list clears.
It resets on play/stop/model switch — start the deck, then steer — and like
style changes it is heard after the buffer drains. Use it to pull a
realtime deck toward the playing track's key rather than hoping a prompt
names the right key.

## A steering workflow that works

1. Author the pad: 3–4 contrasting targets bracketing where the set might
   go (`set_style`).
2. Park the cursor on the current vibe; bring the deck up off-air and
   audition in the cue.
3. Move in small cursor steps a phrase ahead of where the music should be.
4. For a harmonic lock with the other deck, add `set_notes` with the
   track's chord tones.
5. Save what works: an empty pad slot (`toggle_pad`) captures a freeze loop
   of the live stream you can layer or bring back later.
