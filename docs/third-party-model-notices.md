# Third-party model and runtime notices

Audit date: 2026-08-08. Inventory version: `2026-08-08.4`.

This document is a human-readable projection of
[`compliance/model-assets.json`](../compliance/model-assets.json). The JSON
manifest is authoritative for exact revisions and validation. This document
records upstream statements and open decisions; it is not legal advice and does
not assert that any project use or redistribution path has been approved.

## Release blockers found by this audit

1. **LSDJ has no project license file at the audited base revision.** Project
   owners must select the LSDJ code license and notice before publishing a
   licensed source or binary release.
2. **The PyTorch MRT2 converted weights have conflicting provenance metadata.**
   Their cards say Apache-2.0 while also saying the weights are re-keyed,
   numerically identical copies of Google's CC-BY-4.0 weights. The effective
   redistribution, attribution, and acknowledgement path is an owner-review
   gate. Re-keying must not be presented as relicensing.
3. **Current model downloaders are mutable.** `magenta-rt 2.0.2` downloads the
   Google model repository without `revision=`; pinned Stable Audio code does the
   same for `stabilityai/stable-audio-3-optimized`; the LoRA importer fetches
   `resolve/main`. The inventory records audited snapshots, but releases must
   make the runtime request those exact revisions and verify technical
   provenance.
4. **The documented Maqam LoRA has no identified license grant.** Its card says
   `license: other` and supplies no license file. Keep it a user-directed,
   upstream reference. Do not mirror, bundle, or label it official until its
   author and project owners confirm the path.
5. **Two optimized-model source revisions are not identified upstream.** The
   pinned Stable Audio optimized snapshot names the T5Gemma and Stable Audio 3
   Medium source repositories, but does not identify the exact revisions used to
   produce the derivative artifacts. The inventory records those revisions as
   unresolved rather than substituting the repositories' audit-time heads.
6. **The optional Windows CUDA Small Music and Small SFX snapshots remain
   gated and incomplete.** Their immutable revisions and root weight hashes are
   pinned, but authenticated terms review, configuration and nested-component
   hashes, measured VRAM reservations, and physical Windows/NVIDIA qualification
   are incomplete. The installer and runtime fail closed; TFLite remains the
   supported path.

No reviewed third-party model weights belong in installers while their manifest
entry has `redistribution_confirmed: false`.

## Inventory summary

