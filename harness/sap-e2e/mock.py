"""Loopback-only scripted Anthropic/Responses upstream, including RFC 6455 frames."""
import base64
import hashlib
import json
import struct
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def events(items):
    return ''.join(f'event: {x["type"]}\ndata: {json.dumps(x)}\n\n' for x in items).encode()


def codex_quota():
    reset = int(time.time()) + 3600
    return {'x-codex-allowed': 'false', 'x-codex-limit-reached': 'true',
            'x-codex-primary-used-percent': '100', 'x-codex-primary-window-minutes': '300',
            'x-codex-primary-reset-at': str(reset), 'x-codex-primary-reset-after-seconds': '3600'}


def codex_error():
    return dict(type='usage_limit_reached', message='Local scripted usage limit reached',
                plan_type='plus', resets_at=int(time.time()) + 3600, resets_in_seconds=3600)


class Mock(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, evidence, port=0):
        super().__init__(('127.0.0.1', port), Handler)
        self.evidence = evidence
        self.lock = threading.Lock()
        self.records = []
        self.scenario = 'startup'
        self.exhausted = set()
        self.exhaust_next_codex = False
        self.tool_sent = False
        self.force_continue_400 = False
        self.close_next = False
        self.codex_tool = False

    def record(self, handler, body, transport, status, action="reply"):
        # Never persist authentication headers or conversation content. Account labels
        # are derived only from the four literal fake keys created by run.py.
        key = handler.headers.get('x-api-key', handler.headers.get('Authorization', '').removeprefix('Bearer '))
        account = key if key in {'fake-claude-a', 'fake-claude-b', 'fake-codex-a', 'fake-codex-b'} else 'unknown'
        headers = {k.lower(): v for k, v in handler.headers.items()
                   if k.lower().startswith(('x-claude-', 'x-codex-', 'anthropic-beta'))
                   or k.lower() in {'thread-id', 'session-id', 'openai-beta', 'user-agent'}}
        record = dict(scenario=self.scenario, transport=transport, path=handler.path,
                      action=action, connection=getattr(handler, 'connection_id', None),
                      account=account, status=status, headers=headers,
                      fields=sorted(body), thread=body.get('thread'),
                      previous_response_id=body.get('previous_response_id'),
                      generate=body.get('generate'),
                      message_count=len(body.get('messages', [])),
                      input_count=len(body.get('input', [])),
                      tools=[t.get('name') for t in body.get('tools', [])],
                      time=time.time())
        with self.lock:
            self.records.append(record)
            with self.evidence.open('a') as f:
                f.write(json.dumps(record) + '\n')
        return account


