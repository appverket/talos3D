#!/usr/bin/env python3
"""Render-test daylight occlusion in a disposable native product instance.

Requires Pillow. Creates a generic two-box renderer fixture (not a building),
then compares the same ground pixel with the sun shadow enabled/disabled at
50m and 201m. This catches missing distant cascades and unmanaged product lights
that illuminate the scene outside the document lighting API. Outputs are local.
"""
import argparse
import json
from pathlib import Path
import time

from PIL import Image
from mcp_baseline import Client


def sample_ground(path, camera):
    image = Image.open(path).convert("RGB")
    width, height = image.size
    point = [-7, .01, -6]  # Behind the 10m cube relative to the default sun.
    delta = [point[i] - camera["view_position"][i] for i in range(3)]
    dot = lambda axis: sum(delta[i] * axis[i] for i in range(3))
    depth = dot(camera["view_forward"])
    scale = height * camera["focal_length_mm"] / (24 * depth)
    x = round(width / 2 + dot(camera["view_right"]) * scale)
    y = round(height / 2 - dot(camera["view_up"]) * scale)
    if not (3 <= x < width - 3 and 3 <= y < height - 3):
        raise RuntimeError("Reference ground sample is outside the viewport")
    pixels = [image.getpixel((xx, yy)) for xx in range(x-2, x+3)
              for yy in range(y-2, y+3)]
    return sum(.2126*r + .7152*g + .0722*b for r, g, b in pixels) / len(pixels)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--credentials", type=Path, required=True)
    parser.add_argument("--instance", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--replace-disposable-document", action="store_true", required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    client = Client(json.loads(args.credentials.read_text()))
    report = {"schema": "talos.native_shadow_acceptance.v1", "pass": False, "runs": []}
    try:
        client.start()
        info = client.call("get_instance_info")
        if info["instance_id"] != args.instance:
            raise RuntimeError("Instance differs; document was not replaced")
        client.call("get_authoring_guidance")
        client.call("set_session_profile", {"profile": "full"})
        client.call("invoke_command", {"command_id": "core.new"})
        for center, size in [([0, -.5, 0], [100, 1, 100]), ([0, 5, 0], [10, 10, 10])]:
            client.call("create_box", {"center": center, "size": size})
        lights = client.call("get_lighting_scene")["lights"]
        sun = next(light["element_id"] for light in lights if light["name"] == "Sun Key")
        for light in lights:
            client.call("update_light", {"element_id": light["element_id"],
                                        "enabled": light["element_id"] == sun})
        client.call("set_render_settings", {"ssao_enabled": False})
        for radius in [50, 201]:
            client.call("set_camera", {"focus": [0, 3, 0], "radius": radius,
                                      "yaw": 4.1, "pitch": -.6,
                                      "focal_length_mm": min(200, radius),
                                      "projection": "perspective"})
            camera = client.call("get_camera")
            values = {}
            for shadows in [False, True]:
                client.call("update_light", {"element_id": sun, "shadows_enabled": shadows})
                time.sleep(.5)
                path = args.output / f"shadow-{radius}-{shadows}.png"
                client.call("take_screenshot", {"path": str(path.resolve())})
                values[str(shadows)] = sample_ground(path, camera)
            ratio = values["True"] / values["False"]
            report["runs"].append({"radius_m": radius, "luminance": values,
                                   "shadow_to_unshadowed_ratio": ratio})
            if ratio >= .75:
                raise RuntimeError(f"Insufficient daylight occlusion at {radius}m: {ratio:.3f}")
        report["pass"] = True
    finally:
        client.close()
        (args.output / "result.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