| Asset | Exact revision | Code license | Weight/asset terms | Acquisition | Gate |
| --- | --- | --- | --- | --- | --- |
| LSDJ code | `c9cd822ef6cbb86711e72d35f0f7e50a126d666f` | Unresolved: no project LICENSE/NOTICE found | n/a | Bundled app code | Owners select license and notice |
| Google MRT2 Python runtime (`magenta-rt 2.0.2`) | source `4bf995bdd9c29b818543574e1b3a6e67867c9a58`; wheel SHA-256 in manifest | Apache-2.0 | n/a | Hash-locked bundled sidecar dependency | Package notices not yet wired |
| Google MRT2 weights/resources | `010aa0dcb0dfd27b24f0ad07b4dad63e8f9521cc` | n/a | CC-BY-4.0 plus model-card usage statement | Download, not installer | Runtime pin + owner attribution decision |
| Apolinario/multimodalart PyTorch port | `6d076baa3df3b10448876c400521a015a5137c59` | Apache-2.0 | n/a | Credited implementation reference; not acquired or executed | #108 notice review gate |
| PyTorch MRT2 base | `92087988d05d0fe38b11f021f0b0d00a75afb86b` | Card declares Apache-2.0 remote code | Card says Apache-2.0; underlying Google weights say CC-BY-4.0 | Native exact download | License ambiguity must be resolved |
| PyTorch MRT2 small | `7037d99551c84ac5c6afb7f1a5e58c65e7233dbb` | Card declares Apache-2.0 remote code | Card says Apache-2.0; underlying Google weights say CC-BY-4.0 | Native exact download | License ambiguity must be resolved |
| PyTorch MusicCoCa processor | `236c488e38aa98643805514996934d705668298b` | Conversion-code treatment pending | CC-BY-4.0 | Native exact download | Confirm notice path |
| Stable Audio 3 runtime source | `a0b57f5483c4588f827f3552b7d5c6ca2a9687be` | MIT | n/a | Exact source archive download | Carry MIT notice |
| Stable Audio 3 optimized MLX/TFLite assets | `6736003cb57d06b7b1fdc36fad31b2a3709e4774` | n/a | Stability AI Community License plus Gemma Terms for T5Gemma components | Download, not installer | Runtime pin, owner path, acknowledgement |
| Stable Audio 3 Small Music PyTorch/CUDA | `0fef1392cd842149a2b6d445e181c97608faac06`; root weight SHA-256 in manifest | n/a | Unresolved pending authenticated gated-model review | Optional gated download, disabled | Config/nested hashes, #108 terms, VRAM, and Windows qualification |
| Stable Audio 3 Small SFX PyTorch/CUDA | `ae12755283df9d62ca39a9b050a39a0b607b8c20`; root weight SHA-256 in manifest | n/a | Unresolved pending authenticated gated-model review | Optional gated download, disabled | Config/nested hashes, #108 terms, VRAM, and Windows qualification |
| Google T5Gemma B-B UL2 source model | Exact conversion-source revision unresolved | n/a here | Gemma Terms of Use | Direct source is manually gated; LSDJ consumes Stability's optimized derivative | Identify source revision; owner derivative/notice decision |
| Stable Audio 3 Medium upstream source family | Exact conversion/training-source revision unresolved | n/a | Stability AI Community License plus Gemma Terms | Provenance reference only; direct source is gated | Identify source revision; base terms follow optimized model/LoRA review |
| Motif Maqam LoRA | `3e1d9aa6fcb72a619b4ced00a240c5039f76daf0` | n/a | Unresolved (`license: other` only); Stable Audio base terms also relevant | User-directed upstream download | No mirroring/bundling; runtime pin required |

## Notice inputs

### Google Magenta RealTime 2