class Handler(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def log_message(self, *_):
        pass

    def reply(self, status, payload, headers=None):
        self.send_response(status)
        for k, v in (headers or {}).items():
            self.send_header(k, str(v))
        self.send_header('Content-Length', str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)
        self.wfile.flush()

    def handle(self):
        try:
            super().handle()
        except (ConnectionResetError, BrokenPipeError):
            pass

    def do_CONNECT(self):
        # This is also the deny proxy for any accidental non-loopback request.
        self.reply(403, b'External network forbidden by sap-e2e')

    def do_GET(self):
        if self.headers.get('Upgrade', '').lower() == 'websocket':
            return self.websocket()
        if self.path == '/sap/health':
            now = time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())
            health = dict(node='mock', role='primary', state='serving', version='sap-e2e-mock',
                          now=now, started_at=now, upstream=dict(claude=True, codex=True),
                          open_streams=0, sync={})
            return self.reply(200, json.dumps(health).encode(), {'Content-Type': 'application/json'})
        self.reply(200, b'{"data":[],"models":[]}', {'Content-Type': 'application/json'})

    def do_POST(self):
        size = int(self.headers.get('Content-Length', 0))
        body = json.loads(self.rfile.read(size) or b'{}')
        key = self.headers.get('x-api-key', self.headers.get('Authorization', '').removeprefix('Bearer '))
        if self.server.exhaust_next_codex and 'responses' in self.path:
            self.server.exhausted.add(key)
            self.server.exhaust_next_codex = False
        status = 429 if key in self.server.exhausted or 'unknown' in self.server.exhausted else 200
        if self.server.force_continue_400 and body.get('thread', {}).get('type') == 'continue':
            status = 400
            self.server.force_continue_400 = False
        self.server.record(self, body, 'http', status)
        if status != 200:
            claude = 'messages' in self.path
            error = {'type': 'rate_limit_error' if status == 429 else 'invalid_request_error',
                     'code': 'limit_reached', 'message': 'Local scripted exhaustion or missing thread'}
            headers = {'Content-Type': 'application/json'}
            if claude and status == 429:
                headers.update({'anthropic-ratelimit-unified-status': 'rejected',
                                'anthropic-ratelimit-unified-5h-status': 'rejected',
                                'anthropic-ratelimit-unified-5h-reset': str(int(time.time()) + 3600),
                                'anthropic-ratelimit-unified-representative-claim': 'five_hour'})
            elif status == 429:
                headers.update(codex_quota())
                error = codex_error()
            return self.reply(status, json.dumps({'type': 'error', 'error': error}).encode(), headers)
        if 'count_tokens' in self.path:
            return self.reply(200, b'{"input_tokens":10}', {'Content-Type': 'application/json'})
        if 'messages' in self.path:
            items, message = self.claude(body)
            return self.reply(200, events(items) if body.get('stream') else json.dumps(message).encode(),
                              {'Content-Type': 'text/event-stream' if body.get('stream') else 'application/json'})
        if 'responses' in self.path:
            return self.reply(200, events(self.response_events(body)), {'Content-Type': 'text/event-stream'})
        self.reply(200, b'{}', {'Content-Type': 'application/json'})

    def claude(self, body):
        mid = 'msg_local_' + str(len(self.server.records))
        block = {'type': 'text', 'text': 'Local mock answer.'}
        stop = 'end_turn'
        # Exercise a real child agent if the client advertises Agent/Task. Otherwise
        # perform a harmless tool call. Only one tool turn per harness run.
        tools = body.get('tools', [])
        if not self.server.tool_sent and tools:
            tool = next((t for t in tools if t.get('name', '').split('__')[-1] in ('Agent', 'Task')), None)
            if tool:
                block = dict(type='tool_use', id='tool_local_agent', name=tool['name'],
                             input={'description': 'Local mock child', 'prompt': 'Say local.',
                                    'subagent_type': 'general-purpose'})
                stop = 'tool_use'
                self.server.tool_sent = True
        message = dict(id=mid, type='message', role='assistant', model=body.get('model'),
                       content=[block], stop_reason=stop, stop_sequence=None,
                       usage={'input_tokens': 10, 'output_tokens': 4})
        start = {**message, 'content': [], 'stop_reason': None}
        if block['type'] == 'tool_use':
            initial = {**block, 'input': {}}
            delta = {'type': 'input_json_delta', 'partial_json': json.dumps(block['input'])}
        else:
            initial = {'type': 'text', 'text': ''}
            delta = {'type': 'text_delta', 'text': block['text']}
        return [dict(type='message_start', message=start),
                dict(type='content_block_start', index=0, content_block=initial),
                dict(type='content_block_delta', index=0, delta=delta),
                dict(type='content_block_stop', index=0),
                dict(type='message_delta', delta={'stop_reason': stop, 'stop_sequence': None}, usage={'output_tokens': 4}),
                dict(type='message_stop')], message

    def response_events(self, body):
        rid = 'resp_local_' + str(len(self.server.records))
        item = dict(id='item_' + rid, type='message', role='assistant', status='completed',
                    content=[dict(type='output_text', text='Local mock answer.', annotations=[])])
        response = dict(id=rid, object='response', created_at=int(time.time()), status='completed',
                        model=body.get('model', 'gpt-5.4'), output=[item],
                        usage=dict(input_tokens=10, output_tokens=4, total_tokens=14,
                                   input_tokens_details={'cached_tokens': 0}))
        if self.server.codex_tool and body.get('generate') is not False:
            self.server.codex_tool = False
            self.server.close_next = True
            item = dict(id='tool_local', type='function_call', call_id='call_local', name='exec_command',
                        arguments=json.dumps({'cmd': 'printf local'}), status='completed')
            response['output'] = [item]
            return [dict(type='response.created', response={**response, 'status': 'in_progress', 'output': []}),
                    dict(type='response.output_item.added', output_index=0, item=item),
                    dict(type='response.output_item.done', output_index=0, item=item),
                    dict(type='response.completed', response=response)]
        return [dict(type='response.created', response={**response, 'status': 'in_progress', 'output': []}),
                dict(type='response.output_item.added', output_index=0, item={**item, 'content': []}),
                dict(type='response.content_part.added', item_id=item['id'], output_index=0, content_index=0,
                     part=dict(type='output_text', text='', annotations=[])),
                dict(type='response.output_text.delta', item_id=item['id'], output_index=0, content_index=0,
                     delta='Local mock answer.'),
                dict(type='response.output_text.done', item_id=item['id'], output_index=0, content_index=0,
                     text='Local mock answer.'),
                dict(type='response.output_item.done', output_index=0, item=item),
                dict(type='response.completed', response=response)]

    def websocket(self):
        key = self.headers['Sec-WebSocket-Key']
        accept = base64.b64encode(hashlib.sha1((key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
        self.send_response(101)
        self.send_header('Upgrade', 'websocket')
        self.send_header('Connection', 'Upgrade')
        self.send_header('Sec-WebSocket-Accept', accept)
        self.end_headers()
        with self.server.lock:
            self.connection_id = len(self.server.records)
        self.connection.settimeout(30)
        pending = bytearray()
        try:
            while True:
                first, second = self.read_exact(2)
                length = second & 127
                if length == 126:
                    length = struct.unpack('!H', self.read_exact(2))[0]
                elif length == 127:
                    length = struct.unpack('!Q', self.read_exact(8))[0]
                if length > 32 * 1024 * 1024:
                    raise ValueError('oversize WebSocket frame')
                mask = self.read_exact(4) if second & 128 else None
                payload = self.read_exact(length)
                if mask:
                    payload = bytes(v ^ mask[i % 4] for i, v in enumerate(payload))
                opcode = first & 15
                if opcode == 8:
                    self.frame(8, payload)
                    break
                if opcode == 9:
                    self.frame(10, payload)
                    continue
                if opcode not in (0, 1):
                    continue
                pending.extend(payload)
                if not first & 128:
                    continue
                body = json.loads(pending)
                pending.clear()
                key = self.headers.get('Authorization', '').removeprefix('Bearer ')
                if self.server.exhaust_next_codex and body.get('generate') is not False:
                    self.server.exhausted.add(key)
                    self.server.exhaust_next_codex = False
                exhausted = body.get('generate') is not False and (key in self.server.exhausted or 'unknown' in self.server.exhausted)
                close = self.server.close_next and body.get('generate') is not False
                self.server.record(self, body, 'ws', 429 if exhausted else 200,
                                   'rate_limits+error+close' if exhausted else 'close' if close else 'reply')
                if exhausted:
                    self.frame(1, json.dumps(dict(type='codex.rate_limits', plan_type='plus',
                        rate_limits=dict(allowed=False, limit_reached=True, primary=dict(used_percent=100,
                            window_minutes=300, reset_at=int(time.time()) + 3600,
                            reset_after_seconds=3600)))).encode())
                    self.frame(1, json.dumps(dict(type='error', status=429,
                               headers=codex_quota(), error=codex_error())).encode())
                    self.frame(8, struct.pack('!H', 1000))
                    break
                if self.server.close_next and body.get('generate') is not False:
                    self.server.close_next = False
                    self.frame(8, struct.pack('!H', 1000))
                    break
                for event in self.response_events(body):
                    self.frame(1, json.dumps(event).encode())
        except (OSError, EOFError):
            pass
        self.close_connection = True

    def read_exact(self, count):
        result = self.rfile.read(count)
        if len(result) != count:
            raise EOFError()
        return result

    def frame(self, opcode, data):
        length = len(data)
        head = bytes([128 | opcode])
        head += bytes([length]) if length < 126 else b'\x7e' + struct.pack('!H', length) if length < 65536 else b'\x7f' + struct.pack('!Q', length)
        self.wfile.write(head + data)
        self.wfile.flush()


def main():
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port', type=int, default=0)
    parser.add_argument('--script', type=str, help='JSON with exhausted, close_next, force_continue_400')
    parser.add_argument('--evidence', type=str, required=True)
    args = parser.parse_args()
    from pathlib import Path
    server = Mock(Path(args.evidence), args.port)
    if args.script:
        with open(args.script) as handle:
            script = json.load(handle)
        server.exhausted = set(script.get('exhausted', []))
        server.close_next = bool(script.get('close_next', False))
        server.force_continue_400 = bool(script.get('force_continue_400', False))
    print(f'http://127.0.0.1:{server.server_port}', flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == '__main__':
    main()
