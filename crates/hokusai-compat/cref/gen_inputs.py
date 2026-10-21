#!/usr/bin/env python3
"""Convert a .myb brush and a hokusai-compat stroke script to cref text inputs."""

import argparse
import json
import struct
from pathlib import Path


def f32(value):
    return struct.unpack("<f", struct.pack("<f", float(value)))[0]


def fmt(value):
    # Nine significant digits round-trip every f32 exactly.
    return format(f32(value), ".9g")


def write_brush(myb_path, out_path):
    data = json.loads(Path(myb_path).read_text(encoding="utf-8"))
    settings = data.get("settings", {})
    lines = []
    setting_count = 0
    mapping_count = 0
    for setting_name, setting in settings.items():
        base = setting.get("base_value", 0.0)
        lines.append(f"B {setting_name} {fmt(base)}")
        setting_count += 1
        for input_name, points in setting.get("inputs", {}).items():
            if not points:
                continue
            lines.append(
                f"M {setting_name} {input_name} {len(points)} "
                + " ".join(f"{fmt(x)} {fmt(y)}" for x, y in points)
            )
            mapping_count += 1
    Path(out_path).write_text("\n".join(lines) + "\n", encoding="utf-8")
    return setting_count, mapping_count


def write_events(script_path, events_path):
    script = json.loads(Path(script_path).read_text(encoding="utf-8"))
    lines = []
    for event in script["events"]:
        if len(event) == 4:
            x, y, pressure, dtime = event
            xtilt = ytilt = viewzoom = viewrotation = barrel = 0.0
            viewzoom = 1.0
            linear = 1
        elif len(event) == 10:
            x, y, pressure, xtilt, ytilt, dtime, viewzoom, viewrotation, barrel, linear = event
        else:
            raise SystemExit(f"unsupported event arity: {len(event)}")
        # dtime is a double in the C API and in MyPaint's caller; keep the
        # script's f64 so both engines receive the identical time step.
        lines.append(
            f"{fmt(x)} {fmt(y)} {fmt(pressure)} {fmt(xtilt)} {fmt(ytilt)} "
            f"{format(float(dtime), '.17g')} "
            f"{fmt(viewzoom)} {fmt(viewrotation)} {fmt(barrel)} {int(linear)}"
        )
    Path(events_path).write_text("\n".join(lines) + "\n", encoding="utf-8")
    return int(script["width"]), int(script["height"]), len(lines)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("brush", help="input .myb")
    parser.add_argument("script", help="input stroke .json")
    parser.add_argument("out_dir", help="directory for brush.txt/events.txt")
    args = parser.parse_args()
    out = Path(args.out_dir)
    out.mkdir(parents=True, exist_ok=True)
    settings, mappings = write_brush(args.brush, out / "brush.txt")
    width, height, events = write_events(args.script, out / "events.txt")
    print(f"{width} {height}")
    print(
        f"settings={settings} mappings={mappings} events={events}",
        file=__import__("sys").stderr,
    )


if __name__ == "__main__":
    main()
