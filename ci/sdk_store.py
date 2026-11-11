#!/usr/bin/env python3
"""Offline, relocatable SDK store and explicit local development selection.

SDK data stays outside the checkout. Populate accepts only verified local archives;
verify/list/exec are read-only and work from a read-only container mount.
"""
from __future__ import annotations
import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import stat
import subprocess
import sys
import tempfile

# A portable store is deliberately mounted read-only. Importing its helpers must
# not create bytecode files or trigger any mutation in that mount.
sys.dont_write_bytecode = True
from common import digest, read_json, write_json
import provision

HERE = Path(__file__).resolve().parent
SCHEMA = 1
HELPERS = ('sdk_store.py', 'common.py', 'provision.py', 'sdk-packages.json')
CATALOG_FIELDS = {'artifact_id', 'version', 'edition', 'platform', 'sha256',
                  'archive_format', 'version_evidence'}

# This launcher is deliberately stable across helper-generation updates. Its
# legacy handoff keeps existing stores usable if migration stops before the
# atomic index replacement. It imports no mutable sibling helper beforehand.
BOOTSTRAP = '''#!/usr/bin/env python3
"""Read-only launcher for an atomically selected SDK helper generation."""
import hashlib
import json
from pathlib import Path
import runpy
import sys
sys.dont_write_bytecode = True

def fingerprint(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()

def main():
    store = Path(__file__).resolve().parent.parent
    arguments = sys.argv[1:]
    for offset, argument in enumerate(arguments):
        if argument == '--store':
            if offset + 1 >= len(arguments):
                raise ValueError('--store requires a path')
            store = Path(arguments[offset + 1]).resolve()
            break
        if argument.startswith('--store='):
            store = Path(argument.split('=', 1)[1]).resolve()
            break
        if argument in ('retain', 'populate', 'verify', 'verify-store', 'list', 'exec'):
            break
    index = json.loads((store / 'index.json').read_text())
    expected = index.get('tools', {})
    relative = index.get('tool_generation')
    if relative is None:
        handoff = json.loads((store / 'tools/legacy-handoff.json').read_text())
        if handoff.get('schema') != 1 or handoff.get('legacy_tools_sha256') != fingerprint(expected):
            raise ValueError('legacy helper handoff does not match the active index')
        expected, relative = handoff['tools'], handoff['tool_generation']
    if set(expected) != {'sdk_store.py', 'common.py', 'provision.py', 'sdk-packages.json'}:
        raise ValueError('helper generation inventory is incomplete')
    if relative != 'tools/generations/' + fingerprint(expected):
        raise ValueError('helper generation path contradicts its identity')
    generation = (store / relative).resolve()
    if not generation.is_relative_to(store.resolve()):
        raise ValueError('helper generation escapes the store')
    if {path.name for path in generation.iterdir()} != set(expected):
        raise ValueError('helper generation contains unreviewed files')
    for name, wanted in expected.items():
        path = (generation / name).resolve(strict=True)
        if not path.is_relative_to(generation) or hashlib.sha256(path.read_bytes()).hexdigest() != wanted:
            raise ValueError('helper generation content changed: ' + name)
    sys.path.insert(0, str(generation))
    runpy.run_path(str(generation / 'sdk_store.py'), run_name='__main__')

if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, KeyError) as error:
        print('SDK store launcher failed: ' + str(error), file=sys.stderr)
        raise SystemExit(1)
'''


