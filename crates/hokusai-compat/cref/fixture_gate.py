#!/usr/bin/env python3
"""Run the C-vs-Rust gate once per hokusai-compat fixture script.

`diff_gate.py` takes one event stream and a directory of brushes. The compat
fixtures are the opposite: one brush each, its own event stream. This script
pairs them up and reports per-fixture status, so "the fixtures are byte-exact
against libmypaint" stays a command instead of a claim.

Exit code is non-zero if any fixture is not bit-identical.
"""

import argparse
import json
import shutil
import subprocess
import sys
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--fixtures", required=True,
                        help="directory holding the hokusai-compat *.json scripts")
    parser.add_argument("--work", required=True, help="scratch + report directory")
    parser.add_argument("--ref", required=True, help="ref_runner binary (libmypaint C)")
    parser.add_argument("--rust", required=True, help="dump_fix15 binary (hokusai CPU)")
    parser.add_argument("--gate", default=str(Path(__file__).resolve().parent / "diff_gate.py"))
    parser.add_argument("--tolerance", type=int, default=0)
    args = parser.parse_args()

    fixtures = Path(args.fixtures)
    work = Path(args.work)
    if work.exists():
        shutil.rmtree(work)
    work.mkdir(parents=True)

    results = []
    for script_path in sorted(fixtures.glob("*.json")):
        script = json.loads(script_path.read_text(encoding="utf-8"))
        brush = (script_path.parent / script["brush"]).resolve()
        case = work / script_path.stem
        (case / "brushes").mkdir(parents=True)
        shutil.copy(brush, case / "brushes" / brush.name)
        report = case / "gate.md"
        subprocess.run(
            [sys.executable, args.gate,
             "--brushes", str(case / "brushes"),
             "--script", str(script_path),
             "--work", str(case / "run"),
             "--ref", args.ref,
             "--rust", args.rust,
             "--tolerance", str(args.tolerance),
             "--markdown", str(report)],
            check=True, capture_output=True, text=True,
        )
        rows = [line for line in report.read_text(encoding="utf-8").splitlines()
                if "|" in line and "status" not in line and "---" not in line]
        status = rows[-1].split("|")[2].strip() if rows else "NO-ROW"
        results.append((script_path.stem, brush.name, status))
        print(f"{script_path.stem:26} {brush.name:18} {status}")

    identical = sum(1 for _, _, status in results if status == "IDENTICAL")
    print(f"\nfixtures={len(results)} identical={identical} "
          f"other={len(results) - identical} tolerance={args.tolerance}")
    return 0 if identical == len(results) and results else 1


if __name__ == "__main__":
    sys.exit(main())
