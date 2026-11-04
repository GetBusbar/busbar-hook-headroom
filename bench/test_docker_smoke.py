# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class SmokeTests(unittest.TestCase):
    def run_smoke(self, tokens):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "scripts").mkdir()
            (root / "bench").mkdir()
            commands = root / "bin"
            commands.mkdir()
            source = Path(__file__).resolve().parents[1] / "scripts/docker-smoke.sh"
            shutil.copyfile(source, root / "scripts/docker-smoke.sh")
            stubs = {
                "python3": '\n'.join([
                    '#!/bin/bash',
                    'if [ "$1" = "-" ]; then',
                    '  cat >/dev/null',
                    '  mkdir -p "$2/plugins"',
                    '  touch "$2/plugins/busbar-hook-headroom.tar.gz"',
                    'else',
                    '  printf "{}\\n"',
                    'fi',
                ]),
                "docker": '\n'.join([
                    '#!/bin/bash',
                    'if [ "$1" = "inspect" ]; then printf "true\\n"; fi',
                    'if [[ "$*" == *"/stats"* ]]; then printf "%s\\n" "$MOCK_TOKENS"; fi',
                    'exit 0',
                ]),
                "curl": '#!/bin/bash\nexit 0\n',
                "sleep": '#!/bin/bash\nexit 0\n',
            }
            for name, content in stubs.items():
                command = commands / name
                command.write_text(content)
                command.chmod(0o755)
            environment = dict(os.environ, PATH=f"{commands}:{os.environ['PATH']}",
                               MOCK_TOKENS=str(tokens))
            return subprocess.run(
                ["bash", str(root / "scripts/docker-smoke.sh"), "mock-image"],
                env=environment, capture_output=True, text=True, timeout=10,
            )

    def test_current_artifact_name_reaches_successful_smoke(self):
        result = self.run_smoke(4000)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("PASS", result.stdout)

    def test_no_upstream_tokens_cannot_pass_as_compression(self):
        result = self.run_smoke(0)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no compression reached", result.stdout)

    def test_uncompressed_tokens_fail(self):
        result = self.run_smoke(8496)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no compression reached", result.stdout)


if __name__ == "__main__":
    unittest.main()
