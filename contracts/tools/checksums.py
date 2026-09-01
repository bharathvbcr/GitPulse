#!/usr/bin/env python3
"""Write or verify contracts/CHECKSUMS.

The vendored copies in GitPulse and Manvi are checked against this file, so a
contract edited in a consumer instead of here fails that consumer's CI.

Usage:
  python3 contracts/tools/checksums.py --write     # regenerate CHECKSUMS
  python3 contracts/tools/checksums.py             # verify, exit 1 on drift
"""

from __future__ import annotations

import hashlib
import pathlib
import sys

# Every contract file, but not CHECKSUMS itself and not the tools that
# generate it — those are machinery, not contract.
TRACKED = [
    "README.md",
    "verdict.schema.json",
    "verdict.cases.json",
    "event.schema.json",
    "lease.schema.md",
]


def digest(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> int:
    root = pathlib.Path(__file__).resolve().parents[1]
    checksums = root / "CHECKSUMS"

    missing = [n for n in TRACKED if not (root / n).exists()]
    if missing:
        print(f"contract files missing: {missing}", file=sys.stderr)
        return 2

    lines = [f"{digest(root / n)}  {n}" for n in TRACKED]
    body = "\n".join(lines) + "\n"

    if "--write" in sys.argv:
        checksums.write_text(body)
        print(f"wrote {checksums.name}: {len(TRACKED)} files")
        return 0

    if not checksums.exists():
        print("CHECKSUMS is missing; run with --write", file=sys.stderr)
        return 2

    have = checksums.read_text()
    if have != body:
        print("contract checksums do not match CHECKSUMS:", file=sys.stderr)
        want_map = dict(line.split("  ", 1)[::-1] for line in body.strip().split("\n"))
        have_map = dict(line.split("  ", 1)[::-1] for line in have.strip().split("\n"))
        for name in TRACKED:
            if want_map.get(name) != have_map.get(name):
                print(f"  {name}: recorded {have_map.get(name)}, actual {want_map.get(name)}", file=sys.stderr)
        print("\nIf the change was intended, run: python3 contracts/tools/checksums.py --write", file=sys.stderr)
        return 1

    print(f"CHECKSUMS ok: {len(TRACKED)} files")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
