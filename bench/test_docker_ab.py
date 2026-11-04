# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors

import contextlib
import io
import unittest
from unittest.mock import patch
import docker_ab


class Response:
    status = 200

    def getheader(self, name):
        return "busbar;dur=0.5"

    def read(self):
        return b"{}"


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


if __name__ == "__main__":
    unittest.main()
