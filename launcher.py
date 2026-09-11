"""PyInstaller entry point.

snekkie/__main__.py uses relative imports (it's a package module), so
PyInstaller can't run it directly as the top-level script — that leaves it
with no parent package and an ImportError at startup. This wrapper sits
outside the package and imports snekkie normally, so the relative imports
inside it resolve as they would under `python -m snekkie`.
"""

from snekkie.__main__ import main

if __name__ == "__main__":
    raise SystemExit(main())
