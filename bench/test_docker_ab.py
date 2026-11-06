# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors

import argparse
import contextlib
import io
import subprocess
import sys
import unittest
from unittest.mock import patch
import docker_ab


class Response:
    status = 200
    body = b"{}"

    def getheader(self, name):
        return "busbar;dur=0.5" if name.lower() == "server-timing" else None

    def read(self):
        return self.body


class Connection:
    def __init__(self, *args, **kwargs):
        pass

    def request(self, *args, **kwargs):
        pass

    def getresponse(self):
        return Response()

    def close(self):
        pass


class LoadTests(unittest.TestCase):
    @patch.object(docker_ab.http.client, "HTTPConnection", Connection)
    def test_remainder_requests_are_not_dropped(self):
        self.assertEqual(len(docker_ab.load(b"{}", 7, 3, 0)), 7)
        self.assertEqual(len(docker_ab.load(b"{}", 2, 4, 0)), 2)

    @patch.object(docker_ab.http.client, "HTTPConnection", Connection)
    def test_http_errors_fail_the_measurement(self):
        with patch.object(Response, "status", 500):
            with self.assertRaises(RuntimeError):
                docker_ab.load(b"{}", 2, 2, 0)

    @patch.object(docker_ab.http.client, "HTTPConnection", Connection)
    def test_a_failed_request_reports_the_error_body(self):
        with patch.object(Response, "status", 502), \
             patch.object(Response, "body", b'{"error": "plugin failed to open"}'):
            with self.assertRaises(RuntimeError) as error:
                docker_ab.load(b"{}", 1, 1, 0)
        self.assertIn("plugin failed to open", str(error.exception))

    @patch.object(docker_ab.http.client, "HTTPConnection", Connection)
    def test_missing_timing_fails_the_measurement(self):
        with patch.object(Response, "getheader", return_value=None):
            with self.assertRaises(RuntimeError):
                docker_ab.load(b"{}", 1, 1, 0)

    def test_invalid_counts_fail_before_connections(self):
        for n, concurrency, warmup in [(0, 1, 0), (1, 0, 0), (1, 1, -1)]:
            with self.assertRaises(ValueError):
                docker_ab.load(b"{}", n, concurrency, warmup)

    @patch.object(docker_ab.http.client, "HTTPConnection", Connection)
    def test_failed_requests_close_the_connection(self):
        with patch.object(Response, "status", 500), patch.object(Connection, "close") as close:
            with self.assertRaises(RuntimeError):
                docker_ab.load(b"{}", 1, 1, 0)
            self.assertEqual(close.call_count, 2)

    def test_default_busbar_checkout_is_sibling(self):
        with patch.dict(docker_ab.os.environ, {}, clear=True), \
             patch.object(docker_ab, "REPO", "/workspace/headroom"), \
             patch.object(docker_ab.os.path, "isfile", return_value=True):
            self.assertEqual(docker_ab.busbar_dir(), "/workspace/busbar")

    def test_invalid_cli_arguments_fail_before_building(self):
        with patch.object(docker_ab.sys, "argv", ["docker_ab.py", "--requests", "0"]), \
             patch.object(docker_ab, "BUSBAR_IMAGE", "busbar:test"), \
             patch.object(docker_ab, "prep_plugin") as prepare, \
             patch.object(docker_ab, "start_mock"), \
             patch.object(docker_ab, "measure", side_effect=AssertionError("measurement started")), \
             patch.object(docker_ab, "rm"), \
             contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as error:
                docker_ab.main()
            self.assertEqual(error.exception.code, 2)
            prepare.assert_not_called()

    def test_image_must_be_selected_before_building(self):
        with patch.object(docker_ab.sys, "argv", ["docker_ab.py"]), \
             patch.object(docker_ab, "BUSBAR_IMAGE", None), \
             patch.object(docker_ab, "prep_plugin") as prepare, \
             patch.object(docker_ab, "start_mock"), \
             patch.object(docker_ab, "measure", side_effect=AssertionError("measurement started")), \
             patch.object(docker_ab, "rm"), \
             contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as error:
                docker_ab.main()
            self.assertEqual(error.exception.code, 2)
            prepare.assert_not_called()


