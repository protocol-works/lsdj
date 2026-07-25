# Issue #66 — SA3 LoRA adapter manager: hardware/UX checklist

Issue #66 builds the production importer for Stable Audio 3 LoRA finetunes on
top of the spike's merge-at-load runtime (ADR-0028, `docs/spike-sa3-lora.md`):
a `sa3-loras/<base>/<slug>/` registry in the app data dir owned by the Rust
shell, a `lora` field on `/api/generate` that rides `--lora`/`--lora-strength`
into the pinned CLI, plus one contextual LoRA control in every generation
surface for steering and lifecycle management (HuggingFace repo id or local
`.safetensors`, progress, list, and delete).

Unit tests cover name/path trust boundaries, the safetensors-header
validation (pickle refusal, convention detection, base inference), the exact
argv, the `/api/generate` contract, and the contextual generation UI. What follows
needs a real machine with the SA3 checkout warmed — the sandbox cannot run
the shell or MLX. The public PEFT adapter the spike used
(`motiftechnologies/stable-audio-3-maqam-lora`, medium base) is the reference
adapter throughout.

## Import

- [ ] Media Explorer → Generate shows the stable-height **LoRA · None
      installed** control even with an empty registry. Open it: the contextual
      panel says "No adapters installed", offers **Install adapter…**, and has
      an **Open folder** action that reveals
      `~/Library/Application Support/LSDJai/sa3-loras`.
- [ ] Expand **Install adapter…**, enter
      `motiftechnologies/stable-audio-3-maqam-lora`, and Install: fetch /
      download / install progress shows in the same panel, then the adapter
      lists as **New** under Available adapters:
      `stable-audio-3-maqam-lora` — **Medium DiT (tracks)**, ~200 MB.
- [ ] The new adapter remains off until **Apply** is pressed; installation alone
      does not silently alter the next generation.
- [ ] Cancel works mid-download and surfaces as a clean stop, not an error.
- [ ] Import the same adapter's `adapter_model.safetensors` via **Import
      file…** (download it separately first): refused as already installed
      when the slug collides; imports cleanly under a different folder name.
- [ ] A pickle file (`.ckpt`/`.pt` — rename any small file) is refused by the
      file picker's filter, and forcing a path at it (e.g. via the HF id of a
      pickle-only repo) yields the explicit pickle refusal, not a generic
      error.
- [ ] A non-LoRA `.safetensors` (e.g. a Magenta `*_state.safetensors`) is
      refused with "not a recognised SA3 LoRA".

## Generate

- [ ] Media Explorer → Generate, engine **Track (SA3 medium)**: the LoRA
      control reads **Off**. Its panel offers the Maqam adapter under Available;
      Apply moves it to **Applied to this generation**, labels it **On**, and
      lights the collapsed control with `maqam ×1`.
- [ ] On the SFX/Music pad engines, Maqam appears under **Incompatible
      adapters** with "Medium DiT — select Track to apply" instead of silently
      disappearing. Magenta reads **Unavailable for Magenta** but still opens
      the panel for installation and management.
- [ ] Compose two tracks from the same prompt + fixed conditions, adapter
      None vs Maqam at ×1: audibly different in character (the spike measured
      a difference as large as the signal itself).
- [ ] Strength ×0.25 vs ×1.5 audibly scales the adapter's influence.
- [ ] Backend log (the generation server's stderr) shows the CLI's
      `lora: merged 168 layer(s) from 1 adapter(s)` line during a LoRA take.
- [ ] Magenta engine ignores the adapter path entirely (no `lora` in its
      render request).

## Bypass (ADR-0028's bit-exact claim)

- [ ] Two tracks with the same prompt and `seed`, adapter **None** vs Maqam at
      **×0 / Bypassed**: byte-identical WAVs (compare SHA-256). The panel and
      collapsed summary explicitly say **Bypassed**, not On. Seed rides via the
      `/api/generate` `seed` field (issue #54) — use
      `scripts/verify_sa3_surface.py`-style direct calls if the UI has no seed
      control.

## Registry lifecycle

- [ ] Quit and relaunch: the adapter is still listed (the registry is the
      directory layout — nothing else to persist).
- [ ] Delete from the contextual panel: an applied adapter asks for
      confirmation; after confirmation the row disappears, the folder is gone,
      and the stale local choice is omitted from the next generate.
- [ ] Drop a valid adapter folder in by hand (`sa3-loras/medium/<name>/` with
      its `.safetensors`): the watcher lists it live, and it generates.

## Wrong-base refusal

- [ ] POST `/api/generate` directly with `kind: "sfx"` and the medium
      adapter's name: 422 naming the base mismatch (the UI never offers the
      combination; the boundary still refuses it).

## LoRA stack (multi-adapter follow-up)

The generate surfaces expose one stable-height **LoRA control**. Opening its
portalled contextual panel separates Applied, Available, and Incompatible
adapters; each applied adapter has explicit On/Bypassed state, a labelled
strength trim, Bypass/Enable, and Remove. `/api/generate` takes `loras` (a list
of up to 4 `{name, strength}` entries).

**Prerequisite (before anything below):** the pin is back on upstream
`Stability-AI/stable-audio-3` (our LoRA support landed as PR #57; PR #65
added the per-adapter `--lora PATH strength=S` syntax the app now emits).
Settings drawer → Model library → the SA3 row shows **Update available** —
run the update so the installed checkout matches the pin. On the old fork
checkout, a multi-adapter generation fails in argparse
(`unrecognized arguments`); a hand-patched checkout is replaced cleanly.

- [ ] With two medium adapters installed: Apply both from the Generate panel
      with distinct trims (e.g. ×1 and ×0.5) — the collapsed control reads
      **2 active**, the backend log shows
      `merged … from 2 adapter(s)` and the take audibly carries both.
- [ ] Bypass one applied adapter: its row and collapsed summary explicitly say
      **Bypassed**; Enable restores its last non-zero strength. Same prompt +
      seed with that adapter removed entirely is byte-identical.
- [ ] With 4 applied, a 5th Apply action is disabled with the "Stack full"
      hint (and a direct POST with 5 entries returns 422, as
      does a duplicated name).
- [ ] Deck controls/panels take the deck accent (A lime / B violet by default);
      Media Explorer controls/panels take the master accent.
- [ ] Remove and re-Apply an adapter: it remembers its trim for the session.
- [ ] Install enough adapters to overflow the old one-line rack: the collapsed
      generation control remains one fixed-height control and the adapter list
      scrolls inside its panel without taking height from the media library.
