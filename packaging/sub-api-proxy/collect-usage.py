#!/usr/bin/env python3
"""Collect allowlisted local usage telemetry without retaining legacy credentials."""
import json
import os
from pathlib import Path
import time
import urllib.request


def collect(key_file, output):
    key = key_file.read_text().strip()
    request = urllib.request.Request(
        'http://127.0.0.1:8318/v8/management/observability/usage/queue?count=100',
        headers={'Authorization': 'Bearer ' + key})
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(request, timeout=10) as response:
        payload = response.read(8 * 1024 * 1024 + 1)
    if len(payload) > 8 * 1024 * 1024:
        raise ValueError('usage response exceeds limit')
    records = json.loads(payload)
    if not isinstance(records, list):
        raise ValueError('usage response is not an array')
    allowed = ('execution_id', 'request_id', 'failed', 'usage_observed', 'tokens', 'claude_cache')
    fd = os.open(output, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
    with os.fdopen(fd, 'a') as handle:
        os.fchmod(handle.fileno(), 0o600)
        for record in records:
            if isinstance(record, dict):
                handle.write(json.dumps({name: record[name] for name in allowed if name in record}) + '\n')
    return len(records)


def main():
    home = Path.home()
    key = home / '.config/sub-api-proxy/management-key'
    output = home / '.local/state/sub-api-proxy/usage.jsonl'
    while True:
        try:
            count = collect(key, output)
        except (OSError, ValueError):
            # Never print request/response data or exceptions containing credentials.
            print('Local usage collection unavailable; retrying.', flush=True)
            time.sleep(10)
        else:
            time.sleep(0.1 if count == 100 else 5)


if __name__ == '__main__':
    main()