class HarnessTests(unittest.TestCase):
    def test_a_failing_quiet_command_reports_its_error_output(self):
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            with self.assertRaises(subprocess.CalledProcessError):
                docker_ab.sh(sys.executable, "-c", "import sys; sys.stderr.write('port is already allocated'); sys.exit(3)")
        self.assertIn("port is already allocated", stderr.getvalue())

    def test_a_failing_mock_control_call_reports_its_error_output(self):
        failed = subprocess.CompletedProcess([], 1, stdout="", stderr="Error: No such container: hb-mock")
        with patch.object(docker_ab.subprocess, "run", return_value=failed):
            with self.assertRaises(RuntimeError) as error:
                docker_ab.mock_get("/stats")
        self.assertIn("No such container", str(error.exception))

    def test_the_mock_is_polled_until_it_answers(self):
        answers = [RuntimeError("connection refused"), RuntimeError("connection refused"), {"requests": 0}]
        with patch.object(docker_ab, "sh"), patch.object(docker_ab, "rm"), \
             patch.object(docker_ab, "mock_get", side_effect=answers) as probe, \
             patch.object(docker_ab.time, "sleep"):
            docker_ab.start_mock(0)
        self.assertEqual(probe.call_count, 3)

    def test_a_mock_that_never_answers_stops_the_run(self):
        with patch.object(docker_ab, "sh"), patch.object(docker_ab, "rm"), \
             patch.object(docker_ab, "mock_get", side_effect=RuntimeError("connection refused")), \
             patch.object(docker_ab.time, "sleep"):
            with self.assertRaises(RuntimeError):
                docker_ab.start_mock(0)

    def test_a_quiet_command_that_succeeds_stays_quiet(self):
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            docker_ab.sh(sys.executable, "-c", "import sys; sys.stderr.write('noise')")
        self.assertEqual(stderr.getvalue(), "")

    def test_a_phase_whose_requests_never_reached_the_mock_fails(self):
        args = argparse.Namespace(history_kb=1, requests=1, concurrency=1, warmup=0)
        with patch.object(docker_ab, "start_busbar"), \
             patch.object(docker_ab, "load", return_value=[500]), \
             patch.object(docker_ab, "mock_get", side_effect=[{}, {"requests": 0, "prompt_tokens_est": 0}]):
            with self.assertRaises(RuntimeError):
                docker_ab.measure("config.hook.yaml", True, args)

    def test_a_phase_reports_tokens_per_request(self):
        args = argparse.Namespace(history_kb=1, requests=2, concurrency=1, warmup=0)
        with patch.object(docker_ab, "start_busbar"), \
             patch.object(docker_ab, "load", return_value=[500, 700]), \
             patch.object(docker_ab, "mock_get", side_effect=[{}, {"requests": 4, "prompt_tokens_est": 4000}]):
            self.assertEqual(docker_ab.measure("config.hook.yaml", True, args)["tokens_per_req"], 1000)

    def test_the_pack_carries_the_signed_declares_section(self):
        calls = []
        with patch.object(docker_ab, "sh", side_effect=lambda *a, **k: calls.append(a)), \
             patch.object(docker_ab, "busbar_dir", return_value="/busbar"), \
             patch.object(docker_ab.os.path, "exists", return_value=True), \
             patch.object(docker_ab.shutil, "rmtree"), patch.object(docker_ab.os, "makedirs"), \
             contextlib.redirect_stderr(io.StringIO()):
            docker_ab.prep_plugin()
        pack = next(c for c in calls if len(c) > 1 and c[1] == "pack")
        self.assertIn("--declares-file", pack)
        self.assertEqual(pack[pack.index("--declares-file") + 1],
                         docker_ab.os.path.join(docker_ab.REPO, "declares.json"))
        # The tarball name docker-smoke.sh and the --skip-plugin-build path look for.
        self.assertEqual(pack[pack.index("--out") + 1],
                         docker_ab.os.path.join(docker_ab.PLUGIN_DIR, "busbar-hook-headroom.tar.gz"))

    def test_a_cargo_manifest_without_a_version_stops_the_pack(self):
        self.assertEqual(docker_ab.package_version('[package]\nname = "x"\nversion = "2.0.22"\n'), "2.0.22")
        with self.assertRaises(SystemExit):
            docker_ab.package_version('[package]\nname = "x"\n')


if __name__ == "__main__":
    unittest.main()
