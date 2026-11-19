#!/usr/bin/env python3
"""Validate a release tag and select the commit for publishing.

    python ci/release.py v3.0.0
    python ci/release.py --prepare
"""
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib

from verified_tree import find_run

MANIFEST = Path(__file__).resolve().parents[1] / 'Cargo.toml'
TAG = re.compile(r'v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?'
                 r'(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?')


def crate_version(manifest=MANIFEST):
    return tomllib.loads(Path(manifest).read_text(encoding='utf-8'))['package']['version']


def mismatch(tag, version):
    """Return why `tag` does not release `version`, or None when it does."""
    if tag == 'v' + version:
        return None
    return f'tag {tag} does not release crate version {version}; expected v{version}'


def git(*arguments):
    return subprocess.check_output(
        ['git', *arguments], text=True, stderr=subprocess.PIPE, timeout=15,
    ).strip()


def release_commit(tag, event, ref):
    """Select the existing tag from a full checkout and read its crate version."""
    if TAG.fullmatch(tag) is None:
        raise ValueError('expected a v<version> release tag')

    tag_ref = f'refs/tags/{tag}'
    try:
        commit = git('rev-parse', '--verify', f'{tag_ref}^{{commit}}')
    except subprocess.CalledProcessError:
        raise ValueError(f'tag {tag} does not exist or does not point to a commit') from None

    if event == 'push':
        if ref != tag_ref or commit != git('rev-parse', 'HEAD'):
            raise ValueError(f'tag {tag} does not match the pushed commit')
    elif event != 'workflow_dispatch' or ref != 'refs/heads/main':
        raise ValueError('start a release by pushing the tag or retry it from main')

    ancestry = subprocess.run(
        ['git', 'merge-base', '--is-ancestor', commit, 'refs/remotes/origin/main'],
        capture_output=True, timeout=15,
    )
    if ancestry.returncode != 0:
        raise ValueError(f'could not verify that tag {tag} points to a commit of main')

    version = tomllib.loads(git('show', f'{commit}:Cargo.toml'))['package']['version']
    if problem := mismatch(tag, version):
        raise ValueError(problem)
    return commit, version


def prepare(env):
    tag = env['RELEASE_TAG']
    commit, version = release_commit(tag, env['GITHUB_EVENT_NAME'], env['GITHUB_REF'])
    print(f'Selected {tag} at {commit}.')
    run = find_run(env, commit)
    with open(env['GITHUB_OUTPUT'], 'a', encoding='utf-8') as output:
        print(f'commit={commit}', f'version={version}', sep='\n', file=output)
        if run is not None:
            print(f'run_id={run}', file=output)


def main(argv=None):
    arguments = sys.argv[1:] if argv is None else argv
    if arguments == ['--prepare']:
        try:
            prepare(os.environ)
        except (KeyError, OSError, TypeError, ValueError, subprocess.SubprocessError) as error:
            print(f'::error::{error}')
            return 1
        return 0
    if len(arguments) != 1:
        print(__doc__, file=sys.stderr)
        return 2
    problem = mismatch(arguments[0], crate_version())
    if problem:
        print(f'::error::{problem}')
    return int(bool(problem))


if __name__ == '__main__':
    raise SystemExit(main())
