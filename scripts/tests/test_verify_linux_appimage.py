import importlib.util
import stat
import sys
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).parents[1] / "verify_linux_appimage.py"
SPEC = importlib.util.spec_from_file_location("verify_linux_appimage", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
verify_linux_appimage = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = verify_linux_appimage
SPEC.loader.exec_module(verify_linux_appimage)


class AppImageLayoutTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        (self.root / "AppRun").write_text("entry")
        (self.root / "lsdj-app.png").write_bytes(b"png")
        (self.root / "lsdj-app.desktop").write_text(
            "[Desktop Entry]\n"
            "Type=Application\n"
            "Name=LSDJ\n"
            "Exec=lsdj-app\n"
            "Icon=lsdj-app\n"
            "Categories=AudioVideo;Audio;\n"
        )
        binary = self.root / "usr/bin/lsdj-app"
        binary.parent.mkdir(parents=True)
        binary.write_bytes(b"elf")
        binary.chmod(binary.stat().st_mode | stat.S_IXUSR)

    def tearDown(self):
        self.temporary.cleanup()

    def test_desktop_and_binary_layout_produce_deterministic_audit(self):
        audit = verify_linux_appimage.verify_extracted(
            self.root, ["libasound.so.2", "libc.so.6"]
        )

        self.assertEqual(audit["architecture"], "x86_64")
        self.assertEqual(audit["desktop"]["name"], "LSDJ")
        self.assertEqual(audit["elfNeeded"], ["libasound.so.2", "libc.so.6"])

    def test_missing_audio_category_fails(self):
        (self.root / "lsdj-app.desktop").write_text(
            "[Desktop Entry]\nType=Application\nName=LSDJ\nExec=lsdj-app\n"
        )

        with self.assertRaisesRegex(
            verify_linux_appimage.AppImageError, "audio/music category"
        ):
            verify_linux_appimage.verify_extracted(self.root, ["libc.so.6"])

    def test_duplicate_desktop_keys_fail_closed(self):
        (self.root / "lsdj-app.desktop").write_text(
            "[Desktop Entry]\n"
            "Type=Application\nName=LSDJ\nName=Other\n"
            "Exec=lsdj-app\nCategories=Audio;\n"
        )

        with self.assertRaisesRegex(
            verify_linux_appimage.AppImageError, "duplicate desktop key"
        ):
            verify_linux_appimage.verify_extracted(self.root, ["libc.so.6"])

    def test_internal_desktop_symlink_is_accepted(self):
        desktop = self.root / "lsdj-app.desktop"
        packaged = self.root / "usr/share/applications/lsdj-app.desktop"
        packaged.parent.mkdir(parents=True)
        desktop.replace(packaged)
        try:
            desktop.symlink_to(Path("usr/share/applications/lsdj-app.desktop"))
        except OSError as error:
            self.skipTest(f"symlinks are unavailable: {error}")

        audit = verify_linux_appimage.verify_extracted(self.root, ["libc.so.6"])

        self.assertEqual(audit["desktop"]["file"], "lsdj-app.desktop")

    def test_desktop_symlink_cannot_escape_package_root(self):
        desktop = self.root / "lsdj-app.desktop"
        outside = self.root.parent / f"{self.root.name}-outside.desktop"
        desktop.replace(outside)
        self.addCleanup(outside.unlink, missing_ok=True)
        try:
            desktop.symlink_to(outside)
        except OSError as error:
            self.skipTest(f"symlinks are unavailable: {error}")

        with self.assertRaisesRegex(
            verify_linux_appimage.AppImageError, "unsafe desktop entry"
        ):
            verify_linux_appimage.verify_extracted(self.root, ["libc.so.6"])

    def test_absolute_desktop_symlink_is_rejected(self):
        desktop = self.root / "lsdj-app.desktop"
        packaged = self.root / "usr/share/applications/lsdj-app.desktop"
        packaged.parent.mkdir(parents=True)
        desktop.replace(packaged)
        try:
            desktop.symlink_to(packaged.resolve())
        except OSError as error:
            self.skipTest(f"symlinks are unavailable: {error}")

        with self.assertRaisesRegex(
            verify_linux_appimage.AppImageError, "absolute symlink"
        ):
            verify_linux_appimage.verify_extracted(self.root, ["libc.so.6"])


if __name__ == "__main__":
    unittest.main()