def tools_identity(hashes):
    return hashlib.sha256(json.dumps(hashes, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def reviewed_catalog(catalog):
    data = provision.catalog_packages(catalog)
    if not isinstance(data, dict) or any(not isinstance(row, dict) or set(row) - CATALOG_FIELDS
                                         for row in data.values()):
        raise ValueError('store catalog must contain only reviewed artifact metadata, not source locations')
    return data


def empty_index():
    return {'schema': SCHEMA, 'packages': {}, 'archives': {}, 'artifacts': {}}


def contained(root, relative):
    relative = Path(relative)
    target = (root / relative).resolve()
    if relative.is_absolute() or '..' in relative.parts or not target.is_relative_to(root.resolve()):
        raise ValueError('store contains an escaping relative path')
    return target


def load_index(store):
    data = read_json(store / 'index.json')
    if data.get('schema') != SCHEMA or any(not isinstance(data.get(key), dict) for key in ('packages', 'archives', 'artifacts')):
        raise ValueError('unsupported or malformed SDK store index')
    return data


def inventory(root):
    """Inventory the complete retained payload, including docs and its receipt."""
    entries = []
    for path in sorted(root.rglob('*')):
        if path == root / 'manifest.json':
            continue
        name = path.relative_to(root).as_posix()
        if path.is_symlink():
            if not path.resolve(strict=True).is_relative_to(root.resolve()):
                raise ValueError('store artifact contains an external symlink')
            entries.append({'path': name, 'kind': 'symlink', 'target': os.readlink(path)})
        elif path.is_file():
            entries.append({'path': name, 'kind': 'file', 'bytes': path.stat().st_size,
                            'sha256': digest(path), 'mode': stat.S_IMODE(path.stat().st_mode)})
        elif path.is_dir():
            entries.append({'path': name, 'kind': 'directory'})
        else:
            raise ValueError('store artifact contains a special file')
    return entries


def verify_tools(store, index):
    expected = index.get('tools', {})
    if set(expected) != set(HELPERS):
        raise ValueError('store helper inventory is incomplete')
    relative = index.get('tool_generation')
    if relative is not None:
        if relative != 'tools/generations/' + tools_identity(expected):
            raise ValueError('store helper generation path contradicts its identity')
        base = contained(store, relative)
        if {path.name for path in base.iterdir()} != set(expected):
            raise ValueError('store helper generation contains unreviewed files')
        if digest(store/'tools/sdk_store.py') != index.get('bootstrap_sha256'):
            raise ValueError('store bootstrap content changed')
    else:
        # A captured legacy generation permits a read-only launcher handoff
        # while the still-active legacy index remains valid after interruption.
        captured = contained(store, 'tools/generations/' + tools_identity(expected))
        base = captured if captured.is_dir() else store/'tools'
        if captured.is_dir() and digest(store/'tools/sdk_store.py') not in (
                expected['sdk_store.py'], hashlib.sha256(BOOTSTRAP.encode()).hexdigest()):
            raise ValueError('legacy store launcher content changed')
        if captured.is_dir() and digest(store/'tools/sdk_store.py') == hashlib.sha256(BOOTSTRAP.encode()).hexdigest():
            handoff = read_json(store/'tools/legacy-handoff.json')
            hashes = handoff.get('tools', {})
            relative = handoff.get('tool_generation')
            if (handoff.get('schema') != 1 or handoff.get('legacy_tools_sha256') != tools_identity(expected)
                    or set(hashes) != set(HELPERS) or relative != 'tools/generations/' + tools_identity(hashes)):
                raise ValueError('legacy helper handoff does not match its active index')
            generation = contained(store, relative)
            if {path.name for path in generation.iterdir()} != set(hashes):
                raise ValueError('legacy handoff generation contains unreviewed files')
            for name, value in hashes.items():
                path = (generation/name).resolve()
                if not path.is_relative_to(generation) or not path.is_file() or digest(path) != value:
                    raise ValueError('legacy handoff helper content changed: ' + name)
    for name, fingerprint in expected.items():
        path = (base/name).resolve()
        if not path.is_relative_to(base.resolve()):
            raise ValueError('store helper escapes its generation')
        if not path.is_file() or digest(path) != fingerprint:
            raise ValueError('store helper content changed: ' + name)


def resolve_selection(index, selection):
    artifact = index['packages'].get(selection, selection)
    if artifact not in index['artifacts']:
        raise ValueError('SDK selection is not installed: ' + selection)
    return artifact, index['artifacts'][artifact]


def verify_archive(store, artifact, entry):
    package = provision.resolved_package(entry['package'], {entry['package']: entry['catalog_entry']})
    suffix = {'embedded-tar': '.tar.gz', 'apple-pkg': '.pkg', 'windows-installer': '.exe'}[package['archive_format']]
    expected = 'archives/' + package['sha256'] + suffix
    if package['artifact_id'] != artifact or entry.get('path') != expected:
        raise ValueError('retained archive index contradicts reviewed artifact identity')
    path = contained(store, entry['path'])
    if not path.is_file() or path.stat().st_size != entry.get('bytes') or digest(path) != package['sha256']:
        raise ValueError('retained archive changed: ' + artifact)
    return path


def verify_artifact(store, artifact, entry):
    package = provision.resolved_package(entry['package'], {entry['package']: entry['catalog_entry']})
    if package['artifact_id'] != artifact or entry.get('archive_sha256') != package['sha256']:
        raise ValueError('store index artifact identity contradicts its reviewed package')
    location = contained(store, entry['directory'])
    if location != contained(store, 'artifacts/' + artifact):
        raise ValueError('store artifact directory does not match immutable identity')
    manifest = location / 'manifest.json'
    if not manifest.is_file() or digest(manifest) != entry.get('manifest_sha256'):
        raise ValueError('store artifact manifest changed: ' + artifact)
    observed = read_json(manifest)
    if observed.get('schema') != 1 or observed.get('artifact_id') != artifact or observed.get('inventory') != inventory(location):
        raise ValueError('store artifact content changed: ' + artifact)
    receipt = provision.verify_installation(entry['package'], location,
                                           catalog={entry['package']: entry['catalog_entry']})
    root = contained(store, entry['root'])
    if root != Path(receipt['root']) or entry.get('receipt') != provision.sanitized_receipt(receipt):
        raise ValueError('store index and installed receipt disagree')
    return dict(entry, root=str(root), artifact_id=artifact)


def verified_index(store):
    """Check shared metadata/helpers without reading unrelated SDK payloads."""
    index = load_index(store)
    verify_tools(store, index)
    if any(artifact not in index['archives'] for artifact in index['packages'].values()):
        raise ValueError('store package alias references a missing artifact')
    if any(artifact not in index['archives'] for artifact in index['artifacts']):
        raise ValueError('installed SDK lacks its retained original archive')
    return index


def verify_selected(store, selections):
    """Fully verify each activated artifact once; unrelated archives stay unread."""
    store = Path(store).resolve(strict=True)
    index = verified_index(store)
    chosen = {resolve_selection(index, selection)[0] for selection in selections}
    result = {}
    for artifact in sorted(chosen):
        verify_archive(store, artifact, index['archives'][artifact])
        result[artifact] = verify_artifact(store, artifact, index['artifacts'][artifact])
    return index, result


def verify(store, selection=None):
    store = Path(store).resolve(strict=True)
    index = verified_index(store)
    for artifact, entry in sorted(index['archives'].items()):
        verify_archive(store, artifact, entry)
    if selection:
        artifact = index['packages'].get(selection, selection)
        if artifact not in index['archives']:
            raise ValueError('SDK selection is not retained: ' + selection)
        chosen = [(artifact, index['artifacts'][artifact])] if artifact in index['artifacts'] else []
    else:
        chosen = sorted(index['artifacts'].items())
    return [verify_artifact(store, artifact, entry) for artifact, entry in chosen]


@contextmanager
def mutation_lock(store):
    store.mkdir(parents=True, exist_ok=True, mode=0o700)
    lock = store / '.populate.lock'
    try:
        descriptor = os.open(lock, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError:
        raise ValueError('store population is already locked; inspect a stale lock before removing it') from None
    try:
        with os.fdopen(descriptor, 'w') as stream:
            stream.write(str(os.getpid()) + '\n')
        yield
    finally:
        lock.unlink()


def snapshot_tools(store, catalog=None, *, source=None, expected=None):
    """Publish an immutable bundle without changing any active helper."""
    generations = store/'tools/generations'
    generations.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.generation-', dir=generations) as temporary:
        stage = Path(temporary)/'bundle'
        stage.mkdir()
        for name in HELPERS:
            if source is None and name == 'sdk-packages.json':
                write_json(stage/name, reviewed_catalog(catalog))
            else:
                shutil.copyfile((source if source is not None else HERE)/name, stage/name)
        hashes = {name: digest(stage/name) for name in HELPERS}
        if expected is not None and hashes != expected:
            raise ValueError('legacy helpers changed while their generation was captured')
        relative = 'tools/generations/' + tools_identity(hashes)
        destination = store/relative
        if destination.exists():
            if ({path.name for path in destination.iterdir()} != set(hashes)
                    or any(not (destination/name).is_file() or digest(destination/name) != value for name,value in hashes.items())):
                raise ValueError('existing immutable helper generation changed')
        else:
            stage.rename(destination)
    return {'tools': hashes, 'tool_generation': relative,
            'bootstrap_sha256': hashlib.sha256(BOOTSTRAP.encode()).hexdigest()}


def publish_bootstrap(store, previous, generation):
    launcher = store/'tools/sdk_store.py'
    if previous is not None and 'tool_generation' not in previous:
        # Capture the old complete bundle before touching its launcher. Prepare
        # the handoff before replacing that launcher, so either path remains
        # usable if publication is interrupted at any subsequent step.
        captured = store/'tools/generations'/tools_identity(previous['tools'])
        if not captured.exists():
            snapshot_tools(store, source=store/'tools', expected=previous['tools'])
        handoff = {'schema': 1, 'legacy_tools_sha256': tools_identity(previous['tools']),
                   'tools': generation['tools'], 'tool_generation': generation['tool_generation']}
        temporary = store/'tools/.legacy-handoff.new.json'
        write_json(temporary, handoff)
        temporary.replace(store/'tools/legacy-handoff.json')
    if launcher.is_file() and digest(launcher) == generation['bootstrap_sha256']:
        return
    if previous is not None and 'tool_generation' in previous:
        raise ValueError('stable bootstrap changed; explicit bootstrap migration is required')
    temporary = store/'tools/.sdk_store.new.py'
    temporary.write_text(BOOTSTRAP, encoding='utf-8')
    temporary.replace(launcher)


def save_index(store, index, catalog):
    previous = load_index(store) if (store/'index.json').is_file() else None
    generation = snapshot_tools(store, catalog)
    publish_bootstrap(store, previous, generation)
    index.update(generation)
    temporary = store / '.index.new.json'
    write_json(temporary, index)
    temporary.replace(store / 'index.json')


def retain(store, selection, *, archive=None, sources=None, catalog=None):
    store = Path(store).resolve()
    catalog = catalog if catalog is not None else HERE / 'sdk-packages.json'
    catalog = reviewed_catalog(catalog)
    package = provision.resolved_package(selection, catalog)
    artifact = package['artifact_id']
    with mutation_lock(store):
        index = load_index(store) if (store / 'index.json').exists() else empty_index()
        if (store / 'index.json').exists():
            verify(store)
        if artifact in index['archives'] and (archive is not None or sources is not None):
            source, _ = provision.resolve_source(selection, package, archive=archive, sources=sources, offline=True)
            if digest(source) != package['sha256']:
                raise ValueError('supplied local archive SHA256 differs from the reviewed package')
        if artifact not in index['archives']:
            source, _ = provision.resolve_source(selection, package, archive=archive, sources=sources, offline=True)
            if digest(source) != package['sha256']:
                raise ValueError('local archive SHA256 differs from the reviewed package')
            suffix = {'embedded-tar': '.tar.gz', 'apple-pkg': '.pkg', 'windows-installer': '.exe'}[package['archive_format']]
            relative = 'archives/' + package['sha256'] + suffix
            (store / 'archives').mkdir(exist_ok=True)
            with tempfile.NamedTemporaryFile(prefix='.retain-', dir=store / 'archives', delete=False) as output:
                temporary = Path(output.name)
            try:
                shutil.copyfile(source, temporary)
                if digest(temporary) != package['sha256']:
                    raise ValueError('archive changed while being retained')
                temporary.replace(store / relative)
            finally:
                temporary.unlink(missing_ok=True)
            index['archives'][artifact] = {'package': selection, 'catalog_entry': package,
                                           'path': relative, 'bytes': (store / relative).stat().st_size}
        entry = index['archives'][artifact]
        verify_archive(store, artifact, entry)
        index['packages'][selection] = artifact
        save_index(store, index, catalog)
    return dict(entry, artifact_id=artifact)


def populate(store, selection, *, archive=None, sources=None, catalog=None):
    store = Path(store).resolve()
    catalog = reviewed_catalog(catalog if catalog is not None else HERE / 'sdk-packages.json')
    package = provision.resolved_package(selection, catalog)
    artifact = package['artifact_id']
    # Retention is its own complete transaction. A missing native extractor does
    # not discard successfully retained original bytes for another host.
    retain(store, selection, archive=archive, sources=sources, catalog=catalog)
    with mutation_lock(store):
        index = load_index(store)
        verify(store)
        local_archive = verify_archive(store, artifact, index['archives'][artifact])
        if artifact in index['artifacts']:
            installed = verify_artifact(store, artifact, index['artifacts'][artifact])
        else:
            # Windows provisioning executes an installer on Windows; Apple needs
            # native pkgutil. The store does not emulate those host requirements.
            if package['platform'] == 'apple' and sys.platform != 'darwin':
                raise ValueError('Apple SDK store population requires macOS pkgutil')
            if package['platform'] == 'windows' and sys.platform != 'win32':
                raise ValueError('Windows SDK store population requires a Windows host')
            (store / 'artifacts').mkdir(exist_ok=True)
            with tempfile.TemporaryDirectory(prefix='.sdk-store-', dir=store) as temporary:
                staging = Path(temporary) / 'artifact'
                receipt = provision.provision(selection, staging, archive=local_archive,
                                              catalog=catalog, offline=True)
                # Retain the extracted SDK and its complete receipt. The original
                # installer is unnecessary for a read-only development mount.
                installers = staging / 'installers'
                if installers.exists():
                    shutil.rmtree(installers)
                write_json(staging / 'manifest.json', {
                    'schema': 1, 'artifact_id': artifact, 'inventory': inventory(staging)})
                relative = 'artifacts/' + artifact
                entry = {'package': selection, 'catalog_entry': package, 'directory': relative,
                         'archive_sha256': package['sha256'],
                         'root': relative + '/' + receipt['root_relative'],
                         'manifest_sha256': digest(staging / 'manifest.json'),
                         'receipt': provision.sanitized_receipt(receipt)}
                destination = store / relative
                if destination.exists():
                    # An interrupted index publication can leave a complete,
                    # unindexed SDK. Adopt it only if it matches this fresh
                    # extraction from the reviewed retained archive in full.
                    # The candidate manifest/receipt is authoritative here;
                    # never trust an orphan's self-reported inventory alone.
                    verify_artifact(store, artifact, entry)
                else:
                    staging.rename(destination)
                index['artifacts'][artifact] = entry
                installed = verify_artifact(store, artifact, entry)
        index['packages'][selection] = artifact
        save_index(store, index, catalog)
    return installed


def native_linux_identity():
    if sys.platform != 'linux':
        raise ValueError('SDK command selection currently supports native Linux only')
    arch = platform.machine().lower()
    identities = {
        'x86_64': ('x86_64-linux-gnu', 'x86_64-unknown-linux-gnu', 62, 2),
        'amd64': ('x86_64-linux-gnu', 'x86_64-unknown-linux-gnu', 62, 2),
        # The reviewed native CI profile exists in both SDK editions. Other
        # AArch64 BSP directories are distinct artifacts, not fallback choices.
        'aarch64': ('aarch64-rpi4-linux-gnueabi', 'aarch64-unknown-linux-gnu', 183, 2),
        'arm64': ('aarch64-rpi4-linux-gnueabi', 'aarch64-unknown-linux-gnu', 183, 2),
        'i686': ('i686-linux-gnu', 'i686-unknown-linux-gnu', 3, 1),
        'i386': ('i686-linux-gnu', 'i686-unknown-linux-gnu', 3, 1),
    }
    if arch not in identities:
        raise ValueError('native Linux ABI needs an explicitly implemented host selector: ' + arch)
    return identities[arch]


def native_runtime(entry, identity):
    profile, _, machine, bits = identity
    directory = Path(entry['root']) / 'lib' / profile
    core = 'libndi_advanced.so' if entry['catalog_entry']['edition'] == 'advanced' else 'libndi.so'
    library = directory / core
    if not library.is_file():
        raise ValueError('selected package has no native runtime for ' + profile)
    with library.open('rb') as stream:
        header = stream.read(20)
    if len(header) != 20 or header[:4] != b'\x7fELF' or header[4] != bits or header[5] != 1 or int.from_bytes(header[18:20], 'little') != machine:
        raise ValueError('selected runtime ELF identity does not match native host')
    return directory.resolve()


def private_build_directory(store, base, headers, runtime, target):
    base = Path(base).expanduser().absolute() if base else Path.home() / '.cache/grafton-ndi/private-builds'
    if base.is_symlink():
        raise ValueError('private build root must not be a symlink')
    base = base.resolve()
    if base.is_relative_to(store.resolve()):
        raise ValueError('private build output must be outside the SDK store')
    checkout = subprocess.run(['git', 'rev-parse', '--show-toplevel'], capture_output=True, text=True)
    if checkout.returncode == 0 and base.is_relative_to(Path(checkout.stdout.strip()).resolve()):
        raise ValueError('SDK-derived build output must be outside the source checkout')
    key = hashlib.sha256((headers['artifact_id'] + '\n' + runtime['artifact_id'] + '\n' + target).encode()).hexdigest()
    base.mkdir(parents=True, exist_ok=True, mode=0o700)
    if base.is_symlink() or (hasattr(os, 'getuid') and base.stat().st_uid != os.getuid()):
        raise ValueError('private build root has unexpected ownership or a symlink')
    os.chmod(base, 0o700)
    output = base / key
    if output.is_symlink():
        raise ValueError('private build output must not be a symlink')
    output.mkdir(exist_ok=True, mode=0o700)
    if hasattr(os, 'getuid') and output.stat().st_uid != os.getuid():
        raise ValueError('private build output has unexpected ownership')
    os.chmod(output, 0o700)
    return output


def command_environment(store, headers_id, runtime_id, *, build_root=None, environment=None):
    # Fully hash the selected archives/payloads and shared helper/catalog data.
    # Retaining more historical SDKs must not add unrelated I/O to every build.
    # Resolve both selections from the same verified index snapshot.
    index, verified = verify_selected(store, (headers_id, runtime_id))
    headers = verified[resolve_selection(index, headers_id)[0]]
    runtime = verified[resolve_selection(index, runtime_id)[0]]
    hp, rp = headers['catalog_entry'], runtime['catalog_entry']
    if hp['edition'] != rp['edition'] or hp['platform'] != 'linux' or rp['platform'] != 'linux':
        raise ValueError('native selection requires Linux headers/runtime of the same edition')
    identity = native_linux_identity()
    header_directory = native_runtime(headers, identity)
    runtime_directory = native_runtime(runtime, identity)
    # Shared source-map handling removes custom acquisition token_env/url_env
    # names as well as standard NDI credentials before any compiler/child runs.
    env = provision.child_environment(environment)
    if env.get('CARGO_BUILD_TARGET') not in (None, '', identity[1]):
        raise ValueError('inherited Cargo target conflicts with native SDK command selection')
    old_sdk_paths = [Path(env[name]).resolve() for name in ('NDI_SDK_DIR', 'NDI_RUNTIME_DIR_V6') if env.get(name)]
    loader = []
    for value in env.get('LD_LIBRARY_PATH', '').split(os.pathsep):
        if not value:
            continue
        path = Path(value).resolve()
        has_ndi = path.is_dir() and (any(path.glob('libndi*.so*')) or any(path.glob('libNDI*.so*')))
        if path in (header_directory, runtime_directory) or has_ndi or any(path.is_relative_to(old) for old in old_sdk_paths):
            continue
        loader.append(value)
    for name in list(env):
        if name.startswith(('NDI_', 'GRAFTON_NDI_')):
            env.pop(name)
    for name in ('LD_PRELOAD', 'LD_AUDIT', 'DYLD_INSERT_LIBRARIES'):
        env.pop(name, None)
    env.update({'NDI_SDK_DIR': headers['root'], 'NDI_SDK_EDITION': hp['edition'],
                'NDI_SDK_PROFILE': identity[0], 'NDI_RUNTIME_DIR_V6': str(runtime_directory),
                'LD_LIBRARY_PATH': os.pathsep.join([str(runtime_directory), *loader]),
                'RUSTC_WRAPPER': '', 'RUSTC_WORKSPACE_WRAPPER': '',
                'CARGO_BUILD_RUSTC_WRAPPER': '', 'CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER': '',
                'GMS_CACHE_MODE': 'off', 'SCCACHE_DISABLE': '1', 'CCACHE_DISABLE': '1'})
    env['CARGO_TARGET_DIR'] = str(private_build_directory(Path(store), build_root, headers, runtime, identity[1]))
    return env


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--store', type=Path, required=True)
    commands = parser.add_subparsers(dest='command', required=True)
    retained = commands.add_parser('retain', help='Retain an exact original archive offline, for any platform')
    retained.add_argument('package')
    retained.add_argument('--archive', type=Path)
    retained.add_argument('--sources', type=Path)
    retained.add_argument('--catalog', type=Path, default=HERE / 'sdk-packages.json')
    add = commands.add_parser('populate', help='Offline extraction from a reviewed local archive')
    add.add_argument('package')
    add.add_argument('--archive', type=Path)
    add.add_argument('--sources', type=Path, help='Private schema1 local source map; never copied into store')
    add.add_argument('--catalog', type=Path, default=HERE / 'sdk-packages.json')
    check = commands.add_parser('verify', aliases=['verify-store'])
    check.add_argument('package', nargs='?')
    listing = commands.add_parser('list')
    listing.add_argument('--json', action='store_true')
    execute = commands.add_parser('exec')
    execute.add_argument('--headers', required=True)
    execute.add_argument('--runtime', required=True)
    execute.add_argument('--build-root', type=Path)
    execute.add_argument('argv', nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    try:
        if args.command == 'retain':
            entry = retain(args.store, args.package, archive=args.archive, sources=args.sources, catalog=args.catalog)
            print('Verified retained archive ' + entry['artifact_id'])
        elif args.command == 'populate':
            entry = populate(args.store, args.package, archive=args.archive, sources=args.sources, catalog=args.catalog)
            print('Verified installed artifact ' + entry['artifact_id'])
        elif args.command in ('verify', 'verify-store'):
            entries = verify(args.store, args.package)
            print(f'{len(entries)} installed artifacts and {len(load_index(args.store)["archives"])} retained archives verified')
        elif args.command == 'list':
            entries = verify(args.store)
            aliases = load_index(args.store)['packages']
            result = {'schema': 1, 'packages': aliases, 'artifacts': [
                {'artifact_id': e['artifact_id'], **e['receipt']} for e in entries]}
            result['retained'] = [{
                'artifact_id': artifact, 'package': item['package'],
                'version': item['catalog_entry']['version'], 'edition': item['catalog_entry']['edition'],
                'platform': item['catalog_entry']['platform'], 'sha256': item['catalog_entry']['sha256'],
                'installed': artifact in load_index(args.store)['artifacts']}
                for artifact, item in sorted(load_index(args.store)['archives'].items())]
            if args.json:
                print(json.dumps(result, indent=2, sort_keys=True))
            else:
                for key, artifact in sorted(aliases.items()):
                    state = 'installed' if artifact in load_index(args.store)['artifacts'] else 'retained'
                    print(key + '\t' + state + '\t' + artifact)
        else:
            command = args.argv[1:] if args.argv[:1] == ['--'] else args.argv
            if not command:
                raise ValueError('exec requires a command after --')
            env = command_environment(args.store, args.headers, args.runtime, build_root=args.build_root)
            return subprocess.run(command, env=env).returncode
        return 0
    except (ValueError, OSError, KeyError, subprocess.SubprocessError) as error:
        print('SDK store operation failed: ' + str(error), file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
