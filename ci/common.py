"""Shared, SDK-free CI policy and evidence helpers (Python standard library only)."""
from __future__ import annotations
import csv
import hashlib
import json
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
SCHEMA = 1
FEATURE_SETS = ('', 'image-encoding', 'tokio', 'smol', 'tokio,smol',
                'advanced_sdk', 'advanced', 'file-io', 'advanced,file-io,tokio,smol')


def digest(path: Path) -> str:
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def plan_digest(plan) -> str:
    """Bind evidence to canonical plan contents, independent of JSON whitespace."""
    encoded = json.dumps(plan, sort_keys=True, separators=(',', ':'), ensure_ascii=True)
    return hashlib.sha256(encoded.encode('utf-8')).hexdigest()


def read_json(path: Path):
    return json.loads(path.read_text(encoding='utf-8'))


def write_json(path: Path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + '\n', encoding='utf-8')


def git_identity(root: Path = ROOT):
    def git(*args):
        return subprocess.check_output(['git', '-C', str(root), *args], text=True).strip()
    changes = git('status', '--porcelain', '--untracked-files=all').splitlines()
    # This single generated manifest is transferred from the trusted plan job.
    # Untracked source files otherwise make exact-commit evidence invalid.
    changes = [line for line in changes if line != '?? ci-plan.json']
    return {'commit': git('rev-parse', 'HEAD'), 'tree': git('rev-parse', 'HEAD^{tree}'),
            'dirty': bool(changes)}


def profiles(root: Path = ROOT):
    with (root / 'build_profiles.tsv').open(encoding='utf-8') as stream:
        lines = [line for line in stream if line.strip() and not line.startswith('#')]
    rows = list(csv.DictReader(lines, delimiter='\t'))
    required = {'id', 'target', 'os', 'arch', 'subdir', 'editions', 'linkage'}
    if not rows or not required.issubset(rows[0]):
        raise ValueError('build_profiles.tsv has no profiles or required columns')
    if len({r['id'] for r in rows}) != len(rows):
        raise ValueError('duplicate build profile ID')
    return {r['id']: r for r in rows}


def secret_key(value: str) -> str:
    return ''.join(c.upper() if c.isalnum() else '_' for c in value)


def package_key(version, edition, os_name):
    platform = 'apple' if os_name in ('macos', 'ios', 'tvos', 'visionos') else os_name
    return f'{version}-{edition}-{platform}'


def suite_fingerprint(root: Path = ROOT):
    paths = [root / 'build_profiles.tsv']
    for directory in ('ci', 'tools/sdk'):
        paths.extend(p for p in (root / directory).rglob('*')
                     if p.is_file() and p.suffix in ('.py', '.json', '.toml', '.md', '.c', '.rs'))
    # Windows Path comparison also folds case; sort canonical string keys so
    # README.md and lower-case filenames have the same order on every host.
    entries = (f'{p.relative_to(root).as_posix()}:{digest(p)}\n' for p in paths if p.exists())
    return hashlib.sha256(''.join(sorted(entries)).encode()).hexdigest()


def native_modes(cell):
    if cell['suite'] == 'features':
        return [f'core-{index}' for index in range(len(FEATURE_SETS))]
    if cell['suite'] not in ('native', 'compat'):
        return []
    modes = ['core']
    if cell['suite'] != 'native' or cell['headers'] != 'current':
        return modes
    if cell['edition'] == 'advanced':
        modes.append('advanced')
    if cell['profile'] == 'windows-x86':
        modes.append('file-platform-negative')
    elif cell['edition'] == 'advanced' or cell['profile'] == 'windows-x64':
        modes.append('file')
    else:
        modes.append('file-negative')
    return modes


def native_configuration(label):
    """Bind a required native execution label to its exact Cargo features."""
    if label in ('core', 'regressions', 'advanced', 'file', 'file-negative', 'file-platform-negative'):
        return label, 'all'
    for index in range(len(FEATURE_SETS)):
        if label == f'core-{index}':
            return 'core', str(index)
    raise ValueError('unknown native execution configuration')


def feature_flags(selection):
    if selection == 'all':
        return ['--all-features']
    for index, features in enumerate(FEATURE_SETS):
        if selection == str(index):
            return ['--no-default-features'] + (['--features', features] if features else [])
    raise ValueError('unknown Cargo feature selection')


def required_steps(cell):
    labels = {'rustc-version', 'build-library' if cell['suite'] == 'mobile' else 'link-examples', 'abi-probe'}
    if cell['headers'] == 'current':
        labels |= {'discovery-abi-probe', 'file-abi-probe'}
    if cell['profile'] not in ('ios-arm64', 'ios-x86_64-sim', 'tvos-arm64', 'tvos-x86_64-sim', 'visionos-arm64'):
        labels.add('link-sdk-smoke')
    if cell['suite'] in ('native', 'compat'):
        labels |= {'link-runtime-probe', 'runtime-identity', 'tests'}
    labels |= {'native-' + mode for mode in native_modes(cell)}
    if cell['suite'] == 'native':
        labels |= {'sdk-regressions', 'clippy', 'fresh-build'}
        if cell['profile'] == 'x86_64-linux-gnu' and cell['headers'] == 'current':
            labels |= {'sdk-snapshot', 'sdk-api-coverage'}
    elif cell['suite'] == 'msrv':
        labels.add('msrv-check')
    elif cell['suite'] == 'features':
        labels |= {f'features-{i}' for i in range(len(FEATURE_SETS))} | {'rustdoc'}
    elif cell['suite'] == 'mobile':
        labels.add('mobile-package')
        if cell['coverage'] in ('emulator', 'simulator'):
            labels.add('mobile-runtime')
    elif cell['suite'] == 'semver':
        labels.add('semver')
    elif cell['suite'] not in ('link', 'compat'):
        raise ValueError('unknown validation suite')
    return labels
