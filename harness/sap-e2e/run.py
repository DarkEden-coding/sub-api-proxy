#!/usr/bin/env python3
"""Run installed clients through two local SAP nodes and a scripted upstream."""
import argparse
import base64
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid

from mock import Mock

ROOT = Path(__file__).resolve().parents[2]
KEY = 'fake-local-client'
MANAGEMENT = 'fake-local-management'


def port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


def request(base, path, body=None, key=MANAGEMENT):
    req = urllib.request.Request(base + path, data=None if body is None else json.dumps(body).encode(),
                                 headers={'Authorization': 'Bearer ' + key, 'Content-Type': 'application/json'})
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(req, timeout=3) as response:
            return response.status, json.load(response)
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode()


def wait_ready(base, process):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f'process exited {process.returncode}; inspect node/router log')
        try:
            if request(base, '/sap/health')[0] == 200:
                return
        except (OSError, ValueError):
            pass
        time.sleep(.1)
    raise RuntimeError('node did not become ready')


def api_key_config(upstream):
    providers = {}
    for provider, model, suffix in [('claude', 'claude-sonnet-4-6', ''), ('codex', 'gpt-5.4', '/v1')]:
        entries = []
        for account in ('a', 'b'):
            key = {'api-key': f'fake-{provider}-{account}'}
            if provider == 'codex':
                key.update(websockets=True, **{'disable-codex-cloaking': True})
            entries.append({'base-url': upstream + suffix, 'models': [{'name': model}], 'keys': [key]})
        providers[provider] = entries
    return providers


