#!/usr/bin/env python3
"""Pinned public GNU ARMHF tools for hosted build/link validation.

This module extracts reviewed Debian packages; it never installs packages or
executes maintainer scripts. Target libraries stay separate from host libraries.
No SDK, credentials, vendor BSP, emulator or device is part of this recipe.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import subprocess
import tarfile
import tempfile
import urllib.parse
import urllib.request

from provision import child_environment

MANIFEST = Path(__file__).with_name('public-toolchains.json')
TRIPLE = 'arm-linux-gnueabihf'
RECEIPT_KEYS = ('package', 'version', 'architecture', 'sha256')
MAX_PACKAGE = 64 * 1024 * 1024
MAX_TOTAL = 512 * 1024 * 1024


def _digest(path):
    value = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            value.update(chunk)
    return value.hexdigest()


def _recipe(profile_id):
    recipe = json.loads(MANIFEST.read_text(encoding='utf-8'))
    if recipe.get('schema') != 1 or profile_id not in recipe.get('profiles', {}):
        raise ValueError('unknown public toolchain profile or recipe schema')
    profile = recipe['profiles'][profile_id]
    if profile.get('target') != 'armv7-unknown-linux-gnueabihf':
        raise ValueError('public recipe supports only GNU ARMv7 hard-float')
    packages = recipe.get('packages', [])
    if not packages or len(packages) > 64:
        raise ValueError('public recipe requires a bounded package list')
    keys = []
    for package in packages:
        if not all(isinstance(package.get(key), str) and package[key] for key in RECEIPT_KEYS):
            raise ValueError('public package identity is incomplete')
        if not re.fullmatch(r'[a-f0-9]{64}', package['sha256']):
            raise ValueError('public package SHA256 is missing or malformed')
        if type(package.get('size')) is not int or not 0 < package['size'] <= MAX_PACKAGE:
            raise ValueError('public package size exceeds limit')
        url = urllib.parse.urlsplit(package.get('url', ''))
        if (url.scheme != 'https' or url.netloc != 'deb.debian.org'
                or not url.path.startswith('/debian/pool/') or not url.path.endswith('.deb')
                or url.query or url.fragment or '..' in PurePosixPath(url.path).parts):
            raise ValueError('public package URL must be a pinned Debian pool artifact')
        if package.get('role') not in ('compiler', 'target-runtime'):
            raise ValueError('public package role is unknown')
        expected_arch = ('amd64', 'all') if package['role'] == 'compiler' else ('armhf',)
        if package['architecture'] not in expected_arch:
            raise ValueError('host and target public package architectures are mixed')
        keys.append((package['package'], package['architecture']))
    if len(set(keys)) != len(keys) or sum(p['size'] for p in packages) > MAX_TOTAL:
        raise ValueError('duplicate packages or excessive recipe download size')
    required = {'gcc-12-arm-linux-gnueabihf', 'cpp-12-arm-linux-gnueabihf',
                'binutils-arm-linux-gnueabihf', 'libgcc-12-dev-armhf-cross',
                'libc6-dev-armhf-cross', 'libc6-armhf-cross',
                'linux-libc-dev-armhf-cross', 'libstdc++6-armhf-cross',
                'libgcc-s1-armhf-cross', 'libavahi-client3', 'libavahi-common3',
                'libdbus-1-3', 'libsystemd0'}
    if not required.issubset(p['package'] for p in packages):
        raise ValueError('public recipe lacks compiler, CRT, headers or runtime dependencies')
    return recipe


def recipe_identity(profile_id):
    """Fingerprint reviewed pins, profile and assembly code, without networking."""
    recipe = _recipe(profile_id)
    identity = {'recipe': recipe, 'profile': profile_id, 'builder_sha256': _digest(__file__)}
    return hashlib.sha256(json.dumps(identity, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def expected_package_receipts(profile_id):
    """Exact sorted receipt schema used by the independent aggregate gate."""
    return [{key: package[key] for key in RECEIPT_KEYS}
            for package in sorted(_recipe(profile_id)['packages'],
                                  key=lambda p: (p['package'], p['architecture']))]


def _download(package, cache):
    path = cache / (package['sha256'] + '.deb')
    if path.exists():
        if path.stat().st_size != package['size'] or _digest(path) != package['sha256']:
            raise ValueError('cached public package fails pinned size/SHA256 verification')
        return path
    with tempfile.NamedTemporaryFile(prefix=package['sha256'] + '.', suffix='.part',
                                     dir=cache, delete=False) as allocation:
        temporary = Path(allocation.name)
    try:
        request = urllib.request.Request(package['url'], headers={'User-Agent': 'public-gnu-toolchain/1'})
        with urllib.request.urlopen(request, timeout=60) as response, temporary.open('wb') as output:
            final = urllib.parse.urlsplit(response.url)
            if final.scheme != 'https' or final.netloc != 'deb.debian.org':
                raise ValueError('public package redirected outside its reviewed HTTPS host')
            size = 0
            while chunk := response.read(1024 * 1024):
                size += len(chunk)
                if size > package['size']:
                    raise ValueError('public package exceeds pinned size')
                output.write(chunk)
        if temporary.stat().st_size != package['size'] or _digest(temporary) != package['sha256']:
            raise ValueError('downloaded public package fails pinned size/SHA256 verification')
        temporary.replace(path)
    finally:
        temporary.unlink(missing_ok=True)
    return path


def _member_path(root, name):
    path = PurePosixPath(name)
    if path.is_absolute() or '..' in path.parts:
        raise ValueError('package contains an escaping member path')
    destination = root.joinpath(*path.parts)
    if not destination.resolve().is_relative_to(root.resolve()):
        raise ValueError('package member follows an escaping symlink')
    return destination


def _snapshot_package(package, archive, destination):
    """Freeze shared-cache bytes before any subprocess sees them."""
    with archive.open('rb') as source, destination.open('xb') as output:
        shutil.copyfileobj(source, output)
    destination.chmod(0o400)
    if destination.stat().st_size != package['size'] or _digest(destination) != package['sha256']:
        raise ValueError('public package changed before its private extraction snapshot was verified')
    return destination


def _extract_deb(package, archive, root):
    """Extract data only, with explicit traversal/link/type checks."""
    actual = subprocess.check_output(
        ['dpkg-deb', '-f', str(archive), 'Package', 'Version', 'Architecture'],
        text=True, env=child_environment())
    fields = dict(line.split(': ', 1) for line in actual.splitlines())
    for source, key in [('Package', 'package'), ('Version', 'version'), ('Architecture', 'architecture')]:
        if fields.get(source) != package[key]:
            raise ValueError('public package metadata differs from reviewed identity')
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    process = subprocess.Popen(['dpkg-deb', '--fsys-tarfile', str(archive)],
                               stdout=subprocess.PIPE, env=child_environment())
    try:
        size = 0
        with tarfile.open(fileobj=process.stdout, mode='r|') as data:
            for member in data:
                destination = _member_path(root, member.name)
                size += member.size
                if size > 1024 * 1024 * 1024:
                    raise ValueError('public package expands beyond extraction limit')
                destination.parent.mkdir(parents=True, exist_ok=True)
                if member.isdir():
                    destination.mkdir(exist_ok=True)
                elif member.isfile():
                    with data.extractfile(member) as source, destination.open('wb') as output:
                        shutil.copyfileobj(source, output)
                    destination.chmod(member.mode & 0o777)
                elif member.issym():
                    target = member.linkname
                    # Package-root absolute links become relocatable bundle-root links.
                    if target.startswith('/'):
                        target = os.path.relpath(root / target.lstrip('/'), destination.parent)
                    if not (destination.parent / target).resolve().is_relative_to(root.resolve()):
                        raise ValueError('public package contains an escaping symbolic link')
                    if destination.is_symlink():
                        destination.unlink()
                    destination.symlink_to(target)
                elif member.islnk():
                    target = _member_path(root, member.linkname)
                    if destination.exists():
                        destination.unlink()
                    os.link(target, destination)
                else:
                    raise ValueError('public package contains an unsupported file type')
        if process.wait() != 0:
            raise ValueError('public package data extraction failed')
    finally:
        process.stdout.close()
        if process.poll() is None:
            process.kill()
            process.wait()


WRAPPER = '''#!/usr/bin/env python3
import os
from pathlib import Path
import sys
root = Path(__file__).resolve().parents[1]
host = root / 'host'
sysroot = root / 'sysroot'
gcc = host / 'usr/lib/gcc-cross/arm-linux-gnueabihf/12'
target = sysroot / 'usr/arm-linux-gnueabihf'
env = os.environ.copy()
for key in ('GCC_EXEC_PREFIX', 'COMPILER_PATH', 'LIBRARY_PATH', 'CPATH',
            'C_INCLUDE_PATH', 'CPLUS_INCLUDE_PATH', 'OBJC_INCLUDE_PATH'):
    env.pop(key, None)
env['LD_LIBRARY_PATH'] = str(host / 'lib/x86_64-linux-gnu') + ':' + str(host / 'usr/lib/x86_64-linux-gnu')
env['COMPILER_PATH'] = str(gcc) + ':' + str(host / 'usr/bin')
exe = str(host / 'usr/bin/arm-linux-gnueabihf-gcc-12')
args = [exe, '-B' + str(gcc) + '/', '-B' + str(host / 'usr/arm-linux-gnueabihf/bin') + '/',
        '-B' + str(target / 'lib') + '/', '--sysroot=' + str(sysroot),
        '-nostdinc', '-isystem', str(gcc / 'include'), '-isystem', str(target / 'include'),
        '-L' + str(target / 'lib'),
        '-Wl,-rpath-link,' + str(target / 'lib') + ':' + str(sysroot / 'lib/arm-linux-gnueabihf')
            + ':' + str(sysroot / 'usr/lib/arm-linux-gnueabihf'),
        '-march=armv7-a', '-mfpu=vfpv3-d16', '-mfloat-abi=hard']
os.execve(exe, args + sys.argv[1:], env)
'''


def _write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + '\n', encoding='utf-8')


def _self_test(root):
    """Compile and inspect ARM ELF; never execute target code."""
    linker = root / 'bin/armhf-gcc'
    source = root / 'self-test.c'
    source.write_text('#include <stdint.h>\n#include <stdlib.h>\nint main(void) { return sizeof(void*) != 4; }\n')
    output = root / 'self-test.elf'
    subprocess.run([str(linker), str(source), '-o', str(output)], check=True, timeout=60,
                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=child_environment())
    header = subprocess.check_output(['readelf', '-h', str(output)], text=True, env=child_environment())
    if 'Machine:                           ARM' not in header or 'hard-float ABI' not in header:
        raise ValueError('public cross compiler did not emit ARM hard-float ELF')
    crt = subprocess.check_output([str(linker), '-print-file-name=crt1.o'],
                                  text=True, env=child_environment()).strip()
    if not Path(crt).resolve().is_relative_to(root.resolve()):
        raise ValueError('public compiler resolves startup objects outside its bundle')
    directories = [root / 'sysroot/usr/arm-linux-gnueabihf/lib',
                   root / 'sysroot/lib/arm-linux-gnueabihf',
                   root / 'sysroot/usr/lib/arm-linux-gnueabihf']
    queue = ['libavahi-client.so.3', 'libavahi-common.so.3', 'libstdc++.so.6', 'libgcc_s.so.1']
    closure = {}
    while queue:
        name = queue.pop()
        if name in closure:
            continue
        path = next((directory / name for directory in directories if (directory / name).exists()), None)
        if path is None:
            raise ValueError('public target sysroot misses shared dependency: ' + name)
        details = subprocess.check_output(['readelf', '-h', '-d', str(path)],
                                          text=True, env=child_environment())
        if 'Machine:                           ARM' not in details:
            raise ValueError('public target sysroot contains a non-ARM shared dependency')
        closure[name] = _digest(path)
        queue.extend(re.findall(r'\(NEEDED\).*?\[([^]]+)\]', details))
    evidence = {'target': 'armv7-unknown-linux-gnueabihf', 'coverage': 'abi-link',
                'probe_sha256': _digest(output), 'target_dependency_sha256': closure,
                'crt': str(Path(crt).resolve().relative_to(root.resolve())),
                'target_executed': False}
    _write_json(root / 'toolchain-self-test.json', evidence)


def prepare(profile_id, output_dir):
    """Build a fresh, relocatable verified bundle and return bounded receipts.

    NDI_PUBLIC_TOOLCHAIN_CACHE may name a local directory of SHA256.deb files;
    every cached artifact is rehashed before extraction. It contains public GNU
    packages only. Existing output bundles are rejected instead of trusted.
    """
    recipe = _recipe(profile_id)
    if platform.system() != 'Linux' or platform.machine() not in ('x86_64', 'amd64'):
        raise ValueError('public GNU compiler requires an x86_64 Linux host')
    libc, version = platform.libc_ver()
    if libc != 'glibc' or tuple(map(int, version.split('.')[:2])) < (2, 36):
        raise ValueError('public GNU compiler requires host glibc 2.36 or later')
    for command in ('dpkg-deb', 'readelf'):
        if shutil.which(command) is None:
            raise ValueError('public GNU compiler prerequisite missing: ' + command)
    root = Path(output_dir).resolve()
    if root.exists() and any(root.iterdir()):
        raise ValueError('public toolchain output must be empty; existing bundles are not trusted')
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    root.chmod(0o700)
    cache = Path(os.environ.get('NDI_PUBLIC_TOOLCHAIN_CACHE', str(root / 'packages'))).resolve()
    cache.mkdir(parents=True, exist_ok=True)
    for package in recipe['packages']:
        archive = _download(package, cache)
        snapshot = root / '.verified-package.deb'
        try:
            _snapshot_package(package, archive, snapshot)
            _extract_deb(package, snapshot, root / ('host' if package['role'] == 'compiler' else 'sysroot'))
            if _digest(snapshot) != package['sha256']:
                raise ValueError('private public-package snapshot changed during extraction')
        finally:
            snapshot.unlink(missing_ok=True)
    target = root / 'sysroot/usr' / TRIPLE
    target.mkdir(parents=True, exist_ok=True)
    for directory in ('include', 'lib'):
        shutil.copytree(root / 'host/usr' / TRIPLE / directory, target / directory, symlinks=True)
    (root / 'sysroot/usr/include').symlink_to(TRIPLE + '/include')
    (root / 'bin').mkdir()
    (root / 'bin/armhf-gcc').write_text(WRAPPER, encoding='utf-8')
    (root / 'bin/armhf-gcc').chmod(0o755)
    config = {'profile': profile_id, 'target': recipe['profiles'][profile_id]['target'],
              'linker': 'bin/armhf-gcc', 'sysroot': 'sysroot', 'c_args': [],
              'clang_args': ['-march=armv7-a', '-mfpu=vfpv3-d16', '-mfloat-abi=hard'],
              'rustflags': [], 'coverage': 'abi-link', 'target_glibc': recipe['target_glibc'],
              'sdk_cpu_requirement': recipe['profiles'][profile_id]['sdk_cpu_requirement']}
    _write_json(root / 'ci-toolchain.json', config)
    _self_test(root)
    result = {'root': str(root), 'recipe_sha256': recipe_identity(profile_id),
              'config_sha256': _digest(root / 'ci-toolchain.json'),
              'package_receipts': expected_package_receipts(profile_id)}
    _write_json(root / 'public-toolchain-receipt.json', result)
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('profile')
    parser.add_argument('output', type=Path)
    arguments = parser.parse_args()
    print(json.dumps(prepare(arguments.profile, arguments.output), indent=2, sort_keys=True))
