#!/usr/bin/env python3
"""Run the libmypaint C reference and hokusai CPU path over a brush pack."""

import argparse
import json
import struct
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import gen_inputs  # noqa: E402


def load_dump(path):
    data = Path(path).read_bytes()
    width, height = struct.unpack("<II", data[:8])
    values = struct.unpack("<%dH" % (width * height * 4), data[8:])
    return width, height, values


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--brushes", required=True)
    parser.add_argument("--script", required=True)
    parser.add_argument("--work", required=True)
    parser.add_argument("--ref", required=True)
    parser.add_argument("--rust", required=True)
    parser.add_argument("--limit", type=int, default=0)
    parser.add_argument("--tolerance", type=int, default=0)
    parser.add_argument("--markdown", default="")
    args = parser.parse_args()

    brushes = sorted(Path(args.brushes).rglob("*.myb"))
    if args.limit:
        brushes = brushes[: args.limit]
    work = Path(args.work)
    work.mkdir(parents=True, exist_ok=True)
    script = json.loads(Path(args.script).read_text())
    width, height = int(script["width"]), int(script["height"])

    rows = []
    failed = 0
    for index, brush in enumerate(brushes, 1):
        case = work / f"{index:03d}"
        case.mkdir(parents=True, exist_ok=True)
        settings, mappings = gen_inputs.write_brush(brush, case / "brush.txt")
        gen_inputs.write_events(args.script, case / "events.txt")
        ref = subprocess.run(
            [args.ref, str(case / "brush.txt"), str(case / "events.txt"),
             str(case / "c.bin"), str(width), str(height), "1"],
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
        )
        rust = subprocess.run(
            [args.rust, str(brush), args.script, str(case / "r.bin"), "1"],
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
        )
        if ref.returncode != 0 or rust.returncode != 0:
            rows.append((brush, "ERROR", 0, 0, ref.stderr.decode(errors="replace")[-200:]
                         + rust.stderr.decode(errors="replace")[-200:]))
            failed += 1
            continue
        _, _, c = load_dump(case / "c.bin")
        _, _, r = load_dump(case / "r.bin")
        diffs = [abs(a - b) for a, b in zip(c, r) if a != b]
        if not diffs:
            rows.append((brush, "IDENTICAL", 0, 0, ""))
        else:
            max_diff = max(diffs)
            rows.append((brush, "DIFF", len(diffs), max_diff, ""))
            if max_diff > args.tolerance:
                failed += 1
        if index % 10 == 0 or index == len(brushes):
            print(f"[{index}/{len(brushes)}] {brush.name}: {rows[-1][1]} "
                  f"n={rows[-1][2]} max={rows[-1][3]}", flush=True)

    identical = sum(1 for row in rows if row[1] == "IDENTICAL")
    diff = sum(1 for row in rows if row[1] == "DIFF")
    errors = sum(1 for row in rows if row[1] == "ERROR")
    print(f"\nbrushes={len(rows)} identical={identical} diff={diff} error={errors}")
    if args.markdown:
        lines = ["| brush | status | diff values | max diff |", "|---|---|---|---|"]
        for brush, status, n, max_diff, _ in rows:
            lines.append(f"| {brush.name} | {status} | {n} | {max_diff} |")
        Path(args.markdown).write_text("\n".join(lines) + "\n", encoding="utf-8")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
