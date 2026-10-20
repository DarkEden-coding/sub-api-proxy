"""Run with: python3 -m unittest discover -s docs/parity-audit -p 'test_*.py'."""

import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("audit", Path(__file__).with_name("audit.py"))
audit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(audit)


class SummaryTests(unittest.TestCase):
    def test_plugin_http_fixtures_count_as_route_tests_but_not_callback_arguments(self):
        fixture = ('{"steps": [{"op": "http", "args": {"method": "DELETE", '
                   '"path": "/v8/management/plugins/example/quota"}}, '
                   '{"op": "respond", "args": {"method": "quota.reset"}}]}')
        with patch.object(audit, "scan_files", return_value=iter([
            ("crates/cpa-server/tests/fixtures/plugin_routes_go.json", fixture),
            ("crates/cpa-server/tests/fixtures/media_go.json",
             '{"scenarios": [{"method": "POST", "path": "/v1/videos/extensions"}]}'),
        ])):
            tests = audit.test_texts()
        key = "DELETE /v8/management/plugins/:id/quota"
        result = audit.judge_route({"text": f"`{key}`"}, {key: {"routed": True, "status": 400}}, tests)
        self.assertEqual(result[0], "covered")
        key = "POST /v1/videos/extensions"
        result = audit.judge_route({"text": f"`{key}`"}, {key: {"routed": True, "status": 401}}, tests)
        self.assertEqual(result[0], "covered")
        self.assertNotIn("quota.reset", "\n".join(tests.values()))

    def test_both_public_summaries_use_the_same_counts_and_are_idempotent(self):
        rows = [
            {"ms": "M1", "status": "covered"},
            {"ms": "M1", "status": "partial"},
            {"ms": "M1", "status": "partial"},
            {"ms": "M2", "status": "missing"},
        ]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "docs").mkdir()
            readme = root / "README.md"
            parity = root / "docs/PARITY.md"
            readme.write_text("Before. Parity is incomplete: the audit has 99 items, with "
                              "9 covered, 80 partial and 10 missing. After.\n")
            parity.write_text("Before\n<!-- parity-summary:start -->\nstale\n"
                              "<!-- parity-summary:end -->\nAfter\n")
            with patch.object(audit, "ROOT", str(root)):
                audit.summary(rows)
                first = (readme.read_text(), parity.read_text())
                self.assertEqual(first[0], "Before. Parity is incomplete: the audit has 4 items, "
                                 "with 1 covered, 2 partial and 1 missing. After.\n")
                self.assertIn("| M1 | 3 | 1 | 2 | 0 |", first[1])
                self.assertIn("| M2 | 1 | 0 | 0 | 1 |", first[1])
                self.assertIn("| All | 4 | 1 | 2 | 1 |", first[1])
                audit.summary(rows)
                self.assertEqual(first, (readme.read_text(), parity.read_text()))


if __name__ == "__main__":
    unittest.main()
