# Stable Audio 3 pin provenance and audit

`sa3-pin.json` is executable supply-chain policy for the in-app installer, not
just version documentation. Every URL is HTTPS, every revision is immutable,
and every byte-bearing artifact has an application-controlled SHA-256 and exact
size. A pin change must update this record in the same pull request.

## Recorded provenance (2026-08-08)

| Pin family | Immutable upstream evidence | How the manifest value was established |
| --- | --- | --- |
| SA3 source | Stability AI Git commit `0385302ea26522f00c80392c4b708df5ebf1adf5` | Streamed the exact GitHub commit archive (8,436,657 bytes) and calculated SHA-256 `6991aeedd4e8f5509b7ce76b7d9dddc43e4c6f980e81ea9b5179890b518b906f`. GitHub does not publish a signed checksum for this generated archive, so a future archive-byte change must fail closed and receive explicit review. |
| uv runtime | Astral uv release `0.11.7`, target `aarch64-apple-darwin` | The official release archive and Astral release metadata agree on 20,839,135 bytes and SHA-256 `66e37d91f839e12481d7b932a1eccbfe732560f42c1cfb89faddfa2454534ba8`. |
| Python runtime | Astral python-build-standalone release `20251007`, CPython `3.11.13`, target `aarch64-apple-darwin` | The official release archive and the download metadata embedded in pinned uv `0.11.7` agree on 18,949,778 bytes and SHA-256 `78bc6defdc1dac5bf6765c8f938e6849383dbed831ea1e2d11576a4683fb1e8c`. |
| SA3 model weights | Hugging Face repository `stabilityai/stable-audio-3-optimized` at commit `6736003cb57d06b7b1fdc36fad31b2a3709e4774` | Each of the eight manifest size/hash pairs is the immutable revision's LFS object size and SHA-256. The audit script checks metadata without downloading roughly 9 GB; `--include-model-bytes` also streams and hashes every object. |
| Python dependencies | `scripts/sa3-requirements.in` compiled by uv `0.11.7` for Python 3.11 | The committed lock contains 19 exact package versions and 282 wheel/sdist SHA-256 hashes. Installer invocation also enforces `--require-hashes --only-binary :all:` against the public PyPI index with ambient config/index variables removed. |

## Reproduce the audit

From the repository root, with network access:

```console
python3 scripts/audit-sa3-pins.py
```

This downloads and hashes about 50 MB of source/runtime archives, checks all
eight model objects against the pinned Hugging Face revision's LFS metadata, and
audits the lock structure. For a release-bound pin bump, also perform the full
model-byte audit:

```console
python3 scripts/audit-sa3-pins.py --include-model-bytes
```

Regenerate the dependency lock with the same pinned uv release and compare the
result rather than editing it by hand:

```console
uv pip compile --generate-hashes --python-version 3.11 \
  --output-file scripts/sa3-requirements.lock scripts/sa3-requirements.in
git diff --exit-code -- scripts/sa3-requirements.lock
```

Reviewers should reject any pin update whose immutable revision, exact size,
checksum, provenance source, and audit result are not all present. The runtime
installer independently rechecks the same sizes and hashes before extraction,
execution, promotion, recovery, and app-managed readiness.
