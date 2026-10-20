"""Run with: python3 -m unittest discover -s docs/parity-audit -p 'test_*.py'."""

import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("audit", Path(__file__).with_name("audit.py"))
audit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(audit)
spec = importlib.util.spec_from_file_location("probe", Path(__file__).with_name("probe.py"))
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


class SummaryTests(unittest.TestCase):
    def test_v8_035_routes_and_shared_config_paths(self):
        with tempfile.TemporaryDirectory() as directory:
            Path(directory, "checklist.md").write_text(
                '- [ ] **[M1] M1-0128** `GET /v1/models/*model` — model detail\n'
                '- [ ] **[M5] M5-0304** `POST /v8/management/oauth/siwc/bind` — bind\n'
                '- [ ] **[M6] M6-0249** RPC `host.routing.reset_cooldown`\n'
                '- [ ] **[M6] M6-0250** Plugin data contract `ConfigField.Default`\n')
            with patch.object(probe, "HERE", directory):
                self.assertEqual(probe.routes(), [
                    ("M1-0128", "GET", "/v1/models/*model"),
                    ("M5-0304", "POST", "/v8/management/oauth/siwc/bind"),
                ])
            self.assertEqual(probe.concrete("/v1/models/*model"), "/v1/models/probe")
        for key, owner in (("upstream.claude.header-defaults.os", "claude"),
                           ("upstream.codex.response-steering", "codex"),
                           ("upstream.xai.inject-x-search", "openai-xai"),
                           ("oauth.providers.codex.base-url", "codex")):
            with self.subTest(key=key):
                self.assertEqual(audit.config_owner(key), owner)

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
        for key in ("GET /v8/management/plugins/:id/quota", "DELETE /v8/management/plugins/:id/config"):
            result = audit.judge_route({"text": f"`{key}`"}, {key: {"routed": True, "status": 400}}, tests)
            self.assertEqual(result[0], "partial")
        key = "POST /v1/videos/extensions"
        result = audit.judge_route({"text": f"`{key}`"}, {key: {"routed": True, "status": 401}}, tests)
        self.assertEqual(result[0], "covered")
        self.assertNotIn("quota.reset", "\n".join(tests.values()))

    def test_fixture_route_parameters_stop_at_query_quote_and_line_boundaries(self):
        quota = "DELETE /v8/management/plugins/:id/quota"
        models = "GET /v1/models/*model"
        cases = [
            (quota, 'DELETE "/v8/management/plugins/example/quota?next=/other"', "covered"),
            (quota, 'DELETE "/v8/management/plugins/example?next=/quota"', "partial"),
            (models, 'GET "/v1/models/provider/model?next=/other"', "covered"),
            (models, 'GET "/v1/models/?next=/provider/model"', "partial"),
            (models, 'GET "/v1/models/provider" "unrelated"', "partial"),
            (quota, 'DELETE "/v8/management/plugins/ex\rample/quota"', "partial"),
            (quota, 'DELETE "/v8/management/plugins/ex\nample/quota"', "partial"),
            (models, 'GET "/v1/models/provider\rmodel"', "partial"),
            (models, 'GET "/v1/models/provider\nmodel"', "partial"),
        ]
        for key, request, expected in cases:
            with self.subTest(key=key, request=request):
                result = audit.judge_route(
                    {"text": f"`{key}`"}, {key: {"routed": True, "status": 400}},
                    {"requests.json": request})
                self.assertEqual(result[0], expected)

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

    def test_invalid_readme_summary_leaves_all_pages_unchanged(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "docs").mkdir()
            paths = [root / "README.md", root / "docs/PARITY.md", root / "status.md"]
            paths[1].write_text("<!-- parity-summary:start -->\nstale\n<!-- parity-summary:end -->")
            paths[2].write_text("Existing status page")
            sentence = "Parity is incomplete: the audit has 9 items, with 3 covered, 4 partial and 2 missing."
            for readme in ("Summary was reworded", sentence + "\n" + sentence):
                with self.subTest(readme=readme):
                    paths[0].write_text(readme)
                    before = [p.read_text() for p in paths]
                    with patch.object(audit, "ROOT", str(root)), patch.object(audit, "HERE", str(root)):
                        with self.assertRaisesRegex(ValueError, "exactly one parity summary"):
                            audit.render([])
                    self.assertEqual(before, [p.read_text() for p in paths])


if __name__ == "__main__":
    unittest.main()
