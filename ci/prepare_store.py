#!/usr/bin/env python3
"""Prepare a private SDK store from an independently pinned portable bundle."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
FILES = frozenset({'sdk_store.py', 'common.py', 'provision.py',
                   'sdk-packages.json', 'sources.json', 'prepare_store.py'})
TOKEN_ENV = 'NDI_SDK_ARCHIVE_TOKEN'


def digest(path):
    result = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            result.update(block)
    return result.hexdigest()


def validate_bundle(bundle):
    """Validate every helper before importing it; the caller pins the outer tar."""
    bundle = Path(bundle).resolve(strict=True)
    if {p.name for p in bundle.iterdir()} != FILES | {'bootstrap.json'}:
        raise ValueError('bootstrap bundle must contain exactly the seven reviewed files')
    for name in FILES | {'bootstrap.json'}:
        info = (bundle/name).lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_size > 16 * 1024 * 1024:
            raise ValueError('bootstrap bundle contains a nonregular or oversized file')
    manifest = json.loads((bundle/'bootstrap.json').read_text(encoding='utf-8'))
    if (not isinstance(manifest, dict) or set(manifest) != {'schema', 'packages', 'install_packages', 'files'}
            or manifest['schema'] != 1 or not isinstance(manifest['files'], dict)
            or set(manifest['files']) != FILES):
        raise ValueError('invalid bootstrap manifest')
    for name, expected in manifest['files'].items():
        if not isinstance(expected, str) or not re.fullmatch('[0-9a-f]{64}', expected) or digest(bundle/name) != expected:
            raise ValueError('bootstrap file SHA256 mismatch: ' + name)
    for field in ('packages', 'install_packages'):
        values = manifest[field]
        if (not isinstance(values, list) or any(not isinstance(value, str) for value in values)
                or values != sorted(set(values)) or not values):
            raise ValueError('bootstrap package lists must be nonempty, sorted and unique')
    return manifest


def normalized_sources(catalog, sources, provision):
    """Portable bundles contain only private GitHub locators, never credentials."""
    entries = provision.source_map(sources)
    normalized = {}
    used = set()
    for key in sorted(catalog):
        package = provision.resolved_package(key, catalog)
        choices = [name for name in (key, package['artifact_id']) if name in entries]
        if len(choices) != 1:
            raise ValueError('each bootstrap package needs exactly one private GitHub source')
        selected = choices[0]
        used.add(selected)
        entry = entries[selected]
        if set(entry) != {'github_release_asset'}:
            raise ValueError('bootstrap sources must use private GitHub release assets')
        asset = provision.github_asset(entry['github_release_asset'])
        if asset['token_env'] != TOKEN_ENV:
            raise ValueError('bootstrap GitHub sources must use ' + TOKEN_ENV)
        normalized[key] = {'github_release_asset': asset}
    if used != set(entries):
        raise ValueError('bootstrap sources contain an unselected package')
    return {'schema': 1, 'packages': normalized}


def store_environment(sources, provision):
    result = provision.child_environment(sources=sources)
    # The store's Python subprocesses do not need acquisition or interpreter
    # injection settings inherited from the outer host.
    for key in ('GH_TOKEN', 'GITHUB_TOKEN', 'GH_ENTERPRISE_TOKEN', 'GITHUB_ENTERPRISE_TOKEN',
                'GH_DEBUG', 'PYTHONPATH', 'PYTHONHOME', 'PYTHONSTARTUP', 'PYTHONINSPECT',
                'BASH_ENV', 'ENV', 'LD_PRELOAD', 'LD_AUDIT', 'DYLD_INSERT_LIBRARIES'):
        result.pop(key, None)
    return result


def prepare(bundle, store):
    bundle = Path(bundle).resolve(strict=True)
    manifest = validate_bundle(bundle)
    store = Path(store).expanduser().resolve()
    if store.is_relative_to(bundle) or bundle.is_relative_to(store):
        raise ValueError('bootstrap bundle and its private source map must remain outside the SDK store')
    # No bundle code is imported until the independently pinned manifest passes.
    sys.path.insert(0, str(bundle))
    import provision
    import sdk_store
    catalog = sdk_store.reviewed_catalog(bundle/'sdk-packages.json')
    if manifest['packages'] != sorted(catalog):
        raise ValueError('bootstrap package set differs from its exact catalog')
    packages = {key: provision.resolved_package(key, catalog) for key in catalog}
    if manifest['install_packages'] != sorted(key for key, p in packages.items() if p['platform'] in ('linux', 'android')):
        raise ValueError('bootstrap install set must select exactly the Linux and Android catalog packages')
    sources = normalized_sources(catalog, bundle/'sources.json', provision)
    environment = store_environment(sources, provision)
    if (store/'index.json').exists():
        sdk_store.verify(store)
        index = sdk_store.load_index(store)
    else:
        index = sdk_store.empty_index()

    def store_command(action, key, archive=None):
        command = [sys.executable, str(bundle/'sdk_store.py'), '--store', str(store),
                   action, key, '--catalog', str(bundle/'sdk-packages.json')]
        if archive is not None:
            command += ['--archive', str(archive)]
        subprocess.run(command, check=True, env=environment, stdout=subprocess.DEVNULL)

    for key in manifest['packages']:
        package = packages[key]
        artifact = package['artifact_id']
        if artifact in index['archives']:
            if index['archives'][artifact]['catalog_entry'] != package:
                raise ValueError('retained artifact metadata differs from the bootstrap catalog')
            if index['packages'].get(key) != artifact:
                store_command('retain', key)
            continue
        print('Retaining verified SDK archive: ' + key, flush=True)
        with tempfile.TemporaryDirectory(prefix='ndi-private-acquisition-') as temporary:
            archive = Path(temporary)/'archive'
            provision.download_github_asset(sources['packages'][key]['github_release_asset'],
                                            package['sha256'], archive)
            store_command('retain', key, archive)
    for key in manifest['install_packages']:
        print('Preparing SDK installation: ' + key, flush=True)
        store_command('populate', key)
    installed = sdk_store.verify(store)
    index = sdk_store.load_index(store)
    for key, package in packages.items():
        if index['packages'].get(key) != package['artifact_id']:
            raise ValueError('prepared store is missing an exact selected artifact')
    return {'schema': 1, 'retained_packages': manifest['packages'],
            'installed_packages': manifest['install_packages'], 'installed_artifacts': len(installed)}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--store', required=True, type=Path)
    args = parser.parse_args(argv)
    try:
        result = prepare(Path(__file__).resolve().parent, args.store)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print('SDK bootstrap failed: ' + str(error), file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
