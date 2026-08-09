"""LoRA registry tests: name resolution at the trust boundary.

The registry is a directory layout the Rust importer writes; here it is laid
out by hand and `resolve` — the only read the generate path performs — is
exercised against well-formed, malformed, and hostile names.
"""

import hashlib
import json
import os

import pytest

from lsdj import loras

ADAPTER = b"fake safetensors bytes"


def install_adapter(root, base, slug, filename="adapter_model.safetensors"):
    adapter_dir = root / base / slug
    adapter_dir.mkdir(parents=True)
    (adapter_dir / filename).write_bytes(ADAPTER)
    return adapter_dir


class TestLorasDir:
    def test_env_override_wins(self, tmp_path):
        assert (
            loras.loras_dir(
                env={"SA3_LORAS_HOME": str(tmp_path / "elsewhere")}, home=tmp_path
            )
            == tmp_path / "elsewhere"
        )

    def test_uses_the_host_supplied_assets_home(self, tmp_path):
        assert (
            loras.loras_dir(env={"LSDJ_ASSETS_HOME": str(tmp_path / "assets")})
            == tmp_path / "assets" / "sa3-loras"
        )

    def test_refuses_to_guess_a_platform_home(self):
        with pytest.raises(RuntimeError, match="desktop host"):
            loras.loras_dir(env={})


class TestResolve:
    def test_resolves_an_installed_adapter(self, tmp_path):
        adapter_dir = install_adapter(tmp_path, "medium", "maqam")
        resolved, base = loras.resolve(
            "medium/maqam", env={"SA3_LORAS_HOME": str(tmp_path)}, home=tmp_path
        )
        assert resolved == adapter_dir
        assert base == "medium"

    def test_resolves_a_hand_placed_safetensors_name(self, tmp_path):
        # The runtime accepts any single .safetensors in the dir; so does the
        # registry (a user may drop an adapter in by hand).
        install_adapter(tmp_path, "small", "crackle", filename="crackle.safetensors")
        _, base = loras.resolve(
            "small/crackle", env={"SA3_LORAS_HOME": str(tmp_path)}, home=tmp_path
        )
        assert base == "small"

    @pytest.mark.parametrize(
        "name",
        [
            "medium/absent",  # never installed
            "maqam",  # no base segment
            "large/maqam",  # not a known base
            "medium/../maqam",  # traversal
            "medium/.hidden",  # leading dot
            "medium/",  # empty slug
            "medium/sub/dir",  # extra separator lands in the slug check
            "MEDIUM/maqam",  # bases are exact
        ],
    )
    def test_rejects_names_that_do_not_resolve(self, tmp_path, name):
        install_adapter(tmp_path, "medium", "maqam")
        with pytest.raises(loras.UnknownAdapter):
            loras.resolve(name, env={"SA3_LORAS_HOME": str(tmp_path)}, home=tmp_path)

    def test_rejects_a_directory_without_a_safetensors(self, tmp_path):
        (tmp_path / "medium" / "empty").mkdir(parents=True)
        with pytest.raises(loras.UnknownAdapter):
            loras.resolve(
                "medium/empty", env={"SA3_LORAS_HOME": str(tmp_path)}, home=tmp_path
            )

    def test_rejects_a_directory_with_two_safetensors(self, tmp_path):
        # Ambiguous contents mirror the runtime's own refusal — better to
        # refuse at the boundary than let sa3_mlx fail mid-generation.
        adapter_dir = install_adapter(tmp_path, "medium", "both")
        (adapter_dir / "second.safetensors").write_bytes(ADAPTER)
        with pytest.raises(loras.UnknownAdapter):
            loras.resolve(
                "medium/both", env={"SA3_LORAS_HOME": str(tmp_path)}, home=tmp_path
            )

    @pytest.mark.skipif(os.name == "nt", reason="symlink creation needs privileges on Windows")
    def test_rejects_symlinked_directory_and_weights(self, tmp_path):
        outside = tmp_path / "outside"
        install_adapter(outside, "small", "real")
        (tmp_path / "small").mkdir()
        (tmp_path / "small" / "linked-dir").symlink_to(
            outside / "small" / "real", target_is_directory=True
        )
        with pytest.raises(loras.UnknownAdapter):
            loras.resolve(
                "small/linked-dir",
                env={"SA3_LORAS_HOME": str(tmp_path)},
                home=tmp_path,
            )

        linked = tmp_path / "small" / "linked-file"
        linked.mkdir()
        (linked / "adapter_model.safetensors").symlink_to(
            outside / "small" / "real" / "adapter_model.safetensors"
        )
        with pytest.raises(loras.UnknownAdapter):
            loras.resolve(
                "small/linked-file",
                env={"SA3_LORAS_HOME": str(tmp_path)},
                home=tmp_path,
            )

    def test_rejects_tamper_against_import_provenance(self, tmp_path):
        adapter_dir = install_adapter(tmp_path, "small", "sealed")
        adapter = adapter_dir / "adapter_model.safetensors"
        manifest = {
            "source": "https://huggingface.co/friend/adapter",
            "revision": "a" * 40,
            "convention": "peft",
            "adapterType": "lora",
            "rank": 8,
            "files": [
                {
                    "filename": adapter.name,
                    "size": len(ADAPTER),
                    "sha256": hashlib.sha256(ADAPTER).hexdigest(),
                }
            ],
        }
        (adapter_dir / "lora.json").write_text(json.dumps(manifest))
        loras.resolve(
            "small/sealed", env={"SA3_LORAS_HOME": str(tmp_path)}, home=tmp_path
        )
        adapter.write_bytes(b"same-size-tamper!!")
        with pytest.raises(loras.UnknownAdapter):
            loras.resolve(
                "small/sealed",
                env={"SA3_LORAS_HOME": str(tmp_path)},
                home=tmp_path,
            )
