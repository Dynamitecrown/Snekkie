#!/usr/bin/env python3
"""Keep public documentation within the published product's scope."""

from __future__ import annotations

import argparse
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys


DOC_EXTENSIONS = {".md", ".markdown", ".rst", ".txt", ".adoc"}
EXCLUDED_TOP_LEVEL = {"documents", "target", ".git"}
PRIVATE_FEATURE = re.compile(
    r"\binvade(?:\b|_)|\bmini[\s-]?games?\b|\bspace[\s-]+invaders\b|\beaster[\s-]?eggs?\b",
    re.IGNORECASE,
)
PRIVATE_LINK = re.compile(r"\]\((?:[^)]*[/\\])?documents[/\\]", re.IGNORECASE)


def is_public_doc(path: str) -> bool:
    parts = PurePosixPath(path).parts
    if not parts or parts[0].lower() in EXCLUDED_TOP_LEVEL:
        return False
    name = parts[-1].lower()
    if name.startswith("readme"):
        return True
    if PurePosixPath(name).suffix not in DOC_EXTENSIONS:
        return False
    return (
        parts[0].lower() == "docs"
        or name.startswith(("changelog", "changes", "release-notes", "release_notes", "releasenotes"))
        or any(part.lower() in {"releases", "release-notes", "release_notes", "changelog"} for part in parts[:-1])
    )


def check(root: Path) -> tuple[int, list[str]]:
    listed = subprocess.run(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
        cwd=root,
        check=True,
        stdout=subprocess.PIPE,
    ).stdout
    paths = sorted({path for path in listed.decode("utf-8").split("\0") if is_public_doc(path)})
    failures = []
    checked = 0
    for path in paths:
        source = root / path
        if not source.is_file():
            continue  # A tracked document may have been removed in this checkout.
        checked += 1
        for number, line in enumerate(source.read_text(encoding="utf-8-sig").splitlines(), 1):
            if PRIVATE_FEATURE.search(line):
                failures.append(f"{path}:{number}: private feature advertised in public documentation")
            if PRIVATE_LINK.search(line):
                failures.append(f"{path}:{number}: public link points into private project notes")
    return checked, failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    args = parser.parse_args()
    try:
        checked, failures = check(args.root.resolve())
    except (OSError, UnicodeError, subprocess.CalledProcessError) as error:
        print(f"Public documentation check could not run: {error}", file=sys.stderr)
        return 2
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    print(f"Public documentation check passed ({checked} files).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
