#!/usr/bin/env python3
"""Write a stable SHA256SUMS file for explicitly named regular files."""

from __future__ import annotations

import argparse
import hashlib
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("files", nargs="+", type=Path)
    args = parser.parse_args()
    lines = []
    names = set()
    output = args.output.resolve()
    for path in sorted(args.files, key=lambda item: item.name):
        resolved = path.resolve()
        if (
            not path.is_file()
            or path.name != Path(path.name).name
            or "\n" in path.name
            or "\r" in path.name
            or path.name in names
            or resolved == output
        ):
            raise SystemExit(f"unsafe checksum input: {path}")
        names.add(path.name)
        lines.append(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}")
    args.output.write_text("\n".join(lines) + "\n", encoding="ascii", newline="\n")


if __name__ == "__main__":
    main()
