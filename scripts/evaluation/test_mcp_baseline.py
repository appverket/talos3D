import json
import contextlib
import hashlib
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from mcp_baseline import contains, decode_response, main, probe


class BaselineTests(unittest.TestCase):
    def test_changed_control_and_missing_state_fail(self):
        for actual in [{"centre": [5, 0.25, -0.9]}, {}, {"centre": [5, 0.25]}]:
            with self.assertRaises(AssertionError):
                contains(actual, {"centre": [5, 0.25, -1]})

    def test_numeric_tolerance_never_coerces_booleans(self):
        contains({"length": 1.200000047}, {"length": 1.2}, tolerance=1e-6)
        for actual in [True, "1.2", float("nan")]:
            with self.assertRaises(AssertionError):
                contains(actual, 1.2, tolerance=1e-6)

    def test_sse_ignores_notifications_and_other_ids(self):
        body = 'data: {"method":"notifications/tools/list_changed"}\r\n\r\n' + \
               'data: {"id":1,"result":{}}\r\n\r\n' + \
               'data: {"id":2,\r\ndata: "result":{"ok":true}}\r\n\r\n'
        self.assertEqual(decode_response(body, "text/event-stream", 2)["result"], {"ok": True})

    def test_refused_read_stays_failed_and_next_probe_can_run(self):
        class Stub:
            def call(self, name, args):
                if args.get("missing"):
                    raise RuntimeError("Unknown instance")
                return {"id": 24}
        client = Stub()
        check = {"name": "read", "tool": "parametric.inspect", "expect": {"id": 24}}
        self.assertEqual(probe(client, dict(check, arguments={"missing": True}))["status"], "FAIL")
        self.assertEqual(probe(client, check)["status"], "PASS")

    def test_manifest_cannot_smuggle_writes_into_read_probes(self):
        self.assertEqual(probe(None, {"name": "bad", "tool": "create_box"})["status"], "FAIL")

    def test_wrong_instance_is_refused_before_any_load(self):
        invoked = []
        class Stub:
            calls = []
            def __init__(self, credentials):
                pass
            def start(self):
                pass
            def close(self):
                pass
            def call(self, name):
                invoked.append(name)
                return {"instance_id": "another-users-document"}
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            fixture = root / "fixture.json"
            fixture.write_text("{}")
            (root / "credential.json").write_text("{}")
            manifest = root / "manifest.json"
            manifest.write_text(json.dumps({"project_file": fixture.name,
                "project_sha256": hashlib.sha256(fixture.read_bytes()).hexdigest(),
                "checks": [], "unmeasured_gates": ["continued_work"]}))
            argv = ["baseline", str(manifest), "--credentials", str(root / "credential.json"),
                    "--instance", "disposable", "--replace-disposable-document",
                    "--output", str(root / "report.json")]
            with patch("sys.argv", argv), patch("mcp_baseline.Client", Stub), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(main(), 1)
            report = json.loads((root / "report.json").read_text())
            self.assertEqual(report["gates"][-1]["status"], "NOT_RUN")
            self.assertFalse(report["full_acceptance"])
        self.assertEqual(invoked, ["get_instance_info"])


if __name__ == "__main__":
    unittest.main()
