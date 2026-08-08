# Prompting Stable Audio 3 (tracks and samples)

What lands with the SA3 DiTs behind `generate_track` (medium, long-form)
and `generate_sample` sfx/music (small, clips). Grounded in Stable Audio's
prompt conventions (the stableaudio.com user guide) and this repo's
measured runs (`docs/mcp-findings.md`).

## Prompt structure: stacked descriptors, most important first

SA3 responds best to comma-separated descriptor stacks, not prose:

```
<genre/subgenre>, <instrumentation>, <mood/energy>, <production>, <tempo>, <key>
```

- `hyperfocus chiptune, driving 8-bit arpeggios, relentless forward motion,
  tight square-wave bass, 140 BPM` — a real run; the delivered track
  analysed at 140.0 BPM.
- `deep dub techno, chords in a fog of tape delay, subby kick, hypnotic,
  warm analog saturation, 126 BPM, F minor`
- Production words pull real weight: `punchy`, `saturated`, `lo-fi`,
  `sidechained`, `cavernous reverb`, `clean mix`, `four on the floor`.

The text encoder windows at ~256 tokens — plenty, but front-load what
matters; trailing niceties are the first thing diluted.

## Tempo and key actually stick here

Unlike the realtime decks (see the Magenta guide: tempo is NOT steerable
there), SA3 honors `<N> BPM` in the prompt well — measured: a requested
140 BPM track came back analysed at 140.03. Key names (`A minor`) are
respected often enough to be worth stating when you plan to mix
harmonically. Generated tracks carry their detected BPM into the library,
so `sync_deck`/`set_tempo` work on them; imported library tracks may have
`bpm: null` and cannot beat-match.

**Always state a BPM on tracks you intend to mix** — it is the difference
between a beat-matchable deck and a guess.

## Tracks vs samples

- **`generate_track`** (medium DiT, up to 380 s): full arrangements. Long
  prompts can sketch an arc ("sparse intro, building percussion, full drop
  after the midpoint") — structure words influence long-form generations,
  loosely. Remember it is async: job id immediately, ~2.3 s of audio per
  second, +~130 s once when a LoRA stack rides.
- **`generate_sample` kind `music`** (small DiT): loopable phrases and
  beds. Size `seconds` to bars at your BPM (4 bars at 140 ≈ 6.9 s) and it
  loops in a pad slot; say the loop intent (`seamless drum loop`,
  `8-bar pad bed`).
- **`generate_sample` kind `sfx`**: one concrete sonic event, physically
  described — `vinyl spinback`, `808 sub drop, long decaying tail`,
  `riser, white noise sweep, 4 seconds, builds to a cut`. Set
  `one_shot: true` for events; default looping suits beds and textures.
- The `magenta` sample kind is the Magenta renderer, not SA3 — style-vector
  rules from the Magenta guide apply there, and it takes no LoRAs.

## LoRA interplay: adapter owns the timbre, prompt owns the rest

With a LoRA stacked (`list_loras` → `loras: [{name, strength}]`):

- Let the adapter carry the style it was trained on; do NOT fight it with a
  conflicting genre word. Spend the prompt on structure, energy, tempo,
  key: the chiptune adapter + `driving arpeggios, 140 BPM` beats the
  adapter + `orchestral ballad`.
- `strength` ~1 = as trained; below 1 blends the base voice back in; above
  1 (up to 4) exaggerates, and artifacts grow with it. 0 is a bit-exact
  bypass.
- Stacks (max 4) blend adapters — same averaging intuition as the style
  pad: expect *between*, not *both*.
- Bases must match the tool: `medium/…` adapters → `generate_track`;
  `small/…` → `generate_sample` sfx/music.

## Budgeting a set

Generation is serialized server-side — one track at a time. At ~2.3 s of
audio per wall-second (plus the ~130 s LoRA merge), a 3-minute LoRA track
is ~4–5 minutes away; fire it a track early, mix on, and let
`generation_status` tell you when it lands. If a generation fails, the
detail string names the cause — fix the request (length caps, adapter/base
mismatch, magenta + loras) rather than re-rolling blind.
