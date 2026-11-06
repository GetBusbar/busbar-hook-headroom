# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class SmokeTests(unittest.TestCase):
    def run_smoke(self, tokens, **failures):
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
                    'if [ "$1" = "inspect" ]; then printf "%s\\n" "${BUSBAR_RUNNING:-true}"; fi',
                    'if [ "$1" = "logs" ]; then printf "busbar log tail for %s\\n" "$2"; fi',
                    'if [[ "$*" == *"--validate"* && -n "$VALIDATE_FAILS" ]]; then exit 1; fi',
                    'if [[ "$*" == *"/stats"* ]]; then',
                    '  if [ -n "$MOCK_DOWN" ]; then exit 1; fi',
                    '  printf "%s\\n" "$MOCK_TOKENS"',
                    'fi',
                    'exit 0',
                ]),
                "curl": '\n'.join([
                    '#!/bin/bash',
                    'if [[ "$*" == *"/healthz"* && -n "$HEALTHZ_DOWN" ]]; then exit 7; fi',
                    'if [[ "$*" == *"/v1/chat/completions"* && -n "$REQUEST_FAILS" ]]; then exit 22; fi',
                    'if [[ "$*" == *"/v1/chat/completions"* && -n "$UPSTREAM_HANGS" ]]; then',
                    '  if [[ "$*" == *"--max-time"* ]]; then exit 28; fi',
                    '  exec /bin/sleep 12',
                    'fi',
                    'exit 0',
                ]),
                "sleep": '#!/bin/bash\nexit 0\n',
            }
            for name, content in stubs.items():
                command = commands / name
                command.write_text(content)
                command.chmod(0o755)
            environment = dict(os.environ, PATH=f"{commands}:{os.environ['PATH']}",
                               MOCK_TOKENS=str(tokens), **failures)
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

    def test_an_unreadable_token_tally_cannot_pass(self):
        for reading in ["", "n/a", "12.5"]:
            with self.subTest(reading=reading):
                result = self.run_smoke(reading)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertNotIn("PASS", result.stdout)

    def test_the_threshold_sits_between_compressed_and_uncompressed(self):
        self.assertEqual(self.run_smoke(6999).returncode, 0)
        self.assertNotEqual(self.run_smoke(7000).returncode, 0)

    def test_a_hung_request_fails_instead_of_stalling_the_gate(self):
        result = self.run_smoke(4000, UPSTREAM_HANGS="1")
        self.assertNotEqual(result.returncode, 0)

    def test_a_failed_request_reports_and_dumps_busbar_logs(self):
        result = self.run_smoke(4000, REQUEST_FAILS="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("FAIL", result.stdout)
        self.assertIn("compressible request", result.stdout)
        self.assertIn("busbar log tail for smoke-busbar", result.stdout)

    def test_a_rejected_plugin_fails_at_validate(self):
        result = self.run_smoke(4000, VALIDATE_FAILS="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("busbar --validate rejected", result.stdout)

    def test_a_crashed_busbar_fails_before_traffic(self):
        result = self.run_smoke(4000, BUSBAR_RUNNING="false")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("crashed on startup", result.stdout)

    def test_a_mock_upstream_that_never_answers_fails_loudly(self):
        result = self.run_smoke(4000, MOCK_DOWN="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("mock upstream never answered", result.stdout)

    def test_a_busbar_that_never_turns_healthy_fails_loudly(self):
        result = self.run_smoke(4000, HEALTHZ_DOWN="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("never answered /healthz", result.stdout)


if __name__ == "__main__":
    unittest.main()
