"""Ensure the local collector drops credential-bearing legacy fields."""
import importlib.util
import io
import json
from pathlib import Path
import tempfile
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('collector', Path(__file__).with_name('collect-usage.py'))
collector = importlib.util.module_from_spec(spec)
spec.loader.exec_module(collector)
with tempfile.TemporaryDirectory() as temporary:
    root = Path(temporary)
    key = root / 'key'
    key.write_text('fake-management-key')
    output = root / 'usage.jsonl'
    response = json.dumps([dict(execution_id='test', api_key='credential-must-be-dropped',
                               auth_id='legacy-account', tokens={'cache_read_tokens': 12},
                               claude_cache={'schema_version': 2})]).encode()
    class Opener:
        def open(self, request, timeout):
            assert request.get_header('Authorization') == 'Bearer fake-management-key'
            assert timeout == 10
            return io.BytesIO(response)
    with patch.object(collector.urllib.request, 'build_opener', return_value=Opener()):
        assert collector.collect(key, output) == 1
    record = json.loads(output.read_text())
    assert record['tokens']['cache_read_tokens'] == 12
    assert record['claude_cache']['schema_version'] == 2
    assert 'credential-must-be-dropped' not in output.read_text()
    assert 'legacy-account' not in output.read_text()
    assert 'fake-management-key' not in output.read_text()
    assert output.stat().st_mode & 0o777 == 0o600
print('PASS: usage collector retains telemetry, omits credentials, and protects the output')
