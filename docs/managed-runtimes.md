# Managed runtimes on Linux and Windows

Linux and Windows releases launch only application-managed backend generations.
Each generation is assembled in private staging, built exclusively from exact
URL, size, and SHA-256 pins, installed offline, validated, and then atomically
promoted. The prior generation remains available for rollback. At launch, LSDJ
revalidates the target, generation stamp, complete file inventory, executable,
working directory, and every file digest immediately before spawning an
absolute program with a fixed argument vector and a cleared environment.

No production path invokes a shell, searches `PATH`, or falls back to system
Python, Git, or `uv`. The managed services are `mrt2`, `sa3-tflite`, and the
reserved `sa3-pytorch-cuda` service used by the Windows CUDA backend work.

## Hugging Face access

The MRT2 snapshot pins use immutable Hugging Face revisions. LSDJ can pass an
`HF_TOKEN` or `HUGGING_FACE_HUB_TOKEN` only to the native authenticated download
request; credentials are not written to provenance, manifests, command lines,
or logs. LSDJ does not bypass repository gates or accept model terms for a user.
If an upstream repository requires authentication or acceptance of its terms,
that remains an external acquisition prerequisite and the installer fails
closed until the user has completed it.

## Launch-secret seam

The runtime manifest declares two ephemeral launch-only keys:
`LSDJ_API_CAPABILITY` and `LSDJ_WORKER_LAUNCH_TOKEN`. They are accepted only by
the structured `VerifiedCommand::into_command(extra_args, ephemeral)` boundary,
are carried in the child environment, and are excluded from fixed arguments,
static manifest values, provenance, disk, and diagnostics. Issue #130 owns the
authenticated IPC protocol and token generation/verification that populate
this seam.
