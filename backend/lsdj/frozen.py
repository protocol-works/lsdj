"""Entry point for the bundled macOS backend runtime.

The native app ships one PyInstaller ONEDIR tree rather than duplicating the
large MLX/Python dependency closure.  Deck inference and model management use
the sidecar CLI directly; the generation server adds ``--generation-server``
so this tiny dispatcher can hand the remaining arguments to the FastAPI CLI.
"""

from __future__ import annotations

import sys


def check_runtime() -> None:
    """Import the packaging-only dependency paths without loading model weights."""
    try:
        import mlx.nn  # noqa: F401 - import compiles MLX primitives / loads metallib
    except RuntimeError as error:
        # GitHub's virtualized macOS runner may expose no Metal device. Reaching
        # that precise error proves the metallib loaded; a missing metallib has a
        # different error and must still fail the release smoke check.
        if "No Metal device available" not in str(error):
            raise
    import lsdj.controller  # noqa: F401 - FastAPI/uvicorn dependency closure
    import lsdj.sidecar  # noqa: F401 - deck/model-tooling dependency closure


def main(argv: list[str] | None = None) -> None:
    args = list(sys.argv[1:] if argv is None else argv)
    if args == ["--check-runtime"]:
        check_runtime()
        return

    if "--generation-server" in args:
        args.remove("--generation-server")
        from lsdj.controller import main as controller_main

        controller_main(args)
        return

    from lsdj.sidecar import main as sidecar_main

    sidecar_main(args)


if __name__ == "__main__":
    main()
