# SPDX-License-Identifier: Apache-2.0
"""Targeted regression for prebuilding the real e2e subjects outside nextest."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location("prepare", Path(__file__).with_name("prepare.py"))
prepare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prepare)
METADATA = {"packages": [{"name": "actual-packer-owner", "targets": [
    {"name": "busbar-plugin-pack", "kind": ["bin"], "required-features": ["pack"]}]}]}


class Prepare(unittest.TestCase):
    def test_builds_locked_debug_subject_and_discovered_packer(self):
        root = Path("/admitted/host")
        commands = prepare.build_commands(METADATA, root)
        self.assertEqual(len(commands), 2)
        for command in commands:
            self.assertIn("--locked", command)
            self.assertNotIn("--release", command)
        self.assertIn(str(root / "dist/default/Cargo.toml"), commands[0])
        self.assertEqual(commands[0][-2:], ["--bin", "busbar"])
        self.assertIn("actual-packer-owner", commands[1])
        self.assertEqual(commands[1][-2:], ["--features", "pack"])

    def test_ambiguous_packer_owner_is_refused(self):
        for packages in ([], METADATA["packages"] * 2):
            with self.subTest(packages=packages), self.assertRaises(ValueError):
                prepare.build_commands({"packages": packages}, Path("/host"))

    def test_existing_binaries_still_require_cargo_source_checks(self):
        import json
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / "target"
            (target / "debug").mkdir(parents=True)
            for binary in ("busbar", "busbar-plugin-pack"):
                (target / "debug" / binary).write_text("stale executable")
            with mock.patch.dict(os.environ, {"BUSBAR_CHECKOUT": str(root), "CARGO_TARGET_DIR": str(target)}), \
                    mock.patch.object(prepare.subprocess, "run", return_value=subprocess.CompletedProcess(
                        [], 0, stdout=json.dumps(METADATA))) as run:
                prepare.main()
            self.assertEqual(run.call_count, 3)
            self.assertEqual([call.args[0] for call in run.call_args_list[1:]],
                             prepare.build_commands(METADATA, root.resolve()))
            self.assertTrue(all(call.kwargs["check"] for call in run.call_args_list))


if __name__ == "__main__":
    unittest.main()
