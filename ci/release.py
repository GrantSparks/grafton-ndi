#!/usr/bin/env python3
"""Check that a release tag names the crate version.

    python ci/release.py "$GITHUB_REF_NAME"

The tag must be `v` followed by the `version` in Cargo.toml.
"""
from pathlib import Path
import sys
import tomllib

MANIFEST = Path(__file__).resolve().parents[1] / 'Cargo.toml'


def crate_version(manifest=MANIFEST):
    return tomllib.loads(Path(manifest).read_text(encoding='utf-8'))['package']['version']


def mismatch(tag, version):
    """Return why `tag` does not release `version`, or None when it does."""
    if tag == 'v' + version:
        return None
    return f'tag {tag} does not release crate version {version}; expected v{version}'


def main(argv=None):
    arguments = sys.argv[1:] if argv is None else argv
    if len(arguments) != 1:
        print(__doc__, file=sys.stderr)
        return 2
    problem = mismatch(arguments[0], crate_version())
    if problem:
        print(f'::error::{problem}')
    return 1 if problem else 0


if __name__ == '__main__':
    raise SystemExit(main())
