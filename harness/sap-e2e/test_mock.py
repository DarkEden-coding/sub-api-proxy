"""Local protocol checks. These never launch a client or contact a provider."""
import base64
import http.client
import json
from pathlib import Path
import socket
import struct
import tempfile
import threading
import unittest

from mock import Mock


class ProtocolTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.mock = Mock(Path(self.tmp.name) / 'requests.jsonl')
        threading.Thread(target=self.mock.serve_forever, daemon=True).start()
        self.addCleanup(self.tmp.cleanup)
        self.addCleanup(self.mock.server_close)
        self.addCleanup(self.mock.shutdown)

    def post(self, path, body, key):
        client = http.client.HTTPConnection('127.0.0.1', self.mock.server_port, timeout=2)
        self.addCleanup(client.close)
        client.request('POST', path, json.dumps(body), {'Authorization': 'Bearer ' + key})
        response = client.getresponse()
        return response, response.read()

    def test_claude_tool_and_exhaustion(self):
        response, body = self.post('/v1/messages', dict(model='local', stream=True,
            messages=[], tools=[dict(name='Agent')]), 'fake-claude-a')
        self.assertEqual(response.status, 200)
        self.assertIn(b'"stop_reason": "tool_use"', body)
        self.mock.exhausted.add('fake-claude-a')
        response, body = self.post('/v1/messages', {}, 'fake-claude-a')
        self.assertEqual(response.status, 429)
        self.assertEqual(response.getheader('anthropic-ratelimit-unified-5h-status'), 'rejected')
        self.assertNotIn('authorization', self.mock.records[0]['headers'])
        self.assertNotIn('messages', self.mock.records[0])
        self.mock.exhausted.clear()
        self.mock.force_continue_400 = True
        response, _ = self.post('/v1/messages', {'thread': {'type': 'continue'}}, 'fake-claude-a')
        self.assertEqual(response.status, 400)
        response, _ = self.post('/v1/messages', {'thread': {'type': 'create'}}, 'fake-claude-a')
        self.assertEqual(response.status, 200)

    def test_credential_checks_do_not_record_tokens(self):
        token = 'sk-ant-oat01-fake-fixture'
        self.post('/v1/messages', {}, token)
        self.assertTrue(self.mock.records[-1]['client_oauth_forwarded'])
        self.assertNotIn(token, self.mock.evidence.read_text())
        self.post('/v1/messages', {}, 'fake-local-client')
        self.assertTrue(self.mock.records[-1]['router_client_key'])
        self.assertFalse(self.mock.records[-1]['client_oauth_forwarded'])
        self.assertNotIn('fake-local-client', self.mock.evidence.read_text())

    def test_responses_sse_and_websocket(self):
        response, body = self.post('/v1/responses', dict(model='local', input=[]), 'fake-codex-a')
        self.assertEqual(response.status, 200)
        self.assertIn(b'event: response.completed', body)
        self.mock.exhausted.add('fake-codex-a')
        response, body = self.post('/v1/responses', dict(model='local', input=[]), 'fake-codex-a')
        self.assertEqual(response.status, 429)
        self.assertEqual(response.getheader('x-codex-limit-reached'), 'true')
        self.assertEqual(response.getheader('x-codex-primary-used-percent'), '100')
        self.assertEqual(json.loads(body)['error']['type'], 'usage_limit_reached')
        with socket.create_connection(('127.0.0.1', self.mock.server_port), timeout=2) as sock:
            stream = sock.makefile('rb')
            self.addCleanup(stream.close)
            key = base64.b64encode(b'local-fake-nonce!').decode()
            sock.sendall(('GET /v1/responses HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n'
                          'Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\n'
                          f'Sec-WebSocket-Key: {key}\r\nAuthorization: Bearer fake-codex-a\r\n\r\n').encode())
            self.assertIn(b'101', stream.readline())
            while stream.readline() != b'\r\n':
                pass
            def receive():
                head = stream.read(2)
                size = head[1] & 127
                if size == 126:
                    size = struct.unpack('!H', stream.read(2))[0]
                return head[0] & 15, stream.read(size)
            data = b'{"type":"response.create","input":[]}'
            mask = b'abcd'
            sock.sendall(bytes([0x81, 0x80 | len(data)]) + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(data)))
            opcode, payload = receive()
            quota = json.loads(payload)
            self.assertEqual(opcode, 1)
            self.assertEqual(quota['type'], 'codex.rate_limits')
            self.assertTrue(quota['rate_limits']['limit_reached'])
            opcode, payload = receive()
            error = json.loads(payload)
            self.assertEqual(error['status'], 429)
            self.assertEqual(error['error']['type'], 'usage_limit_reached')
            self.assertEqual(receive(), (8, struct.pack('!H', 1000)))
        self.assertEqual(self.mock.records[-1]['action'], 'rate_limits+error+close')

    def test_websocket_normal_close(self):
        with socket.create_connection(('127.0.0.1', self.mock.server_port), timeout=2) as sock:
            stream = sock.makefile('rb')
            self.addCleanup(stream.close)
            key = base64.b64encode(b'local-fake-nonce!').decode()
            sock.sendall(('GET /v1/responses HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n'
                          'Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\n'
                          f'Sec-WebSocket-Key: {key}\r\nAuthorization: Bearer fake-codex-a\r\n\r\n').encode())
            self.assertIn(b'101', stream.readline())
            while stream.readline() != b'\r\n':
                pass
            self.mock.close_next = True
            data = b'{"type":"response.create","input":[]}'
            mask = b'abcd'
            sock.sendall(bytes([0x81, 0x80 | len(data)]) + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(data)))
            self.assertEqual(stream.read(2), b'\x88\x02')
            self.assertEqual(stream.read(2), struct.pack('!H', 1000))
        self.assertEqual(self.mock.records[-1]['action'], 'close')


if __name__ == '__main__':
    unittest.main()
