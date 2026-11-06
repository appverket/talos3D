#!/usr/bin/env python3
"""Compare real native input/presentation traces on a disposable saved project.

Run the same fixture, binary configuration, hardware, camera and viewport before
and after an engine update. Timing excludes HTTP and physical display scanout.
Credentials, fixture files, traces and reports belong outside Git.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import time

from mcp_baseline import Client


def percentile(values, fraction):
    ordered = sorted(values)
    return ordered[max(0, math.ceil(len(ordered) * fraction) - 1)]


def observe_idle(client):
    for _ in range(200):
        observation = client.call("ux_observe")
        if observation["pending_steps"] == 0:
            return observation
        time.sleep(0.05)
    raise RuntimeError("Native input queue did not finish")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("project", type=Path)
    parser.add_argument("--credentials", type=Path, required=True)
    parser.add_argument("--instance", required=True)
    parser.add_argument("--root", type=int, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--replace-disposable-document", action="store_true", required=True)
    parser.add_argument("--repeat", type=int, default=3, choices=range(1, 11))
    parser.add_argument("--budget-ms", type=float, default=33)
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--build-label", default="unspecified")
    parser.add_argument("--camera", type=json.loads, default={
        "projection": "orthographic", "orthographic_scale": 38,
        "focus": [0, 0, 0], "yaw": 0.5, "pitch": -0.5,
    })
    args = parser.parse_args()
    report = {"schema": "talos.native_engine_benchmark.v1", "pass": False,
              "fixture_sha256": hashlib.sha256(args.project.read_bytes()).hexdigest(),
              "declared_build": args.build_label, "camera": args.camera,
              "budget_active_p95_ms": args.budget_ms, "runs": []}
    client = Client(json.loads(args.credentials.read_text()))
    active_times = []
    try:
        client.start()
        info = client.call("get_instance_info")
        if info["instance_id"] != args.instance:
            raise RuntimeError("Instance differs; no project was replaced")
        client.call("get_authoring_guidance")
        snapshot = client.call("get_capability_snapshot")
        for card in snapshot["must_read_guidance_card_ids"]:
            client.call("get_guidance_card", {"card_id": card})
        client.call("set_session_profile", {"profile": "full"})
        for iteration in range(args.repeat):
            client.call("load_project", {"path": str(args.project.resolve())})
            client.call("set_camera", args.camera)
            client.call("set_selection", {"element_ids": [args.root]})
            client.call("invoke_command", {"command_id": "modeling.move"})
            observe_idle(client)
            time.sleep(0.35)  # Let the camera and selection-dependent chrome settle.
            observation = client.call("ux_observe")
            selected = next(e for e in observation["entities"] if e["element_id"] == args.root)
            start = selected["screen_center"]
            report["window"] = observation["window"]
            client.call("ux_drag", {"start": start, "end": [start[0]+110, start[1]-35],
                                    "steps": 72, "button": "left", "trace": True})
            observe_idle(client)
            time.sleep(0.15)
            trace = client.call("ux_observe")["drag_trace"]
            samples = trace["samples"]
            motion = [s for s in samples if s["action"] == "pointer_move"
                      and s["grab_anchor"] is not None]
            if len(motion) < 60 or any(s["present_ms"] is None for s in motion):
                raise RuntimeError("Motion lacks captured swapchain presentation evidence")
            times = [s["present_ms"] for s in motion]
            release = [s for s in samples if s["committed_exact_candidate"] is not None]
            if not release or not release[-1]["committed_exact_candidate"]:
                raise RuntimeError("Release did not commit the displayed candidate")
            if release[-1]["present_ms"] is None:
                raise RuntimeError("Release lacks swapchain presentation evidence")
            press = [s["present_ms"] for s in samples if s["action"] == "press"]
            if not press or any(value is None for value in press):
                raise RuntimeError("Activation lacks swapchain presentation evidence")
            active_times.extend(times)
            run = {"active_samples": len(times), "active_p95_ms": percentile(times, 0.95),
                   "active_worst_ms": max(times), "activation_ms": max(press),
                   "release_ms": release[-1]["present_ms"],
                   "release_to_commit_ms": release[-1]["release_to_commit_ms"],
                   "exact_candidate": True}
            report["runs"].append(run)
            args.output.with_name(f"{args.output.stem}-trace-{iteration}.json").write_text(json.dumps(trace))
            print(json.dumps(run), flush=True)
        report["active_p95_ms"] = percentile(active_times, 0.95)
        report["worst_observed_ms"] = max(
            max(r["active_worst_ms"], r["activation_ms"], r["release_ms"])
            for r in report["runs"])
        if any(r["active_p95_ms"] > args.budget_ms for r in report["runs"]):
            raise RuntimeError("Active p95 exceeds the acceptance budget")
        if args.baseline:
            baseline = json.loads(args.baseline.read_text())
            for field in ("fixture_sha256", "camera", "window"):
                if baseline[field] != report[field]:
                    raise RuntimeError(f"Baseline {field} differs")
            for metric in ("active_p95_ms", "activation_ms", "release_ms", "active_worst_ms"):
                old = max(r[metric] for r in baseline["runs"])
                new = max(r[metric] for r in report["runs"])
                if new > max(old * 1.2, old + 5):
                    raise RuntimeError(f"Material regression in {metric}")
        report["pass"] = True
    except (RuntimeError, ValueError, KeyError, StopIteration) as error:
        report["error"] = str(error)
    finally:
        client.close()
        args.output.write_text(json.dumps(report, indent=2))
    return 0 if report["pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