- Runtime source: [Apache-2.0 at the locked source commit](https://github.com/magenta/magenta-realtime/blob/4bf995bdd9c29b818543574e1b3a6e67867c9a58/LICENSE).
- Weights and shared MusicCoCa/SpectroStream resources:
  [CC-BY-4.0 model card at the audited snapshot](https://huggingface.co/google/magenta-realtime-2/blob/010aa0dcb0dfd27b24f0ad07b4dad63e8f9521cc/README.md).
- Attribution input: Magenta RealTime 2, authors Google DeepMind; copyright
  2026 Google LLC. Link the exact model card and CC-BY-4.0 legal code.
- The card asks users to act responsibly and not generate content that infringes
  or violates others' rights. Include that link in the model disclosure rather
  than paraphrasing it as a new LSDJ license term.

### PyTorch MRT2 port and snapshots

- Port source: [Apache-2.0 at `6d076…`](https://github.com/multimodalart/magenta-realtime-torch/blob/6d076baa3df3b10448876c400521a015a5137c59/LICENSE).
- The [base](https://huggingface.co/magenta-community/magenta-realtime-2/blob/92087988d05d0fe38b11f021f0b0d00a75afb86b/README.md)
  and [small](https://huggingface.co/magenta-community/magenta-realtime-2-small/blob/7037d99551c84ac5c6afb7f1a5e58c65e7233dbb/README.md)
  cards label the snapshots Apache-2.0 and state that their weights are re-keyed
  and numerically identical to the Google checkpoint.
- Google's source weights are [declared CC-BY-4.0](https://huggingface.co/google/magenta-realtime-2/blob/010aa0dcb0dfd27b24f0ad07b4dad63e8f9521cc/README.md).
  Show both statements until owners resolve the effective path.
- The [MusicCoCa processor snapshot](https://huggingface.co/magenta-community/magenta-rt-musiccoca-torch/tree/236c488e38aa98643805514996934d705668298b)
  is declared CC-BY-4.0 and is also a converted Google artifact.

### Stable Audio 3 and T5Gemma

- Stable Audio source is [MIT at the pinned commit](https://github.com/Stability-AI/stable-audio-3/blob/a0b57f5483c4588f827f3552b7d5c6ca2a9687be/LICENSE),
  copyright 2026 Stability AI.
- The exact optimized model snapshot carries the
  [Stability AI Community License](https://huggingface.co/stabilityai/stable-audio-3-optimized/blob/6736003cb57d06b7b1fdc36fad31b2a3709e4774/LICENSE.md),
  [Gemma Terms](https://huggingface.co/stabilityai/stable-audio-3-optimized/blob/6736003cb57d06b7b1fdc36fad31b2a3709e4774/LICENSE_GEMMA.md),
  and a [NOTICE](https://huggingface.co/stabilityai/stable-audio-3-optimized/blob/6736003cb57d06b7b1fdc36fad31b2a3709e4774/NOTICE).
- Required notice inputs from those upstream files include the Stability AI
  Community attribution, a “Powered by Stability AI” display, and the Gemma
  terms notice. The application surface must also link Stability's
  [acceptable-use policy](https://stability.ai/use-policy) and
  [privacy policy](https://stability.ai/privacy-policy).
- The original [T5Gemma repository](https://huggingface.co/google/t5gemma-b-b-ul2)
  is manually gated on Hugging Face. It requires an account and explicit Gemma
  terms acceptance for direct access. The reviewed evidence does not identify
  the exact revision used for Stability's optimized derivative. That derivative
  is anonymously downloadable, but its pinned repository says T5Gemma is
  redistributed under the Gemma Terms.
- The optional Windows CUDA path separately pins [Small Music at
  `0fef139…`](https://huggingface.co/stabilityai/stable-audio-3-small-music/tree/0fef1392cd842149a2b6d445e181c97608faac06)
  and [Small SFX at
  `ae12755…`](https://huggingface.co/stabilityai/stable-audio-3-small-sfx/tree/ae12755283df9d62ca39a9b050a39a0b607b8c20).
  Public metadata supplied each root weight hash, but the authenticated audit
  has not completed the configuration/nested hashes or exact terms and notices.
  These are optional fail-closed entries: they are neither installable nor
  advertised while `releaseReady` or `gatedArtifactsComplete` is false.

## LoRA inventory and user imports

At the audited revision, `bundled_lora_ids` and `official_lora_ids` are empty.
LSDJ accepts arbitrary user-supplied safetensors and can download a repository
the user names; that generic capability is not an official catalog.

The only adapter named in LSDJ documentation/tests is
[`motiftechnologies/stable-audio-3-maqam-lora@3e1d…`](https://huggingface.co/motiftechnologies/stable-audio-3-maqam-lora/tree/3e1d9aa6fcb72a619b4ced00a240c5039f76daf0).
Its card identifies Motif Technologies, a Stable Audio 3 Medium base, and
`license: other`, but contains no license text or redistribution permission.
Show that provenance and unresolved status. Do not mirror or bundle it.

For every user-imported LoRA, show that LSDJ does not verify the user's rights,
that the adapter may carry independent terms, and that the base-model terms may
still apply. Never infer a license from `.safetensors` format or public access.

## Distribution and credential rules for follow-up implementation

- Keep all model weights out of installers until an exact manifest entry records
  owner-confirmed redistribution permission.
- Downloads must use `revision=<manifest hash>` (or an equivalent immutable URL),
  verify expected provenance/hashes, and fail closed rather than falling back to
  `main` or another mutable reference.
- Before a download whose terms require acknowledgement, show exact model,
  revision, license/terms, notice, privacy, and acceptable-use links. Store the
  acknowledgement against the inventory revision and asset revision.
- A gated upstream may require a user token after terms acceptance. Credentials
  must be redacted from errors/logs and placed in the OS credential store or kept
  intentionally ephemeral—never in plaintext application data.
- The LSDJ license must be presented separately and must say explicitly that it
  does not relicense third-party model weights or adapters.
