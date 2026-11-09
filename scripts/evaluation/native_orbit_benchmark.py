#!/usr/bin/env python3
"""Measure native orbit input-to-present on a disposable saved project.

Use an optimized product build and identical fixture, camera, viewport and
hardware for both versions. Keep the window visible. No model reads or profiler
samples run during the gesture. Private fixtures, credentials and output stay
outside Git. --record-baseline records a slow baseline without accepting it.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import time

from mcp_baseline import Client
from native_engine_benchmark import percentile


def summarize(trace, steps):
    samples = trace["samples"]
    press = next(s for s in samples if s["action"] == "press")
    release = next(s for s in samples if s["action"] == "release")
    motion = [s for s in samples if s["action"] == "pointer_move"
              and press["index"] < s["index"] < release["index"]]
    if len(motion) != steps or any(s["present_ms"] is None for s in samples):
        raise RuntimeError("Incomplete gesture or missing swapchain presentation")
    if any(s["model_revision"] != press["model_revision"] or s["plan_id"] is not None
           for s in samples):
        raise RuntimeError("Orbit changed authored state or started a model edit")
    return {"motion_samples": len(motion),
            "active_p95_ms": percentile([s["present_ms"] for s in motion], .95),
            "active_worst_ms": max(s["present_ms"] for s in motion),
            "main_p95_ms": percentile([s["main_frame_ms"] for s in motion], .95),
            "activation_ms": press["present_ms"], "release_ms": release["present_ms"],
            "worst_observed_ms": max(s["present_ms"] for s in samples)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("project", type=Path)
    parser.add_argument("--credentials", type=Path, required=True)
    parser.add_argument("--instance", required=True)
    parser.add_argument("--camera", type=Path, required=True, help="JSON set_camera parameters")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--replace-disposable-document", action="store_true", required=True)
    parser.add_argument("--repeat", type=int, default=3, choices=range(1, 11))
    parser.add_argument("--steps", type=int, default=72, choices=range(12, 121))
    parser.add_argument("--wait-seconds", type=int, default=40, choices=range(1, 61),
                        help="Wait without observer reads; fail if the gesture is still pending")
    parser.add_argument("--budget-ms", type=float, default=33)
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--record-baseline", action="store_true")
    parser.add_argument("--build-label", required=True)
    args = parser.parse_args()
    camera = json.loads(args.camera.read_text())
    report = {"schema": "talos.native_orbit_benchmark.v1", "pass": False,
              "fixture_sha256": hashlib.sha256(args.project.read_bytes()).hexdigest(),
              "build": args.build_label, "camera": camera, "delta_pixels": [300, 80],
              "budget_active_p95_ms": args.budget_ms, "runs": []}
    client = Client(json.loads(args.credentials.read_text()))
    complete = False
    try:
        client.start()
        info = client.call("get_instance_info")
        if info["instance_id"] != args.instance:
            raise RuntimeError("Instance differs; document was not replaced")
        client.call("get_authoring_guidance")
        client.call("set_session_profile", {"profile": "full"})
        client.call("load_project", {"path": str(args.project.resolve())})
        time.sleep(2)
        for iteration in range(args.repeat):
            client.call("set_camera", camera)
            time.sleep(.5)
            before = client.call("ux_observe")
            if before["pending_steps"]:
                raise RuntimeError("Another input gesture is pending")
            window = before["window"]
            report["window"] = window
            start = [window["width"]*.5, window["height"]*.5]
            client.call("ux_drag", {"start": start, "end": [start[0]+300, start[1]+80],
                                   "steps": args.steps, "modifier": "alt",
                                   "button": "right", "trace": True})
            time.sleep(args.wait_seconds)
            after = client.call("ux_observe")
            if after["pending_steps"]:
                raise RuntimeError("Gesture is still pending; rerun with a longer wait")
            trace = after["drag_trace"]
            args.output.with_name(f"{args.output.stem}-trace-{iteration}.json").write_text(json.dumps(trace))
            run = summarize(trace, args.steps)
            # Camera navigation must preserve every authored world bound.
            bounds = lambda o: {e["element_id"]: e["world_bounds"] for e in o["entities"]}
            if bounds(before) != bounds(after):
                raise RuntimeError("Orbit changed authored bounds")
            actual = client.call("get_camera")
            if not math.isclose(actual["yaw"], camera["yaw"]-1.5, abs_tol=1e-4) or not math.isclose(
                    actual["pitch"], max(-math.pi/2+.05, camera["pitch"]-.4), abs_tol=1e-4):
                raise RuntimeError("Orbit did not reach the expected camera orientation")
            run["authored_bounds_unchanged"] = True
            report["runs"].append(run)
            print(json.dumps(run), flush=True)
        complete = True
        if args.baseline:
            baseline = json.loads(args.baseline.read_text())
            for field in ("fixture_sha256", "camera", "window", "delta_pixels"):
                if baseline[field] != report[field]:
                    raise RuntimeError(f"Baseline {field} differs")
        if any(r["active_p95_ms"] > args.budget_ms for r in report["runs"]):
            raise RuntimeError("Active orbit p95 exceeds the acceptance budget")
        report["pass"] = True
    except (RuntimeError, ValueError, KeyError, StopIteration) as error:
        report["error"] = str(error)
    finally:
        client.close()
        args.output.write_text(json.dumps(report, indent=2))
    return 0 if report["pass"] or (args.record_baseline and complete) else 1


if __name__ == "__main__":
    raise SystemExit(main())
