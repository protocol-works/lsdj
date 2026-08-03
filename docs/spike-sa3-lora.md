# Stable Audio 3 LoRA on the MLX path — spike findings

De-risking spike for [issue #44]. Investigated 2026-06-27 on an Apple M5, 16 GB,
against the pinned SA3 checkout (`bccf5b7`, `sa3-pin.json`), MLX 0.31.2. The
proof-of-concept ran a **real public adapter**
([`motiftechnologies/stable-audio-3-maqam-lora`](https://huggingface.co/motiftechnologies/stable-audio-3-maqam-lora))
through LSDJ's own subprocess path.

[issue #44]: https://github.com/brxs/lsdjai/issues/44

## Recommendation

**Take Path 1 — add LoRA support to the MLX path by merging the adapter into the
DiT weights at load time.** It is self-contained, cheap, safetensors-safe, and
proven by the PoC below. It rides the existing spawned `sa3_mlx.py` subprocess
(ADR-0012's "spawn, never import" is untouched) via a new `--lora` flag. The
merge math (all nine SA3 adapter types) belongs upstream in
`Stability-AI/stable-audio-3`. The trust boundary — **accept `.safetensors`
only, never the pickle `.ckpt` path** — is settled in [ADR-0028](adr/0028-sa3-lora-via-the-mlx-path.md).

The real importer + #43-style registry/UI is the **gated follow-up build issue**
([#66](https://github.com/brxs/lsdjai/issues/66)), not this spike.

## The blocker (recap)

LSDJ runs SA3 through the MLX fork (`optimized/mlx/scripts/sa3_mlx.py`, ADR-0012).
Its CLI loads the DiT from one MLX `.npz` and had **no `--lora`** and zero LoRA
code. LoRA was documented only for the PyTorch Gradio path. So a valid SA3 LoRA
could not be loaded by our runtime at all — and no importer could be specced until
that gap was understood. This spike closes it.

## Answers to the issue's questions

### What is in the adapter, and is it pickle-based? (the trust question)

**The distributed adapter is `.safetensors` (data-only) — not a pickle.** This is
the single most important finding: it removes the arbitrary-code-execution fear the
issue raised.

- The SA3-native trainer saves safetensors with the adapter config JSON-encoded in
  the file **metadata**: `save_lora_safetensors` →
  `safetensors.torch.save_file(fp16_dict, path, metadata={"lora_config": …})`
  (`stable_audio_3/models/lora/utils.py:201`). Tensor keys are
  `<layer>.parametrizations.weight.0.{lora_A,lora_B,M_xs,magnitude,magnitude_r,magnitude_c}`
  (`utils.py:76` `name_is_lora`).
- The HuggingFace `peft` ecosystem (what the public Maqam adapter uses) saves
  `adapter_model.safetensors` + a sibling `adapter_config.json`; keys are
  `base_model.model.<layer>.lora_{A,B}.weight`.
- A pickle `.ckpt` path exists **only** in PyTorch's loader as a legacy fallback —
  `torch.load(path, weights_only=False)` (`utils.py:234`) — and our MLX runtime
  never calls it. The only other pickle vector is the **training-time**
  `--svd_bases_path` `.pt`, loaded `weights_only=True` (safe) and never part of a
  distributed adapter (`loader.py:67`).

**Resulting stance:** the importer accepts safetensors only and **refuses**
`.ckpt`/`.pt`/`.pth`/`.bin`. An imported adapter then cannot execute code; the only
residual surface is numeric (a malformed adapter → bad audio or a load error). See
[ADR-0028](adr/0028-sa3-lora-via-the-mlx-path.md).

### Does the MLX DiT expose the layers a LoRA targets?

**Yes.** The MLX DiT (`models/defs/dit_mlx.py`, `dit_mlx_medium.py`) is built from
named `nn.Linear`/`nn.Conv1d` submodules whose names match the PyTorch layer names
a LoRA targets — verified against the npz keys, e.g.
`transformer.layers.0.self_attn.to_qkv.weight`, `…cross_attn.to_kv.weight`,
`…ff.ff.0.proj.weight`. The npz converter keeps PyTorch names, with two cosmetic
remaps (`.gamma`→`.weight`, `to_local_embed.{0,2}`→`.seq.{0,2}`) and the Conv1d
layout swap `(out,in,k)`→`(out,k,in)` (`dit_mlx.py:286`). So a LoRA's per-layer
delta `W' = W + (α/r)·B·A` can be **merged straight into the npz weight** before the
model is materialised — no runtime parametrization, no per-step cost, and a
bit-exact bypass at strength 0. Fused `to_qkv` needs no special handling (the LoRA
was trained on the same fused layer); Conv1d targets round-trip through the layout
swap.

### Which path is least-effort and most maintainable, and what does it cost?

| Path | What it takes | Cost | Verdict |
|---|---|---|---|
| **1. LoRA in the MLX path** (chosen) | merge adapter into the DiT weight dict at load; new `--lora` flag | ~350-line module, in-checkout; merge runs in a few seconds at load; **peak memory unchanged** (in-place on the weight dict before materialisation); adapter on disk 50–300 MB | **Self-contained, cheap, safe.** |
| 2. Merge in PyTorch, re-export npz | run the full torch stack, `merge_loras_into_base_model`, then re-export a new MLX npz per finetune | needs the (unavailable) torch env; **a fresh multi-GB npz per finetune**; re-optimize each time | Heavy in effort, disk, lifecycle. |
| 3. Wait for upstream native MLX LoRA | nothing now | blocks the feature indefinitely | No timeline; revisit trigger only. |

Path 1 is the only one that is both cheap and available today.

### How does a LoRA fit the per-deck / SA3 subprocess model and #43's registry?

- **Subprocess model unchanged.** The LoRA is passed as a CLI flag to the same
  spawned `sa3_mlx.py` LSDJ already runs (ADR-0012); nothing imports SA3 code into
  the backend. `backend/lsdj/sa3.py`'s `generate` simply gains `--lora …` in its
  argv (the follow-up build issue wires this + the `/api/generate` contract).
- **Many LoRAs over one base.** A LoRA is small and rides a base model; deltas
  accumulate against the *original* weight, so stacking is order-independent for
  linear LoRA, and a per-deck strength knob is just `--lora-strength`. This is a
  different lifecycle from a full model — the registry/UI (the #43 pattern) tracks
  adapters-by-base, not standalone models.
- **Base matching is explicit.** A medium adapter needs `--dit medium`; non-matching
  layers are skipped rather than corrupting weights, and the importer validates the
  pairing.

### Magenta finetuning — when it ships, what artifact, does `mrt mlx export` consume it?

Still upstream-blocked: supervised fine-tuning is "coming soon" on Google's model
card with no finetuned checkpoints to import. Not investigated further here; see
**Revisit triggers**.

## Proof of concept

### What was built

A merge-at-load module `optimized/mlx/models/defs/lora_merge.py` plus a `--lora` /
`--lora-strength` flag on `sa3_mlx.py` (wired through both DiT loaders). The full,
cleanly-applying patch is the appendix.

- **All nine adapter types** are implemented (lora, dora-rows, dora-cols, bora,
  lora-xs, dora-rows-xs, dora-cols-xs, bora-xs), mirroring the PyTorch reference
  forwards in `stable_audio_3/models/lora/model.py` and the
  accumulate-deltas-against-the-original semantics of `merge_loras_into_base_model`.
- **Both conventions** are read: SA3-native (config in safetensors metadata) and
  PEFT (config in a sibling `adapter_config.json`).
- **`-xs` bases are recomputed** from the base weight via SVD with the reference's
  deterministic sign convention (the frozen U/V bases are *not* stored in the
  checkpoint — `name_is_lora` excludes them).
- **Trust boundary enforced:** pickle extensions are refused before any read.

### The real adapter

`motiftechnologies/stable-audio-3-maqam-lora` — a PEFT standard-LoRA finetune for
**stable-audio-3-medium**, rank 64 / alpha 128 (scaling 2.0), targeting
`self_attn.to_qkv`, `self_attn.to_out`, `cross_attn.{to_q,to_kv,to_out}`,
`ff.ff.0.proj`, `ff.ff.2` across all 24 layers = **168 target layers**, all of
which resolved to medium-DiT npz weights and merged.

### Method — LSDJ's exact subprocess path

Three generations through the **same** venv python + `sa3_mlx.py` LSDJ spawns,
with the track-kind flags (`--dit medium --decoder same-l --steps 8`) and a fixed
`--seed` so the runs are comparable:

- **base** — no `--lora`
- **lora** — `--lora <maqam>` (strength 1.0)
- **zero** — `--lora <maqam> --lora-strength 0` (control)

### Results — measurably different, clean bypass

| metric | value |
|---|---|
| RMS(base) | 0.09735 |
| RMS(lora) | 0.02624 |
| RMS(lora − base) | 0.09732 — **100% of base RMS** |
| max\|lora − base\| | 0.946 |
| spectral L2(base, lora) | 2.50 |
| **RMS(zero − base)** (control) | **0.00000000 — bit-exact** |

The LoRA transforms the output substantially (the difference signal is as large as
the signal itself), and `--lora-strength 0` is bit-identical to no LoRA — proving
the change is the adapter, not a code-path artifact. Strength is a free knob
(2.0 scaling here is strong; production would tune it per deck).

### Merge-math correctness

With no torch available, the math is validated numerically (`test_lora_merge.py`,
in the spike scratch — not committed): the **zero-init → identity** invariant holds
for all nine types (a strong guard on axis/reshape/transpose bugs), standard LoRA
and PEFT reconstruct `W + (α/r)·B·A` exactly, `--lora-strength` scales the delta
linearly, the Conv1d layout round-trips, the `to_local_embed` remap resolves, and
the recomputed SVD bases are orthonormal.

## Reproduce

1. `git apply -p1` the appendix patch in the SA3 checkout
   (`~/Library/Application Support/LSDJ/stable-audio-3`).
2. `huggingface-cli download motiftechnologies/stable-audio-3-maqam-lora --local-dir <dir>`
3. From `optimized/mlx`, run twice with a fixed `--seed`, once with
   `--dit medium --decoder same-l --lora <dir>` and once without; compare the WAVs.

## Revisit triggers

- **Upstream native MLX LoRA.** If `Stability-AI/stable-audio-3` adds `--lora` to
  the MLX path, drop our merge module and bump `sa3-pin.json` (ADR-0012's upgrade
  path). Track via the pinned commit's changelog when bumping.
- **Magenta RT 2 finetuning GA.** Revisit when the
  [magenta-realtime `MODEL.md`](https://github.com/magenta/magenta-realtime/blob/main/MODEL.md)
  or the [HF model card](https://huggingface.co/google/magenta-realtime-2) flips
  fine-tuning from "coming soon" to GA, or an exported finetune artifact appears
  that `mrt mlx export` consumes. At that point spec the Magenta side as its own
  investigation (the artifact shape is unknown today).

## Appendix: PoC patch

Applies cleanly with `git apply -p1` from the SA3 checkout root (verified against
`bccf5b7`). Three edits (loaders + CLI) plus the new `lora_merge.py`.

```diff
--- a/optimized/mlx/scripts/sa3_mlx.py
+++ b/optimized/mlx/scripts/sa3_mlx.py
@@ -170,13 +170,19 @@
     return args
 
 
-def load_dit(dit_name: str, T_lat: int, dtype):
+def load_dit(dit_name: str, T_lat: int, dtype, lora_paths=None, lora_strength=1.0):
     cfg = DIT_CHOICES[dit_name]
     ckpt = ensure_local(cfg["ckpt"])
     import importlib, io, contextlib
     mod = importlib.import_module(cfg["loader"])
+    # The loader's own chatter is swallowed, but the LoRA merge summary is routed
+    # to stderr (not redirected) so it stays visible — and is captured by LSDJ,
+    # which folds the subprocess's stderr into its log.
+    lora_log = lambda m: print(m, file=sys.stderr)
     with contextlib.redirect_stdout(io.StringIO()):
-        model = mod.load_dit(str(ckpt), T_lat=T_lat, dtype=dtype, compile_=False)
+        model = mod.load_dit(str(ckpt), T_lat=T_lat, dtype=dtype, compile_=False,
+                             lora_paths=lora_paths, lora_strength=lora_strength,
+                             lora_log=lora_log)
     return model, str(ckpt)
 
 
@@ -387,6 +393,17 @@
                     help="Path to the bundled T5Gemma FP16 .npz (weights + tokenizer). "
                          "Default points at models/mlx/t5gemma_f16.npz next to this script; "
                          "auto-downloaded from HuggingFace if not present.")
+    ap.add_argument("--lora", nargs="+", default=None, metavar="ADAPTER",
+                    help="One or more Stable Audio 3 LoRA adapters to merge into the DiT "
+                         "at load time (issue #44). Each is a .safetensors file (SA3-native "
+                         "train_lora.py output) or a PEFT adapter directory/.safetensors "
+                         "(with its adapter_config.json). ONLY .safetensors is accepted — a "
+                         "pickle .ckpt/.pt is refused (it would execute code on load). The "
+                         "adapter's base must match --dit (e.g. a medium adapter with "
+                         "--dit medium).")
+    ap.add_argument("--lora-strength", type=float, default=1.0,
+                    help="Application weight for every --lora delta (default 1.0). 0 disables "
+                         "the adapter (bit-identical to no LoRA); >1 amplifies it.")
 
     # ── Sampling ──────────────────────────────────────────────────────────────
     ap.add_argument("--seconds", type=float, default=30.0,
@@ -625,7 +642,11 @@
 
     # ── 3b. DiT pingpong sampling ──
     stage("[3/5]", f"DiT — load + sample ({args.steps} steps, σmax={sigma_max:.2f})")
-    t0 = time.time(); dit_model, _ = load_dit(args.dit, T_lat=T_lat, dtype=dtype)
+    if args.lora:
+        sub(f"lora  {', '.join(os.path.basename(p.rstrip('/')) for p in args.lora)}  "
+            f"(strength {args.lora_strength:g})")
+    t0 = time.time(); dit_model, _ = load_dit(args.dit, T_lat=T_lat, dtype=dtype,
+                                              lora_paths=args.lora, lora_strength=args.lora_strength)
     _stage_peak_b('DiT load')
     sub(f"load {time.time()-t0:.1f}s")
 
--- a/optimized/mlx/models/defs/dit_mlx.py
+++ b/optimized/mlx/models/defs/dit_mlx.py
@@ -304,12 +304,17 @@
     return out
 
 
-def load_dit(weights_path, T_lat=320, dtype=mx.float16, compile_=False):
+def load_dit(weights_path, T_lat=320, dtype=mx.float16, compile_=False,
+             lora_paths=None, lora_strength=1.0, lora_log=print):
     """Build MLX DiT and load weights.
 
     weights_path can be either:
       - the sa3-sm-music torch ckpt (slow; converts at load time), OR
       - a pre-converted MLX file (.npz or .safetensors — fast path).
+
+    lora_paths: optional list of SA3 LoRA adapters (.safetensors / PEFT dir) to
+      merge into the weights at load time (issue #44). lora_strength scales every
+      adapter's delta. See models/defs/lora_merge.py.
     """
     p = str(weights_path)
     if p.endswith(".npz") or p.endswith(".safetensors"):
@@ -317,6 +322,11 @@
     else:
         wd = convert_weights_from_torch_ckpt(p)
 
+    if lora_paths:
+        from .lora_merge import merge_loras_into_weights
+        stats = merge_loras_into_weights(wd, lora_paths, strength=lora_strength, log=lora_log)
+        lora_log(f"lora: merged {stats['merged']} layer(s) from {stats['adapters']} adapter(s)")
+
     model = DiT(T_lat=T_lat)
     wd_list = [(k, v.astype(dtype)) for k, v in wd.items()]
     model.load_weights(wd_list, strict=False)
--- a/optimized/mlx/models/defs/dit_mlx_medium.py
+++ b/optimized/mlx/models/defs/dit_mlx_medium.py
@@ -405,11 +405,16 @@
     return out
 
 
-def load_dit(weights_path, T_lat=320, dtype=mx.float16, compile_=False):
+def load_dit(weights_path, T_lat=320, dtype=mx.float16, compile_=False,
+             lora_paths=None, lora_strength=1.0, lora_log=print):
     """Build MLX DiT and load weights.
 
     weights_path: either the .safetensors (we'll convert in-memory) or a
                   pre-converted .safetensors-mlx file.
+
+    lora_paths: optional list of SA3 LoRA adapters (.safetensors / PEFT dir) to
+      merge into the weights at load time (issue #44). lora_strength scales every
+      adapter's delta. See models/defs/lora_merge.py.
     """
     weights_path = str(weights_path)
     if weights_path.endswith(".safetensors") and ("medium-ARC" in weights_path):
@@ -418,6 +423,11 @@
     else:
         wd = dict(mx.load(weights_path))
 
+    if lora_paths:
+        from .lora_merge import merge_loras_into_weights
+        stats = merge_loras_into_weights(wd, lora_paths, strength=lora_strength, log=lora_log)
+        lora_log(f"lora: merged {stats['merged']} layer(s) from {stats['adapters']} adapter(s)")
+
     model = DiT(T_lat=T_lat)
 
     # Cast to target dtype (no-op when already at `dtype`).
--- /dev/null
+++ b/optimized/mlx/models/defs/lora_merge.py
@@ -0,0 +1,350 @@
+"""LoRA merge-at-load for the MLX SA3 DiT.
+
+Spike PoC for LSDJ issue #44 — run Stable Audio 3 LoRA finetunes through the
+MLX inference path. The MLX CLI (`sa3_mlx.py`) has no LoRA support; this module
+adds it the cheapest way that preserves the spawn-never-import model (ADR-0012):
+the LoRA delta is **merged into the DiT weight dict at load time**, before the
+model is built. No runtime parametrization, no extra forward cost.
+
+Trust boundary (the point of the spike): only `.safetensors` adapters are
+accepted. The legacy pickle `.ckpt`/`.pt`/`.bin` path — which `torch.load` would
+execute arbitrary code from — is refused outright. We never call `torch.load`.
+
+Two on-disk conventions are supported:
+
+  * **SA3-native** (`scripts/train_lora.py` output): tensor keys
+    ``<layer>.parametrizations.weight.0.{lora_A,lora_B,M_xs,magnitude,
+    magnitude_r,magnitude_c}`` with the adapter config
+    (``adapter_type``/``rank``/``alpha``/``include``/``exclude``) JSON-encoded in
+    the safetensors **metadata** under ``"lora_config"``. Covers all nine adapter
+    types (lora, dora-rows/cols, bora, and the four -xs variants).
+  * **PEFT** (huggingface `peft`): keys ``base_model.model.<layer>.lora_{A,B}.weight``
+    with ``r``/``lora_alpha`` in a sibling ``adapter_config.json``. Standard LoRA,
+    plus DoRA when ``use_dora`` is set.
+
+The per-adapter-type math mirrors ``LoRAParametrization.*_forward`` in
+``stable_audio_3/models/lora/model.py`` (and the accumulate-deltas-against-the-
+original-weight semantics of ``merge_loras_into_base_model``), computed in
+float32 and cast back to the DiT dtype. `-xs` adapters do not store their frozen
+SVD bases, so they are recomputed from the base weight here, matching the
+reference (`torch.linalg.svd` + a deterministic sign convention).
+"""
+
+from __future__ import annotations
+
+import json
+import os
+
+import mlx.core as mx
+import numpy as np
+
+# Pickle-backed extensions we refuse to load (the trust boundary).
+_PICKLE_EXTS = (".ckpt", ".pt", ".pth", ".bin")
+
+# Adapter param names per type (mirrors utils._get_adapter_param_names).
+_PARAMS_FOR = {
+    "lora": ("lora_A", "lora_B"),
+    "dora-rows": ("lora_A", "lora_B", "magnitude"),
+    "dora-cols": ("lora_A", "lora_B", "magnitude"),
+    "bora": ("lora_A", "lora_B", "magnitude_r", "magnitude_c"),
+    "lora-xs": ("M_xs",),
+    "dora-rows-xs": ("M_xs", "magnitude"),
+    "dora-cols-xs": ("M_xs", "magnitude"),
+    "bora-xs": ("M_xs", "magnitude_r", "magnitude_c"),
+}
+
+
+class LoraError(Exception):
+    """An adapter could not be loaded or applied."""
+
+
+# ── safetensors reading (no torch, no safetensors pkg — MLX reads it) ──────────
+
+def _np(arr) -> np.ndarray:
+    return np.array(arr.astype(mx.float32), dtype=np.float32)
+
+
+def _load_safetensors(path: str):
+    """Return ``(tensors: dict[str, np.ndarray], metadata: dict)``. Refuses pickle."""
+    lower = path.lower()
+    if lower.endswith(_PICKLE_EXTS):
+        raise LoraError(
+            f"refusing to load pickle-format adapter {os.path.basename(path)!r} — "
+            f"only .safetensors adapters are accepted (a .ckpt/.pt is unpickled by "
+            f"torch.load and can execute arbitrary code)"
+        )
+    if not lower.endswith(".safetensors"):
+        raise LoraError(f"not a .safetensors adapter: {path!r}")
+    arrs, meta = mx.load(path, return_metadata=True)
+    return {k: _np(v) for k, v in arrs.items()}, (meta or {})
+
+
+# ── SVD bases for -xs adapters (recomputed; mirrors model.py) ──────────────────
+
+def _canonicalize_svd_signs(U: np.ndarray, Vh: np.ndarray):
+    """Deterministic sign convention: largest-magnitude element of each U column
+    is positive (mirrors model._canonicalize_svd_signs)."""
+    max_abs_idx = np.argmax(np.abs(U), axis=0)
+    signs = np.sign(U[max_abs_idx, np.arange(U.shape[1])])
+    signs[signs == 0] = 1.0
+    return U * signs[None, :], Vh * signs[:, None]
+
+
+def _svd_bases(W0: np.ndarray, rank: int):
+    """Return ``(U[:, :rank], V[:, :rank])`` from the SVD of ``W0`` (fan_out, fan_in),
+    with V such that ``U @ diag(S) @ V.T`` reconstructs W0 (mirrors model.py)."""
+    U_full, _S, Vh_full = np.linalg.svd(W0, full_matrices=False)
+    U_full, Vh_full = _canonicalize_svd_signs(U_full, Vh_full)
+    U = U_full[:, :rank]
+    V = Vh_full[:rank, :].T
+    return U, V
+
+
+# ── per-type merge math (numpy, float32) ───────────────────────────────────────
+
+def _merged_weight(W0: np.ndarray, p: dict, adapter_type: str, scaling: float) -> np.ndarray:
+    """Return the LoRA-merged weight for one layer at full strength (lora_strength=1).
+
+    ``W0`` is (fan_out, fan_in) float32; ``p`` holds the adapter tensors for this
+    layer. Mirrors the matching ``*_forward`` in model.py.
+    """
+    if adapter_type == "lora":
+        delta = p["lora_B"] @ p["lora_A"]
+        return W0 + scaling * delta
+
+    if adapter_type in ("dora-rows", "dora-cols"):
+        norm_dim = 1 if adapter_type == "dora-rows" else 0
+        delta = p["lora_B"] @ p["lora_A"]
+        V = W0 + scaling * delta
+        V_hat = V / (np.linalg.norm(V, axis=norm_dim, keepdims=True) + 1e-12)
+        mag = _mag_2d(p["magnitude"], norm_dim)
+        return V_hat * mag
+
+    if adapter_type == "bora":
+        delta = p["lora_B"] @ p["lora_A"]
+        V = W0 + scaling * delta
+        V_r = V / (np.linalg.norm(V, axis=1, keepdims=True) + 1e-12)
+        inter = p["magnitude_r"].reshape(-1, 1) * V_r
+        H_c = inter / (np.linalg.norm(inter, axis=0, keepdims=True) + 1e-12)
+        return H_c * p["magnitude_c"].reshape(1, -1)
+
+    if adapter_type.endswith("-xs"):
+        rank = p["M_xs"].shape[0]
+        U, V = _svd_bases(W0, rank)
+        delta = U @ p["M_xs"] @ V.T
+        Vfull = W0 + scaling * delta
+        if adapter_type == "lora-xs":
+            return Vfull
+        if adapter_type in ("dora-rows-xs", "dora-cols-xs"):
+            norm_dim = 1 if adapter_type == "dora-rows-xs" else 0
+            V_hat = Vfull / (np.linalg.norm(Vfull, axis=norm_dim, keepdims=True) + 1e-12)
+            mag = _mag_2d(p["magnitude"], norm_dim)
+            return V_hat * mag
+        if adapter_type == "bora-xs":
+            V_r = Vfull / (np.linalg.norm(Vfull, axis=1, keepdims=True) + 1e-12)
+            inter = p["magnitude_r"].reshape(-1, 1) * V_r
+            H_c = inter / (np.linalg.norm(inter, axis=0, keepdims=True) + 1e-12)
+            return H_c * p["magnitude_c"].reshape(1, -1)
+
+    raise LoraError(f"unknown adapter_type {adapter_type!r}")
+
+
+def _mag_2d(mag: np.ndarray, norm_dim: int) -> np.ndarray:
+    """Reshape a (possibly 2D) magnitude vector to broadcast against the weight on
+    ``norm_dim`` (mirrors `magnitude.unsqueeze(norm_dim)` after a squeeze)."""
+    mag = np.squeeze(mag)
+    return mag.reshape(-1, 1) if norm_dim == 1 else mag.reshape(1, -1)
+
+
+# ── checkpoint parsing → normalized per-layer adapter ──────────────────────────
+
+def _layer_to_npz_key(layer: str) -> str:
+    """Map a checkpoint layer name to its DiT npz weight key. The MLX converter
+    renames ``to_local_embed.{0,2}`` → ``to_local_embed.seq.{0,2}`` (dit_mlx.py);
+    every other Linear/Conv1d name passes through unchanged."""
+    layer = layer.replace(".to_local_embed.0", ".to_local_embed.seq.0")
+    layer = layer.replace(".to_local_embed.2", ".to_local_embed.seq.2")
+    return f"{layer}.weight"
+
+
+def _resolve_path(path: str) -> str:
+    """Accept a .safetensors file or a PEFT adapter directory (resolve to the
+    adapter_model.safetensors inside it)."""
+    if os.path.isdir(path):
+        cand = os.path.join(path, "adapter_model.safetensors")
+        if os.path.isfile(cand):
+            return cand
+        hits = [f for f in os.listdir(path) if f.lower().endswith(".safetensors")]
+        if len(hits) == 1:
+            return os.path.join(path, hits[0])
+        raise LoraError(
+            f"{path!r}: expected one .safetensors adapter in the directory, found {hits}"
+        )
+    return path
+
+
+def _parse_adapter(path: str):
+    """Load one adapter and return ``(adapter_type, scaling, layers)`` where
+    ``layers`` maps a checkpoint layer name → its param dict (numpy float32)."""
+    tensors, meta = _load_safetensors(path)
+
+    native_marker = ".parametrizations.weight.0."
+    is_native = any(native_marker in k for k in tensors)
+
+    if is_native:
+        cfg = json.loads(meta.get("lora_config", "{}")) if meta else {}
+        layers = _group_native(tensors)
+        rank = int(cfg.get("rank") or _infer_rank(layers))
+        alpha = float(cfg.get("alpha", rank))
+        adapter_type = _resolve_native_type(cfg.get("adapter_type", "lora"))
+        scaling = alpha / rank
+        return adapter_type, scaling, layers
+
+    # PEFT — config lives in a sibling adapter_config.json
+    peft_marker = ".lora_A.weight"
+    if any(k.endswith(peft_marker) for k in tensors):
+        cfg = _read_peft_config(path)
+        rank = int(cfg["r"])
+        alpha = float(cfg.get("lora_alpha", rank))
+        use_dora = bool(cfg.get("use_dora", False))
+        use_rslora = bool(cfg.get("use_rslora", False))
+        adapter_type = "dora-rows" if use_dora else "lora"
+        scaling = alpha / (np.sqrt(rank) if use_rslora else rank)
+        layers = _group_peft(tensors)
+        return adapter_type, scaling, layers
+
+    raise LoraError(
+        f"{os.path.basename(path)!r}: not a recognised LoRA (no SA3-native "
+        f"parametrization keys and no PEFT lora_A/lora_B keys)"
+    )
+
+
+def _group_native(tensors: dict) -> dict:
+    marker = ".parametrizations.weight.0."
+    layers: dict[str, dict] = {}
+    for k, v in tensors.items():
+        if marker not in k:
+            continue
+        layer, _, param = k.partition(marker)
+        layers.setdefault(layer, {})[param] = v
+    return layers
+
+
+def _group_peft(tensors: dict) -> dict:
+    prefix = "base_model.model."
+    layers: dict[str, dict] = {}
+    for k, v in tensors.items():
+        name = k[len(prefix):] if k.startswith(prefix) else k
+        for suffix, param in ((".lora_A.weight", "lora_A"),
+                              (".lora_B.weight", "lora_B"),
+                              (".lora_magnitude_vector.weight", "magnitude")):
+            if name.endswith(suffix):
+                layers.setdefault(name[: -len(suffix)], {})[param] = v
+                break
+    return layers
+
+
+def _read_peft_config(path: str) -> dict:
+    base = os.path.dirname(path)
+    cfg_path = os.path.join(base, "adapter_config.json")
+    if not os.path.isfile(cfg_path):
+        raise LoraError(
+            f"PEFT adapter at {path!r} is missing its adapter_config.json sibling"
+        )
+    with open(cfg_path) as fh:
+        return json.load(fh)
+
+
+def _infer_rank(layers: dict) -> int:
+    for params in layers.values():
+        if "lora_A" in params:
+            return params["lora_A"].shape[0]
+        if "M_xs" in params:
+            return params["M_xs"].shape[0]
+    raise LoraError("cannot infer LoRA rank (no lora_A / M_xs tensors)")
+
+
+def _resolve_native_type(adapter_type: str) -> str:
+    """Legacy 'dora' → 'dora-rows' (the paper-correct default; mirrors
+    utils.resolve_adapter_type, minus the 2D-magnitude shape sniff we don't need
+    because saved magnitudes are 1D)."""
+    return "dora-rows" if adapter_type == "dora" else adapter_type
+
+
+# ── public entry point ─────────────────────────────────────────────────────────
+
+def merge_loras_into_weights(weights: dict, lora_paths, strength: float = 1.0,
+                             log=lambda _m: None) -> dict:
+    """Merge one or more LoRA adapters into ``weights`` in place.
+
+    ``weights`` is the DiT weight dict as loaded from the npz (str → mx.array).
+    ``strength`` is the application weight applied to every adapter's delta (the
+    `--lora-strength` knob; matches ``application_weight`` in
+    ``merge_loras_into_base_model``). Deltas are accumulated against the original
+    weight, then applied once, so stacking is order-independent for linear LoRA.
+
+    Returns a stats dict ``{"merged": int, "skipped": list[str], "adapters": int}``.
+    """
+    if not lora_paths:
+        return {"merged": 0, "skipped": [], "adapters": 0}
+
+    parsed = []
+    for raw in lora_paths:
+        path = _resolve_path(raw)
+        adapter_type, scaling, layers = _parse_adapter(path)
+        parsed.append((path, adapter_type, scaling, layers))
+        log(f"lora: {os.path.basename(path)} — {adapter_type}, "
+            f"scaling={scaling:.3f}, {len(layers)} target layers")
+
+    # Accumulate deltas per npz key against the *original* weight.
+    deltas: dict[str, np.ndarray] = {}
+    skipped: list[str] = []
+    for path, adapter_type, scaling, layers in parsed:
+        need = _PARAMS_FOR.get(adapter_type, ())
+        for layer, params in layers.items():
+            key = _layer_to_npz_key(layer)
+            if key not in weights:
+                skipped.append(layer)
+                continue
+            missing = [n for n in need if n not in params]
+            if missing:
+                raise LoraError(f"{layer}: adapter is {adapter_type} but missing {missing}")
+            W0, restore = _weight_as_2d(weights[key])
+            merged = _merged_weight(W0, params, adapter_type, scaling)
+            delta = strength * (merged - W0)
+            deltas[key] = deltas.get(key, 0.0) + delta
+            # stash the layout restorer with the key (same for repeats)
+            deltas.setdefault(key + "\0restore", restore)
+
+    merged_count = 0
+    for key, delta in list(deltas.items()):
+        if key.endswith("\0restore"):
+            continue
+        restore = deltas[key + "\0restore"]
+        W0, _ = _weight_as_2d(weights[key])
+        weights[key] = mx.array(restore(W0 + delta))
+        merged_count += 1
+
+    if skipped:
+        log(f"lora: skipped {len(skipped)} layer(s) not in this DiT "
+            f"(e.g. {skipped[0]})")
+    return {"merged": merged_count, "skipped": skipped, "adapters": len(parsed)}
+
+
+def _weight_as_2d(arr):
+    """Return ``(W2d, restore)`` where ``W2d`` is the PyTorch-layout 2D weight
+    (fan_out, fan_in) as numpy float32, and ``restore(W2d)`` rebuilds the MLX
+    layout. Linear weights are already 2D == PyTorch layout; Conv1d weights are
+    stored MLX-style (out, k, in) and round-trip through PyTorch (out, in, k)."""
+    np_arr = _np(arr)
+    if np_arr.ndim == 2:
+        return np_arr, lambda w: w.astype(np.float32)
+    if np_arr.ndim == 3:
+        out, k, cin = np_arr.shape
+        w2d = np_arr.transpose(0, 2, 1).reshape(out, cin * k)  # (out, in*k), PyTorch order
+
+        def restore(w):
+            return w.reshape(out, cin, k).transpose(0, 2, 1).astype(np.float32)
+
+        return w2d, restore
+    raise LoraError(f"unexpected weight rank {np_arr.ndim} for a LoRA target")
```
