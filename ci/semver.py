#!/usr/bin/env python3
"""Check the public API against the newest published release.

    python ci/semver.py

Runs `cargo semver-checks` with no options: the release type comes from the
version in Cargo.toml and every lint applies.

The check is skipped in one case, when the crate's major version is ahead of
every published release. A new major version may change anything, so the tool
would run no lint.
"""
import json
from pathlib import Path
import re
import subprocess
import sys
import tomllib
import urllib.request

MANIFEST = Path(__file__).resolve().parents[1] / 'Cargo.toml'
INDEX = 'https://index.crates.io/'
INSTALL = ['cargo', 'install', 'cargo-semver-checks', '--version', '0.51.0', '--locked']
CHECK = ['cargo', 'semver-checks']


def index_path(name):
    """Where the crates.io sparse index keeps a crate with a name of four or more characters."""
    name = name.lower()
    return f'{name[:2]}/{name[2:4]}/{name}'


def newest_release(index):
    """The highest version in a crate's index file that is neither yanked nor a pre-release."""
    releases = []
    for line in index.splitlines():
        entry = json.loads(line)
        if not entry['yanked'] and re.fullmatch(r'\d+\.\d+\.\d+', entry['vers']):
            releases.append(tuple(int(part) for part in entry['vers'].split('.')))
    return max(releases, default=None)


def applies(version, newest):
    """Whether a release exists that `version` must stay compatible with."""
    return newest is not None and newest[0] >= int(version.split('.')[0])


def main():
    package = tomllib.loads(MANIFEST.read_text(encoding='utf-8'))['package']
    with urllib.request.urlopen(INDEX + index_path(package['name']), timeout=60) as response:
        newest = newest_release(response.read().decode('utf-8'))
    if not applies(package['version'], newest):
        published = 'nothing is published' if newest is None else 'the newest release is ' + '.'.join(map(str, newest))
        print(f"semver check skipped: {package['version']} is a new major version ({published}), "
              'so every API change is permitted')
        return 0
    for command in (INSTALL, CHECK):
        status = subprocess.run(command).returncode
        if status:
            return status
    return 0


if __name__ == '__main__':
    sys.exit(main())
