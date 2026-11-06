# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors

import http.client
import json
import threading
import unittest
from http.server import ThreadingHTTPServer

import mock_upstream


class MockUpstreamTests(unittest.TestCase):
    def setUp(self):
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), mock_upstream.Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.post("/reset", b"")

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()

    def request(self, method, path, body=None):
        connection = http.client.HTTPConnection("127.0.0.1", self.server.server_address[1], timeout=5)
        try:
            connection.request(method, path, body=body, headers={"content-type": "application/json"})
            response = connection.getresponse()
            return response.status, response.read()
        finally:
            connection.close()

    def post(self, path, body):
        return self.request("POST", path, body)

    def stats(self):
        return json.loads(self.request("GET", "/stats")[1])

    def test_a_chat_body_is_counted(self):
        body = json.dumps({"messages": [{"role": "user", "content": "x" * 40}]}).encode()
        status, _ = self.post("/v1/chat/completions", body)
        self.assertEqual(status, 200)
        self.assertEqual(self.stats(), {"requests": 1, "prompt_chars": 40, "prompt_tokens_est": 10})

    def test_content_block_text_is_counted(self):
        body = json.dumps({"messages": [
            {"role": "user", "content": [{"type": "text", "text": "y" * 6}, {"type": "image_url"}]},
            {"role": "assistant", "content": "zz"},
        ]}).encode()
        self.assertEqual(self.post("/v1/chat/completions", body)[0], 200)
        self.assertEqual(self.stats(), {"requests": 1, "prompt_chars": 8, "prompt_tokens_est": 2})

    def test_reset_zeroes_the_tally(self):
        body = json.dumps({"messages": [{"role": "user", "content": "abcd"}]}).encode()
        self.post("/v1/chat/completions", body)
        self.post("/reset", b"")
        self.assertEqual(self.stats(), {"requests": 0, "prompt_chars": 0, "prompt_tokens_est": 0})

    def test_a_body_that_is_not_a_chat_request_is_refused_and_not_counted(self):
        # A body the mock cannot read must not be booked as a tiny, perfectly compressed prompt:
        # the smoke gate would read that as "compression reached the provider".
        for body in [b"", b"not json", b"[]", b"null", b'{"messages": ["x"]}', b'{"messages": 3}',
                     b'{"messages": []}', b'{"messages": [{"role": "user", "text": "projection form"}]}']:
            with self.subTest(body=body):
                status, _ = self.post("/v1/chat/completions", body)
                self.assertEqual(status, 400)
                self.assertEqual(self.stats()["requests"], 0)
                self.assertEqual(self.stats()["prompt_tokens_est"], 0)


if __name__ == "__main__":
    unittest.main()
