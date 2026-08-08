#!/usr/bin/env python3
"""Extract a CI development artifact into a clean destination."""

from __future__ import annotations

import argparse
import shutil
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    if args.output.exists():
        raise FileExistsError(f"clean extraction destination already exists: {args.output}")
    args.output.mkdir(parents=True)
    shutil.unpack_archive(args.archive, args.output)
    print(f"extracted {args.archive.name} into clean destination")


if __name__ == "__main__":
    main()
