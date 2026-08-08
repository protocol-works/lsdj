import re
from pathlib import Path

import pytest


BACKEND_ROOT = Path(__file__).parents[1]
LOCKS = {
    "linux": BACKEND_ROOT / "runtime-locks/mrt2-pytorch-linux-x86_64.txt",
    "windows": BACKEND_ROOT / "runtime-locks/mrt2-pytorch-windows-x86_64.txt",
}
DIRECT_PINS = {
    "huggingface-hub": "1.5.0",
    "numpy": "2.3.5",
    "resampy": "0.4.3",
    "safetensors": "0.7.0",
    "sentencepiece": "0.2.1",
    "torch": "2.12.1+cu130",
    "transformers": "5.8.0",
}
REQUIREMENT = re.compile(r"^([a-z0-9][a-z0-9_.-]*)==([^ \\]+) \\$", re.MULTILINE)


def _requirements(text: str) -> dict[str, str]:
    return dict(REQUIREMENT.findall(text))


@pytest.mark.parametrize("platform", LOCKS)
def test_target_runtime_lock_is_immutable_and_hashed(platform):
    text = LOCKS[platform].read_text()
    requirements = _requirements(text)

    assert requirements
    assert DIRECT_PINS.items() <= requirements.items()
    assert "git+" not in text
    assert "http://" not in text
    assert " @ " not in text
    assert "--editable" not in text
    assert text.count("--hash=sha256:") >= len(requirements)

    for match in REQUIREMENT.finditer(text):
        next_requirement = REQUIREMENT.search(text, match.end())
        block_end = len(text) if next_requirement is None else next_requirement.start()
        assert "--hash=sha256:" in text[match.end() : block_end]


def test_target_locks_capture_platform_specific_dependency_graphs():
    linux = _requirements(LOCKS["linux"].read_text())
    windows = _requirements(LOCKS["windows"].read_text())

    assert linux["triton"] == "3.7.1"
    assert "triton" not in windows
    assert windows["colorama"] == "0.4.6"
    assert "colorama" not in linux