class Run:
    def __init__(self, output, client_only=False):
        self.output = output
        self.client_only = client_only
        self.tmp = Path(tempfile.mkdtemp(prefix='sap-e2e-'))
        self.processes = []
        self.handles = []
        self.results = []
        (output / 'run.json').write_text(json.dumps(dict(mode='router-mock-diagnostic' if client_only else 'sap-chain',
                                                        recorded_at=time.time(), commit=subprocess.check_output(
                                                        ['perl', '-e', 'alarm 10; exec @ARGV', 'git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip())) + '\n')
        self.mock = Mock(output / 'upstream.jsonl')
        threading.Thread(target=self.mock.serve_forever, daemon=True).start()
        self.upstream = f'http://127.0.0.1:{self.mock.server_port}'
        # Deny every non-loopback connection even if a CLI ignores its base URL.
        self.profile = self.tmp / 'local-only.sb'
        self.profile.write_text('(version 1) (allow default) (deny network*) '
                                '(allow network-outbound (remote ip "localhost:*")) '
                                '(allow network-inbound (local ip "localhost:*"))')
        self.env = {'PATH': os.environ['PATH'], 'HOME': str(self.tmp), 'TMPDIR': str(self.tmp),
                    'LANG': 'en_US.UTF-8', 'TERM': 'dumb',
                    'HTTP_PROXY': self.upstream, 'HTTPS_PROXY': self.upstream,
                    'ALL_PROXY': self.upstream, 'NO_PROXY': '127.0.0.1,localhost',
                    'http_proxy': self.upstream, 'https_proxy': self.upstream,
                    'all_proxy': self.upstream, 'no_proxy': '127.0.0.1,localhost'}

    def result(self, name, passed, evidence):
        entry = dict(name=name, passed=passed, status='diagnostic' if passed is None else 'pass' if passed else 'fail', evidence=evidence)
        self.results.append(entry)
        print(json.dumps(entry), flush=True)

    def start(self, command, name):
        log = (self.output / (name + '.log')).open('wb')
        self.handles.append(log)
        process = subprocess.Popen(['/usr/bin/sandbox-exec', '-f', str(self.profile), 'perl', '-e', 'alarm 240; exec @ARGV', *command],
                                   cwd=self.tmp, env=self.env, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        self.processes.append(process)
        return process

    def node(self, name):
        work = self.tmp / name
        work.mkdir()
        (work / 'auth').mkdir()
        listen = port()
        config = dict(**{'config-version': 8}, server={'host': '127.0.0.1', 'port': listen},
                      access={'api-keys': [KEY]}, oauth={'auth-dir': str(work / 'auth')},
                      management={'secret-key': MANAGEMENT, 'allow-remote': True,
                                  'disable-control-panel': True, 'disable-auto-update-panel': True},
                      requests={'proxy-url': self.upstream},
                      routing={'retry': {'request-retry': 0, 'max-retry-interval': 0}},
                      **{'api-keys': api_key_config(self.upstream)})
        (work / 'config.yaml').write_text(json.dumps(config))
        (work / 'sap.yaml').write_text(json.dumps(dict(node=name, role='primary' if name == 'primary' else 'fallback',
                                                       store_path=str(work / 'sap.sqlite'))))
        proc = self.start([str(ROOT / 'target/debug/cliproxy'), '--config', str(work / 'config.yaml'),
                           '--sap-config', str(work / 'sap.yaml')], name)
        base = f'http://127.0.0.1:{listen}'
        wait_ready(base, proc)
        return proc, base

    def client(self, name, command, env, seconds=15, diagnostic=False):
        self.mock.scenario = name
        log = self.output / (name + '.log')
        start = time.monotonic()
        with log.open('wb') as handle:
            result = subprocess.run(['perl', '-e', 'alarm shift; exec @ARGV', str(seconds),
                                     '/usr/bin/sandbox-exec', '-f', str(self.profile), *command],
                                    cwd=self.tmp, env={**self.env, **env}, stdin=subprocess.DEVNULL,
                                    stdout=handle, stderr=subprocess.STDOUT, timeout=seconds + 3)
        inference = [r for r in self.mock.records if r['scenario'] == name and
                     ('messages' in r['path'] or 'responses' in r['path'])]
        self.result(name, None if diagnostic else result.returncode == 0 and (name.endswith('version') or bool(inference)),
                    dict(exit=result.returncode, elapsed=round(time.monotonic() - start, 3), log=log.name,
                         requests=len(inference), diagnostic_reason='Native ChatGPT bootstrap is not emulated'
                         if diagnostic and 'chatgpt' in name else 'Intentional direct-mock exhaustion' if diagnostic else None))
        return result.returncode

    def execute(self):
        primary, primary_base = self.node('primary')
        _, backup_base = self.node('backup')
        router_port = port()
        router = self.tmp / 'router.yaml'
        router.write_text(json.dumps(dict(listen=f'127.0.0.1:{router_port}', client_host='sap-e2e',
                                         servers=[dict(name='primary', base_url=self.upstream if self.client_only else primary_base),
                                                  dict(name='backup', base_url=self.upstream if self.client_only else backup_base)],
                                         probe_interval_ms=1000, probe_timeout_ms=1500, down_after_failures=2,
                                         stable_secs=30, connect_timeout_ms=1000)))
        proc = self.start([str(ROOT / 'target/debug/sap-router'), '--config', str(router)], 'router')
        self.base = f'http://127.0.0.1:{router_port}'
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            try:
                if request(self.base, '/sap/router/status')[1].get('preferred') == 'primary':
                    break
            except OSError:
                pass
            time.sleep(.1)
        else:
            raise RuntimeError('router not ready')
        if not self.client_only:
            account_status, accounts = request(self.base, '/sap/api/accounts')
            (self.output / 'accounts.json').write_text(json.dumps(dict(status=account_status, body=accounts), indent=2) + '\n')
        claude_home = self.tmp / 'claude'
        claude_home.mkdir()
        (claude_home / 'settings.json').write_text(json.dumps({'skipDangerousModePermissionPrompt': True}))
        claude_env = dict(CLAUDE_CONFIG_DIR=str(claude_home), ANTHROPIC_BASE_URL=self.base,
                          ANTHROPIC_AUTH_TOKEN=KEY, CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC='1',
                          DISABLE_AUTOUPDATER='1', DISABLE_TELEMETRY='1', DISABLE_ERROR_REPORTING='1',
                          CLAUDE_CODE_SKIP_OAUTH='1')
        claude = [shutil.which('claude'), '-p', '--model', 'claude-sonnet-4-6',
                  '--dangerously-skip-permissions', '--output-format', 'json']
        self.client('claude-version', [claude[0], '--version'], claude_env, 10)
        session = str(uuid.uuid4())
        self.client('claude-new-tool-child', [*claude, '--session-id', session, 'Say local.'], claude_env)
        self.client('claude-resume', [*claude, '--resume', session, 'Continue locally.'], claude_env)
        if not self.client_only:
            status, threads = request(self.base, '/sap/api/threads')
            (self.output / 'threads.json').write_text(json.dumps(dict(status=status, body=threads), indent=2) + '\n')
            self.result('sap-assignment-and-affinity', status == 200 and isinstance(threads, list)
                        and bool(threads) and all(t.get('account') for t in threads), dict(status=status, threads=threads))
        if not self.client_only:
            self.claude_move(claude, claude_env)
        for mode in ('custom', 'custom-http', 'chatgpt'):
            if mode == 'custom-http' and not self.client_only:
                # Isolate quota state between transport scenarios, retaining the ports
                # and sandbox. Each scenario must start with two usable accounts.
                command = primary.args[6:]
                primary.terminate()
                primary.wait(timeout=5)
                for path in (self.tmp / 'primary').glob('sap.sqlite*'):
                    path.unlink()
                primary = self.start(command, 'primary-http')
                wait_ready(primary_base, primary)
            home = self.tmp / ('codex-' + mode)
            home.mkdir()
            config = 'model = "gpt-5.4"\n'
            if mode.startswith('custom'):
                config += (f'model_provider = "local"\n[model_providers.local]\nname = "Local mock"\n'
                           f'base_url = "{self.base}/v1"\nenv_key = "OPENAI_API_KEY"\n'
                           f'wire_api = "responses"\nsupports_websockets = {str(mode == "custom").lower()}\n')
            else:
                config += f'chatgpt_base_url = "{self.base}/"\n'
                # Fake ChatGPT claims, no refresh token, no real login file.
                def jwt(value):
                    enc = lambda v: base64.urlsafe_b64encode(json.dumps(v).encode()).decode().rstrip('=')
                    return enc({'alg': 'none'}) + '.' + enc(value) + '.fake'
                token = jwt({'exp': 4102444800, 'email': 'local@example.invalid',
                             'https://api.openai.com/auth': {'chatgpt_account_id': 'fake-client', 'chatgpt_plan_type': 'plus'}})
                (home / 'auth.json').write_text(json.dumps(dict(auth_mode='chatgpt', tokens=dict(id_token=token,
                    access_token=token, refresh_token='', account_id='fake-client'), last_refresh='2026-10-22T00:00:00Z')))
            (home / 'config.toml').write_text(config)
            codex_env = dict(CODEX_HOME=str(home), OPENAI_API_KEY=KEY, OPENAI_BASE_URL=self.base + '/v1')
            codex = [shutil.which('codex'), 'exec', '--skip-git-repo-check', '--json', 'Say local.']
            self.client('codex-' + mode + '-version', [codex[0], '--version'], codex_env, 10)
            self.client('codex-' + mode, codex, codex_env, diagnostic=mode == 'chatgpt')
            if self.client_only and mode == 'custom':
                self.mock.codex_tool = True
                self.client('codex-' + mode + '-socket-close', codex, codex_env, 15)
            if self.client_only:
                self.mock.exhausted.add('unknown')
            elif mode.startswith('custom'):
                self.mock.exhaust_next_codex = True
            self.client('codex-' + mode + '-exhausted', codex, codex_env, 15, diagnostic=mode == 'chatgpt' or self.client_only)
            if mode.startswith('custom') and not self.client_only:
                rows = [r for r in self.mock.records if r['scenario'] == 'codex-' + mode + '-exhausted']
                failed = next((r['account'] for r in rows if r['status'] == 429), None)
                client_log = (self.output / ('codex-' + mode + '-exhausted.log')).read_text()
                hidden = 'turn.failed' not in client_log and 'usage_limit_reached' not in client_log
                self.result('sap-codex-' + mode + '-exhaustion-move', hidden and failed is not None
                            and any(r['account'] != failed and r['status'] == 200 and r['generate'] is not False
                                    for r in rows), rows)
            self.mock.exhausted.clear()
            self.mock.exhaust_next_codex = False
        if self.client_only:
            self.client('claude-compact', [*claude, '--resume', session, '/compact'], claude_env)
        # Exercise provider-shaped exhaustion through the real Claude client.
        used = [r['account'] for r in self.mock.records if 'messages' in r['path'] and r['status'] == 200]
        if used:
            self.mock.exhausted.add(used[-1])
        self.client('claude-exhausted', [*claude, '--resume', session, 'Continue after exhaustion.'], claude_env, 15, diagnostic=self.client_only)
        self.mock.exhausted.clear()
        if self.client_only:
            self.client_features()
            return
        self.kill_time = time.time()
        primary.kill()
        primary.wait(timeout=3)
        self.mock.scenario = 'router-untracked-transport-probe'
        status, _ = request(self.base, '/v1/messages', dict(model='claude-sonnet-4-6', max_tokens=20,
                            messages=[dict(role='user', content='Local transport probe')]), key=KEY)
        elapsed = time.time() - self.kill_time
        self.result('router-untracked-transport-probe', status == 200 and elapsed < 5,
                    dict(status=status, kill_to_response_secs=round(elapsed, 3)))
        before = len(self.mock.records)
        self.client('claude-backup', [*claude, '--resume', session, 'Continue on backup.'], claude_env, 15)
        reached = self.mock.records[before:]
        self.result('router-real-client-failover-under-five-seconds', bool(reached)
                    and reached[0]['time'] - self.kill_time < 5,
                    dict(kill_to_first_request_secs=round(reached[0]['time'] - self.kill_time, 3) if reached else None,
                         router=request(self.base, '/sap/router/status')[1]))


    def claude_move(self, claude, env):
        # One live process, two turns. Only synthetic user messages enter stdin.
        name = 'claude-in-process-exhaustion'
        self.mock.scenario = name
        session = str(uuid.uuid4())
        command = [*claude[:-1], 'stream-json', '--input-format', 'stream-json', '--verbose',
                   '--session-id', session]
        log = self.output / (name + '.log')
        with log.open('wb') as handle:
            process = subprocess.Popen(['perl', '-e', 'alarm 35; exec @ARGV',
                '/usr/bin/sandbox-exec', '-f', str(self.profile), *command], cwd=self.tmp,
                env={**self.env, **env}, stdin=subprocess.PIPE, stdout=handle, stderr=handle)
            self.processes.append(process)
            def turn(text, count):
                process.stdin.write((json.dumps(dict(type='user', session_id=session,
                    message=dict(role='user', content=text))) + '\n').encode())
                process.stdin.flush()
                deadline = time.monotonic() + 15
                while time.monotonic() < deadline and process.poll() is None:
                    lines = log.read_text(errors='replace').splitlines()
                    results = []
                    for line in lines:
                        try:
                            value = json.loads(line)
                            if value.get('type') == 'result':
                                results.append(value)
                        except ValueError:
                            pass
                    if len(results) >= count:
                        return not results[-1].get('is_error', False)
                    time.sleep(.1)
                return False
            first = turn('Say local.', 1)
            before = [r for r in self.mock.records if r['scenario'] == name]
            account = before[-1]['account'] if before else None
            if account:
                self.mock.exhausted.add(account)
            second = turn('Continue locally after exhaustion.', 2) if first else False
            process.stdin.close()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.terminate()
                process.wait(timeout=3)
        self.mock.exhausted.clear()
        rows = [r for r in self.mock.records if r['scenario'] == name]
        continued = any(r['thread'] and r['thread'].get('type') == 'continue' for r in rows)
        moved = any(r['status'] == 429 and r['account'] == account for r in rows) and any(
            r['status'] == 200 and r['account'] != account for r in rows)
        self.result('sap-exhaustion-move-and-forced-resend', first and second and moved and not continued,
            dict(log=log.name, native_continue=continued, forced_resend_triggered=False,
                 note='Native continuation observed; forced 400/full resend needs verification' if continued else
                 'No thread field in custom-base mode; each request carries full history, so the move is free.',
                 requests=rows))

    def client_features(self):
        records = self.mock.records
        claude = [r for r in records if r['scenario'] == 'claude-new-tool-child']
        self.result('claude-session-and-agent-headers', any('x-claude-code-agent-id' in r['headers'] for r in claude)
                    and all('x-claude-code-session-id' in r['headers'] for r in claude),
                    dict(requests=len(claude), message_counts=[r['message_count'] for r in claude]))
        self.result('claude-native-thread-and-request-class', None,
                    dict(thread_fields=[r['thread'] for r in claude],
                         request_class=[r['headers'].get('x-claude-code-request-class') for r in claude],
                         compaction_headers=[r['headers'].get('x-claude-code-compaction') for r in records if r['scenario'] == 'claude-compact']))
        closed = [r for r in records if r['scenario'] == 'codex-custom-socket-close']
        close_index = next((i for i, r in enumerate(closed) if r['action'] == 'close'), None)
        resent = closed[close_index + 1:] if close_index is not None else []
        recovered = False
        if resent:
            original, retry = closed[close_index], resent[0]
            recovered = (bool(original['previous_response_id']) and retry['previous_response_id'] is None
                         and retry['input_count'] > original['input_count']
                         and retry['connection'] != original['connection'])
        self.result('codex-full-input-after-normal-close', recovered,
                    [dict(action=r['action'], connection=r['connection'], input_count=r['input_count'],
                          previous_response_id=r['previous_response_id'], time=r['time']) for r in closed])
        custom = [r for r in records if r['scenario'] == 'codex-custom']
        self.result('codex-custom-native-metadata', bool(custom) and all(r['transport'] == 'ws'
                    and 'thread-id' in r['headers'] and 'x-codex-turn-metadata' in r['headers'] for r in custom),
                    dict(prewarm=any(r['generate'] is False for r in custom),
                         remote_compaction_advertised=any(r['headers'].get('x-codex-beta-features') == 'remote_compaction_v2' for r in custom)))

    def close(self):
        for process in reversed(self.processes):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=3)
        self.mock.shutdown()
        self.mock.server_close()
        for handle in self.handles:
            handle.close()
        shutil.rmtree(self.tmp)
        (self.output / 'results.json').write_text(json.dumps(self.results, indent=2) + '\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--client-only', action='store_true', help='diagnose client/router/mock protocol; bypasses cliproxy and does not validate SAP')
    parser.add_argument('--output', type=Path, required=True, help='new evidence directory outside real client homes')
    args = parser.parse_args()
    output = args.output.resolve()
    protected = [Path.home() / name for name in ('.claude', '.codex', '.cli-proxy-api')]
    if any(output == home or home in output.parents for home in protected):
        raise SystemExit('Evidence must be outside real client/login directories')
    args.output.mkdir(parents=True, exist_ok=False)
    if not shutil.which('sandbox-exec'):
        raise SystemExit('macOS sandbox-exec required for loopback-only client safety')
    run = Run(args.output.resolve(), args.client_only)
    try:
        run.execute()
    finally:
        run.close()
    return 0 if all(r['passed'] is not False for r in run.results) else 1


if __name__ == '__main__':
    raise SystemExit(main())
