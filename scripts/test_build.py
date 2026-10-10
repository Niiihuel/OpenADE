"""Safety checks for obsolete build cleanup; run with python3 -B scripts/test_build.py."""

import io
import json
import os
from pathlib import Path
import tempfile
import time
import unittest
from unittest.mock import patch

import build


class CleanupTests(unittest.TestCase):
    def test_desktop_cleanup_preserves_the_development_profile(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp)
            desktop = target / "desktop"
            deps = desktop / "deps"
            deps.mkdir(parents=True)
            debug = target / "debug" / "deps"
            debug.mkdir(parents=True)
            keep = deps / "libopenide_ui-0123456789abcdef.rlib"
            old = deps / "libopenide_ui-fedcba9876543210.rlib"
            dev = debug / old.name
            binary = desktop / "agents-window"
            for path in [keep, old, dev, binary]:
                path.write_bytes(b"artifact")
                os.utime(path, ns=(1, 1))
            manifest = json.dumps({
                "reason": "compiler-artifact", "filenames": [str(keep), str(binary)],
                "target": {"name": "agents-window"}, "executable": str(binary),
            }) + "\n"
            with patch.object(build, "TARGET", target), \
                 patch.object(build.sys, "argv", ["build.py"]), \
                 patch.object(build.subprocess, "Popen") as popen, \
                 patch.object(build.sys, "stdout", io.StringIO()):
                popen.return_value.stdout = io.StringIO(manifest)
                popen.return_value.wait.return_value = 0
                self.assertEqual(build.main(), 0)
            self.assertTrue(keep.exists())
            self.assertTrue(binary.exists())
            self.assertTrue(dev.exists())
            self.assertFalse(old.exists())

    def test_current_graph_survives_and_old_builds_are_removed(self):
        with tempfile.TemporaryDirectory() as tmp:
            deps = Path(tmp)
            current = "libc-0123456789abcdef"
            retained = ["lib" + current + ".rlib", "lib" + current + ".rmeta", current + ".d"]
            old = ["liblibc-fedcba9876543210.rlib", "libc-fedcba9876543210.d",
                   "agents_window-fedcba9876543210"]
            for name in retained + old:
                path = deps / name
                path.write_bytes(b"artifact")
                os.utime(path, ns=(1, 1))
            count, size = build.prune(deps, {current}, time.time_ns())
            self.assertEqual((count, size), (3, 24))
            self.assertTrue(all((deps / name).exists() for name in retained))
            self.assertTrue(all(not (deps / name).exists() for name in old))

    def test_newer_files_unknown_files_and_symlinks_survive(self):
        with tempfile.TemporaryDirectory() as tmp:
            deps = Path(tmp)
            unknown = deps / "notes.txt"
            unknown.write_text("keep")
            newer = deps / "libnewer-0123456789abcdef.rlib"
            newer.write_text("keep")
            started = time.time_ns()
            os.utime(newer, ns=(started + 1, started + 1))
            linked = deps / "liblinked-fedcba9876543210.rlib"
            linked.symlink_to(unknown)
            self.assertEqual(build.prune(deps, set(), started), (0, 0))
            self.assertTrue(linked.is_symlink())

    def test_library_prefix_is_not_confused_with_crate_name(self):
        for crate in ["libc", "libloading", "openide_core"]:
            unit = crate + "-0123456789abcdef"
            for name in ["lib" + unit + ".rlib", "lib" + unit + ".so", unit + ".d"]:
                self.assertEqual(build.artifact_unit(name), unit)

    def test_failed_build_never_prunes(self):
        with tempfile.TemporaryDirectory() as tmp:
            with patch.object(build, "TARGET", Path(tmp)), \
                 patch.object(build.sys, "argv", ["build.py"]), \
                 patch.object(build.subprocess, "Popen") as popen, \
                 patch.object(build, "prune") as prune:
                popen.return_value.stdout = io.StringIO("")
                popen.return_value.wait.return_value = 101
                self.assertEqual(build.main(), 101)
                prune.assert_not_called()

    def test_missing_manifest_never_prunes(self):
        with tempfile.TemporaryDirectory() as tmp:
            with patch.object(build, "TARGET", Path(tmp)), \
                 patch.object(build.sys, "argv", ["build.py"]), \
                 patch.object(build.subprocess, "Popen") as popen, \
                 patch.object(build, "prune") as prune, \
                 patch.object(build.sys, "stderr", io.StringIO()):
                popen.return_value.stdout = io.StringIO("")
                popen.return_value.wait.return_value = 0
                self.assertEqual(build.main(), 1)
                prune.assert_not_called()


if __name__ == "__main__":
    unittest.main()
