# Stable Audio 3 pin provenance and audit

`sa3-pin.json` and `sa3-tflite-pin.json` are executable supply-chain policy for
the in-app installer, not just version documentation. Every URL is HTTPS, every
revision is immutable, and every byte-bearing artifact has an
application-controlled SHA-256 and exact size. A pin change must update this
record in the same pull request.

## Recorded provenance (2026-08-08)

| Pin family | Immutable upstream evidence | How the manifest value was established |
| --- | --- | --- |
| SA3 source | Stability AI Git commit `a0b57f5483c4588f827f3552b7d5c6ca2a9687be` | Streamed the exact GitHub commit archive: 50,494,239 bytes, SHA-256 `98e206e061a3b64a4e65f50b2802bdb6965910ac1fab65da919808dfb4497e9f`. The 442-entry archive expands to 83,845,120 bytes. GitHub does not publish a signed checksum for generated source archives, so any byte change fails closed and requires explicit review. |
| uv runtime | Astral uv release `0.11.7` | The publisher checksums and downloaded bytes agree for `aarch64-apple-darwin` (20,839,135 bytes, `66e37d91…34ba8`), `x86_64-unknown-linux-gnu` (24,249,861 bytes, `6681d691…ea868`), and `x86_64-pc-windows-msvc` (23,572,531 bytes, `fe0c7815…a8b29`). The Windows artifact is securely extracted from ZIP; the other two are tarballs. |
| Python runtime | python-build-standalone release `20251007`, CPython `3.11.13` | Publisher metadata and downloaded bytes agree for Apple arm64 (18,949,778 bytes, `78bc6def…e8c`), Linux x64 (30,157,215 bytes, `43bfc425…f0c3`), and Windows x64 (25,990,147 bytes, `cde5153f…8b29`). |
| MLX model weights | `stabilityai/stable-audio-3-optimized` at commit `6736003cb57d06b7b1fdc36fad31b2a3709e4774` | The eight manifest size/hash pairs match the immutable revision's LFS objects. The unique model payload is 9,154,794,562 bytes. |
| TFLite model weights | The same immutable model revision | `sa3-tflite-pin.json` selects the official fp32 CPU set. Its eight unique LFS objects total 14,138,994,904 bytes. Shared SAME-S artifacts are installed once even though both small bundles reference them. |
| Python dependencies | The two `.in` files compiled by uv `0.11.7` for Python 3.11 | The MLX lock contains 19 exact packages and 282 hashes; the portable LiteRT lock contains 26 exact packages and 433 hashes. Installer invocation enforces `--require-hashes --only-binary :all:` against public PyPI with ambient index/config variables removed. |

The model manager discloses the selected backend's exact unique model payload
before installation. Source archives, Python, uv, and dependency wheels add
download and on-disk overhead beyond that displayed model-byte figure.

## Reproduce the audit

From the repository root, with network access:

```console
python3 scripts/audit-sa3-pins.py
```

This downloads and hashes the source and three host runtime pairs, checks both
sets of eight model objects against immutable Hugging Face LFS metadata, and
audits both dependency locks. For a release-bound pin bump, also perform the
full model-byte audit (roughly 23 GB):

```console
python3 scripts/audit-sa3-pins.py --include-model-bytes
```

Regenerate each dependency lock with the same pinned uv release and compare the
result instead of editing it by hand:

```console
uv pip compile --generate-hashes --python-version 3.11 \
  --output-file scripts/sa3-requirements.lock scripts/sa3-requirements.in
uv pip compile --generate-hashes --python-version 3.11 \
  --output-file scripts/sa3-tflite-requirements.lock scripts/sa3-tflite-requirements.in
git diff --exit-code -- scripts/sa3-requirements.lock scripts/sa3-tflite-requirements.lock
```

Reviewers should reject a pin update unless its immutable revision, exact size,
checksum, provenance source, and audit result are all present. The installer
independently rechecks sizes and hashes before extraction, execution, promotion,
recovery, and app-managed readiness.
