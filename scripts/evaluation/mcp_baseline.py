#!/usr/bin/env python3
"""Read-probe baseline, not an authoring IR or an aggregate acceptance score.

The owning domain supplies a native project and expected read responses. This
runner loads it only with an explicit disposable-document flag, uses a fresh
authenticated MCP session, and reports PASS/FAIL/NOT_RUN for each gate.
Credentials and raw responses are never written to the report.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import time
import urllib.error
import urllib.request


READ_TOOLS = frozenset({
    "get_entity_details", "get_entities_details", "list_entities", "model_summary",
    "get_world_aabb", "get_claim_grounding", "get_authoring_provenance",
    "get_obligations", "explain_design", "get_dependency_graph", "get_refinement_state",
    "parametric.inspect", "parametric.explain", "occurrence.resolve", "definition.explain",
    "discover_curated_paths", "check_overlaps", "check_floating", "check_clearance",
})


def contains(actual, expected, path="$", tolerance=0.0):
    """Expected maps are subsets; arrays and scalar values must match exactly."""
    if isinstance(expected, dict):
        if not isinstance(actual, dict):
            raise AssertionError(f"{path}: expected object")
        for key, value in expected.items():
            if key not in actual:
                raise AssertionError(f"{path}.{key}: missing")
            contains(actual[key], value, f"{path}.{key}", tolerance)
    elif isinstance(expected, list):
        if not isinstance(actual, list) or len(actual) != len(expected):
            raise AssertionError(f"{path}: array length differs")
        for i, (left, right) in enumerate(zip(actual, expected)):
            contains(left, right, f"{path}[{i}]", tolerance)
    elif type(expected) in (int, float) and type(actual) in (int, float):
        if not math.isfinite(actual) or not math.isfinite(expected) or not math.isclose(
            actual, expected, rel_tol=0, abs_tol=tolerance
        ):
            raise AssertionError(f"{path}: numeric value differs")
    elif type(actual) is not type(expected) or actual != expected:
        raise AssertionError(f"{path}: value differs")


def decode_response(body, content_type, request_id):
    if "text/event-stream" in content_type:
        messages = []
        for event in body.replace("\r\n", "\n").split("\n\n"):
            data = "\n".join(line[5:].lstrip() for line in event.splitlines() if line.startswith("data:"))
            if data:
                messages.append(json.loads(data))
    else:
        messages = [json.loads(body)]
    return next(m for m in messages if m.get("id") == request_id)


class Client:
    def __init__(self, credentials):
        self.url = credentials["url"]
        self.token = credentials["token"]
        self.session = None
        self.sequence = 0
        self.calls = []

    def request(self, method, params=None, notification=False):
        self.sequence += 1
        payload = {"jsonrpc": "2.0", "method": method, "params": params or {}}
        if not notification:
            payload["id"] = self.sequence
        headers = {"Content-Type": "application/json", "Accept": "application/json, text/event-stream",
                   "MCP-Protocol-Version": "2025-06-18", "Authorization": "Bearer " + self.token}
        if self.session:
            headers["Mcp-Session-Id"] = self.session
        start = time.perf_counter()
        req = urllib.request.Request(self.url, data=json.dumps(payload).encode(), headers=headers)
        try:
            with urllib.request.urlopen(req, timeout=45) as response:
                self.session = response.headers.get("Mcp-Session-Id", self.session)
                body = response.read()
                content_type = response.headers.get("Content-Type", "")
        except urllib.error.HTTPError as error:
            raise RuntimeError(f"MCP HTTP status {error.code}") from None
        except urllib.error.URLError:
            raise RuntimeError("MCP endpoint unavailable") from None
        self.calls.append({"method": params.get("name", method) if params else method,
                           "response_bytes": len(body), "elapsed_ms": round((time.perf_counter()-start)*1000, 3)})
        if notification:
            return None
        decoded = decode_response(body.decode(), content_type, self.sequence)
        if "error" in decoded:
            raise RuntimeError(str(decoded["error"].get("message", "MCP error"))[:300])
        return decoded["result"]

    def start(self):
        self.request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                    "clientInfo": {"name": "talos-baseline", "version": "1"}})
        if self.session:
            self.request("notifications/initialized", notification=True)

    def call(self, tool, arguments=None):
        result = self.request("tools/call", {"name": tool, "arguments": arguments or {}})
        if result.get("isError"):
            raise RuntimeError(f"{tool} returned a tool error")
        texts = [item["text"] for item in result.get("content", []) if item.get("type") == "text"]
        if not texts:
            raise RuntimeError(f"{tool} returned no structured text")
        return json.loads(texts[0])

    def close(self):
        if not self.session:
            return
        req = urllib.request.Request(self.url, method="DELETE", headers={
            "Authorization": "Bearer " + self.token, "Mcp-Session-Id": self.session,
            "MCP-Protocol-Version": "2025-06-18"})
        try:
            with urllib.request.urlopen(req, timeout=5):
                pass
        except (urllib.error.URLError, TimeoutError):
            pass


def probe(client, check):
    name = check["name"]
    if check["tool"] not in READ_TOOLS:
        return {"name": name, "status": "FAIL", "reason": "Tool is not an allowed read probe"}
    try:
        actual = client.call(check["tool"], check.get("arguments", {}))
        contains(actual, check["expect"], tolerance=check.get("absolute_tolerance", 0.0))
        return {"name": name, "status": "PASS"}
    except (AssertionError, RuntimeError, ValueError) as error:
        return {"name": name, "status": "FAIL", "reason": str(error)[:300]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--credentials", type=Path, required=True, help="Private JSON with url/token; never copied")
    parser.add_argument("--instance", required=True, help="Expected disposable app instance id")
    parser.add_argument("--replace-disposable-document", action="store_true", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--source", action="append", default=[], help="Recorded repo=SHA for the built app")
    args = parser.parse_args()
    manifest = json.loads(args.manifest.read_text())
    fixture = (args.manifest.parent / manifest["project_file"]).resolve()
    digest = hashlib.sha256(fixture.read_bytes()).hexdigest()
    if digest != manifest["project_sha256"]:
        parser.error("Native fixture digest differs from the locked manifest")
    checks = manifest["checks"]
    pending = manifest.get("unmeasured_gates", [])
    names = [c["name"] for c in checks] + pending
    if len(names) != len(set(names)):
        parser.error("Gate names must be unique")
    client = Client(json.loads(args.credentials.read_text()))
    report = {"schema": "talos.mcp_baseline.v1", "fixture_sha256": digest,
              "manifest_sha256": hashlib.sha256(args.manifest.read_bytes()).hexdigest(),
              "declared_build_sources": args.source, "lane": "deterministic_read_baseline",
              "gates": [], "full_acceptance": False}
    try:
        client.start()
        info = client.call("get_instance_info")
        if info["instance_id"] != args.instance:
            raise RuntimeError("Instance identity mismatch; no document was loaded")
        report["app"] = {key: info[key] for key in ("instance_id", "app_name", "authoring_guidance_version")}
        guidance = client.call("get_authoring_guidance")
        report["guidance_sha256"] = hashlib.sha256(guidance["prompt_text"].encode()).hexdigest()
        capability = client.call("get_capability_snapshot")
        for card in capability["must_read_guidance_card_ids"]:
            client.call("get_guidance_card", {"card_id": card})
        for skill in capability.get("must_read_agent_skill_ids", []):
            client.call("get_agent_skill", {"skill_id": skill})
        report["gates"].append({"name": "bootstrap", "status": "PASS"})
        client.request("tools/list")
        client.call("load_project", {"path": str(fixture)})
        for check in checks:
            report["gates"].append(probe(client, check))
        report["gates"].extend({"name": name, "status": "NOT_RUN"} for name in pending)
    except (RuntimeError, ValueError, KeyError, StopIteration) as error:
        report["gates"].append({"name": "episode", "status": "FAIL", "reason": str(error)[:300]})
        recorded = {gate["name"] for gate in report["gates"]}
        report["gates"].extend({"name": name, "status": "NOT_RUN"} for name in names if name not in recorded)
    finally:
        client.close()
        report["calls"] = client.calls
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"report": str(args.output), "gates": report["gates"], "full_acceptance": False}, indent=2))
    return 1 if any(g["status"] == "FAIL" for g in report["gates"]) else 2


if __name__ == "__main__":
    raise SystemExit(main())
