#!/usr/bin/env python3
"""Install one pinned NDI SDK package for CI or local development.

    python ci/sdk.py install current-standard-linux --dest /tmp/ndi \\
        --target x86_64-unknown-linux-gnu [--archive FILE] [--github-env FILE]
    python ci/sdk.py list

The archive comes from --archive or from the private source map in
NDI_SDK_SOURCES_JSON. Its size and SHA-256 are checked against
ci/sdk-packages.json before anything opens it. The command prints the
NDI_SDK_DIR and NDI_RUNTIME_DIR_V6 values to use, and appends them to
--github-env when given.

Nothing this script prints includes a value from the source map or a credential:
the private store's repository, asset identifiers, URLs and local paths must not
reach a public log. Local archives are copied before use, so the vendor tools it
runs are never given such a path either.
"""
from __future__ import annotations

import argparse
import fnmatch
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import shutil
import ssl
import subprocess
import sys
import tarfile
import tempfile
import time
import traceback
from typing import NamedTuple
import urllib.error
import urllib.parse
import urllib.request

PINS = Path(__file__).with_name('sdk-packages.json')
PIN_FIELDS = {'version', 'edition', 'platform', 'size', 'sha256'}
ARCHIVE_NAME = {'linux': 'sdk.tar.gz', 'android': 'sdk.tar.gz', 'apple': 'sdk.pkg', 'windows': 'sdk.exe'}
# Rust target -> (package platform, runtime directory, core library for standard / advanced).
RUNTIMES = {
    'x86_64-unknown-linux-gnu': ('linux', 'lib/x86_64-linux-gnu', 'libndi.so.6', 'libndi_advanced.so.6'),
    'i686-unknown-linux-gnu': ('linux', 'lib/i686-linux-gnu', 'libndi.so.6', 'libndi_advanced.so.6'),
    'aarch64-unknown-linux-gnu': ('linux', 'lib/aarch64-rpi4-linux-gnueabi', 'libndi.so.6', 'libndi_advanced.so.6'),
    'x86_64-pc-windows-msvc': ('windows', 'Bin/x64', 'Processing.NDI.Lib.x64.dll', 'Processing.NDI.Lib.Advanced.x64.dll'),
    'i686-pc-windows-msvc': ('windows', 'Bin/x86', 'Processing.NDI.Lib.x86.dll', 'Processing.NDI.Lib.Advanced.x86.dll'),
    'aarch64-apple-darwin': ('apple', 'lib/macOS', 'libndi.dylib', 'libndi_advanced.dylib'),
    'x86_64-apple-darwin': ('apple', 'lib/macOS', 'libndi.dylib', 'libndi_advanced.dylib'),
}
DEFAULT_TOKEN_ENV = 'NDI_SDK_ARCHIVE_TOKEN'
RETRY_DELAYS = (2, 5)
TRANSIENT_STATUS = {408, 500, 502, 503, 504}
INSTALLER_MARKER = re.compile(rb'^__NDI_ARCHIVE_BEGIN__\r?\n', re.MULTILINE)
VERSION = re.compile(r'(?<![0-9])v(\d+(?:\.\d+){1,3})(?![0-9.])')


class SdkError(Exception):
    """A failure whose message is safe to print in a public log."""


class Download(NamedTuple):
    url: str
    headers: dict


def load_pins(path=PINS):
    pins = json.loads(Path(path).read_text(encoding='utf-8'))
    for name, pin in pins.items():
        if (not isinstance(pin, dict) or set(pin) != PIN_FIELDS
                or pin['platform'] not in ARCHIVE_NAME
                or pin['edition'] not in ('standard', 'advanced')
                or not re.fullmatch(r'\d+(\.\d+){1,3}', str(pin['version']))
                or type(pin['size']) is not int or pin['size'] <= 0
                or not re.fullmatch(r'[0-9a-f]{64}', str(pin['sha256']))):
            raise SdkError(f'{name}: malformed entry in {Path(path).name}')
    return pins


def artifact_id(pin):
    """The content-qualified name the private source map may use instead of the package id."""
    return f"ndi-{pin['version']}-{pin['edition']}-{pin['platform']}-{pin['sha256']}"


def source_map(environ):
    """The `packages` object of the private source map in NDI_SDK_SOURCES_JSON."""
    raw = environ.get('NDI_SDK_SOURCES_JSON')
    if not raw:
        raise SdkError('NDI_SDK_SOURCES_JSON is not set; pass --archive to use a local archive')
    try:
        data = json.loads(raw)
    except ValueError:
        raise SdkError('NDI_SDK_SOURCES_JSON is not valid JSON') from None
    if not isinstance(data, dict) or data.get('schema') != 1 or not isinstance(data.get('packages'), dict):
        raise SdkError('NDI_SDK_SOURCES_JSON must be {"schema": 1, "packages": {...}}')
    return data['packages']


def secret_variables(environ):
    """Names of the variables that carry acquisition secrets: ours and every one the source map names."""
    names = {name for name in environ if name.startswith('NDI_SDK_')}
    try:
        entries = list(source_map(environ).values())
    except SdkError:
        return names
    for entry in entries:
        asset = entry.get('github_release_asset') if isinstance(entry, dict) else None
        for name in (entry.get('url_env') if isinstance(entry, dict) else None,
                     asset.get('token_env') if isinstance(asset, dict) else None):
            if isinstance(name, str):
                names.add(name)
    return names


def select_source(package, pin, environ):
    """Resolve the package's entry in the source map to a local path or a download."""
    packages = source_map(environ)
    entries = [packages[key] for key in (package, artifact_id(pin)) if key in packages]
    if len(entries) != 1 or not isinstance(entries[0], dict) or len(entries[0]) != 1:
        raise SdkError(f'{package}: the source map must hold exactly one single-kind entry for this package')
    kind, value = next(iter(entries[0].items()))
    if kind == 'archive' and isinstance(value, str) and Path(value).is_absolute():
        return Path(value)
    if kind == 'url_env' and isinstance(value, str):
        return https_download(environ.get(value, ''), {})
    if kind == 'github_release_asset' and isinstance(value, dict):
        repo, asset = value.get('repo'), value.get('asset_id')
        token_env = value.get('token_env', DEFAULT_TOKEN_ENV)
        if (not isinstance(repo, str) or not re.fullmatch(r'[\w.-]+/[\w.-]+', repo, re.ASCII)
                or type(asset) is not int or asset <= 0 or not isinstance(token_env, str)):
            raise SdkError(f'{package}: malformed github_release_asset source')
        token = environ.get(token_env)
        if not token:
            raise SdkError(f'{package}: the archive read token is not set '
                           f'({DEFAULT_TOKEN_ENV}, or the variable the source map names)')
        # The token goes to api.github.com only; the redirect to the signed asset URL drops it.
        return https_download(f'https://api.github.com/repos/{repo}/releases/assets/{asset}', {
            'Authorization': 'Bearer ' + token, 'Accept': 'application/octet-stream',
            'X-GitHub-Api-Version': '2026-03-10'})
    raise SdkError(f'{package}: source entries are "archive", "url_env" or "github_release_asset"')


def https_download(url, headers):
    if not url.startswith('https://'):
        raise SdkError('the SDK source is not configured as an HTTPS URL')
    return Download(url, headers)


class CredentialFreeRedirects(urllib.request.HTTPRedirectHandler):
    """Follow HTTPS redirects only, without forwarding credentials to the new host."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if urllib.parse.urlsplit(newurl).scheme.lower() != 'https':
            raise SdkError('the SDK download was redirected away from HTTPS')
        redirected = super().redirect_request(req, fp, code, msg, headers, newurl)
        if redirected is not None:
            for name in list(redirected.headers) + list(redirected.unredirected_hdrs):
                if name.lower() in ('authorization', 'cookie', 'proxy-authorization'):
                    redirected.remove_header(name)
        return redirected


def download(source, destination, *, opener=None, sleep=time.sleep):
    """Fetch to `destination`, retrying a transient failure at most twice."""
    opener = opener or urllib.request.build_opener(CredentialFreeRedirects())
    for attempt in range(len(RETRY_DELAYS) + 1):
        request = urllib.request.Request(source.url, headers={'User-Agent': 'grafton-ndi-ci', **source.headers})
        try:
            with opener.open(request, timeout=120) as response, destination.open('wb') as output:
                shutil.copyfileobj(response, output, 1 << 20)
            return
        except urllib.error.HTTPError as error:
            # 403/404 from a private store are authorisation answers, and 429 asks for a
            # longer wait than this short retry: none of them is worth repeating.
            reason, transient = f'HTTP {error.code}', error.code in TRANSIENT_STATUS
            error.close()
        except (http.client.HTTPException, OSError) as error:
            cause = getattr(error, 'reason', None)
            cause = cause if isinstance(cause, BaseException) else error
            reason, transient = type(cause).__name__, not isinstance(cause, ssl.SSLError)
        if not transient or attempt == len(RETRY_DELAYS):
            raise SdkError(f'SDK download failed ({reason}); the source location is withheld')
        sleep(RETRY_DELAYS[attempt])


def snapshot(source, destination):
    """Copy a local archive into the staging directory.

    Everything afterwards reads the copy: the bytes that are verified are the bytes
    that are unpacked, and no later message or tool argument can name the source.
    """
    try:
        shutil.copyfile(source, destination)
    except OSError as error:
        raise SdkError(f'the local archive could not be read ({error.strerror or type(error).__name__})') from None


def verify(archive, pin):
    """Refuse an archive whose size or SHA-256 differs from the pin."""
    size = archive.stat().st_size
    if size != pin['size']:
        raise SdkError(f'archive is {size} bytes, the pin says {pin["size"]}')
    digest = hashlib.sha256()
    with archive.open('rb') as stream:
        for block in iter(lambda: stream.read(1 << 20), b''):
            digest.update(block)
    if digest.hexdigest() != pin['sha256']:
        raise SdkError(f'archive SHA-256 is {digest.hexdigest()}, the pin says {pin["sha256"]}')


def checked_members(stream, root):
    """Yield tar members that stay inside `root`; reject the archive otherwise."""
    for member in stream:
        target = (root / member.name).resolve()
        if not target.is_relative_to(root):
            raise SdkError('archive member escapes the extraction directory')
        if member.issym() or member.islnk():
            base = target.parent if member.issym() else root
            if not (base / member.linkname).resolve().is_relative_to(root):
                raise SdkError('archive link escapes the extraction directory')
        elif not (member.isfile() or member.isdir()):
            raise SdkError('archive contains a device or other special member')
        yield member


def reject_outside_links(root):
    """Refuse an unpacked tree in which a symbolic link leads out of it.

    A link passes `checked_members` while its target does not exist yet, and can
    point somewhere else once a later member lands, so the finished tree is checked too.
    """
    root = root.resolve()
    for directory, subdirectories, files in os.walk(root):  # links are listed, never followed
        for name in subdirectories + files:
            path = Path(directory, name)
            if not path.is_symlink():
                continue
            try:
                inside = path.resolve().is_relative_to(root)
            except (OSError, RuntimeError):  # a loop
                inside = False
            if not inside:
                raise SdkError('unpacked package contains a link that leads out of it')


def extract_tar(fileobj, destination):
    destination.mkdir(parents=True)
    root = destination.resolve()
    # Python 3.12+ applies its own member filter as well; older versions rely on checked_members.
    options = {'filter': 'data'} if hasattr(tarfile, 'data_filter') else {}
    with tarfile.open(fileobj=fileobj, mode='r|*') as stream:
        stream.extractall(root, members=checked_members(stream, root), **options)


def extract_embedded(archive, destination):
    """Unpack a Linux or Android download.

    The download is a tarball holding one self-extracting `Install_NDI*.sh` whose tail,
    after a marker line, is the SDK tarball. The script is not executed.
    """
    with tarfile.open(archive, mode='r:*') as outer:
        scripts = [member for member in outer.getmembers()
                   if member.isfile() and fnmatch.fnmatch(Path(member.name).name, 'Install_NDI*.sh')]
        if len(scripts) != 1:
            raise SdkError('archive must contain exactly one Install_NDI*.sh')
        with outer.extractfile(scripts[0]) as script, tempfile.TemporaryFile() as payload:
            shutil.copyfileobj(script, payload, 1 << 20)
            payload.seek(0)
            marker = INSTALLER_MARKER.search(payload.read(1 << 20))
            if not marker:
                raise SdkError('installer script has no embedded archive marker')
            payload.seek(marker.end())
            extract_tar(payload, destination)


def run_tool(name, command, environ, timeout=None):
    """Run a vendor tool. A failure reports the tool and its status, never its arguments."""
    try:
        subprocess.run(command, check=True, env=environ, timeout=timeout)
    except subprocess.CalledProcessError as error:
        raise SdkError(f'{name} failed with exit status {error.returncode}') from None
    except subprocess.TimeoutExpired:
        raise SdkError(f'{name} did not finish within {timeout} seconds') from None
    except OSError as error:
        raise SdkError(f'{name} could not be started ({error.strerror or type(error).__name__})') from None


def extract(archive, platform, destination, environ):
    if platform in ('linux', 'android'):
        extract_embedded(archive, destination)
    elif platform == 'apple':
        if sys.platform != 'darwin':
            raise SdkError('Apple packages can only be unpacked on macOS (pkgutil)')
        # --expand-full unpacks the payload without installing it or running package scripts.
        run_tool('pkgutil', ['pkgutil', '--expand-full', str(archive), str(destination)], environ)
    else:
        if sys.platform != 'win32':
            raise SdkError('Windows packages can only be unpacked on Windows (vendor installer)')
        destination.mkdir(parents=True)
        run_tool('the vendor installer', [str(archive), '/VERYSILENT', '/SP-', '/SUPPRESSMSGBOXES', '/NORESTART',
                                          '/NOCANCEL', f'/DIR={destination}'], environ, timeout=300)


def sdk_root(directory):
    roots = sorted({header.parent.parent for header in directory.rglob('Processing.NDI.Lib.h')
                    if header.parent.name.lower() == 'include'})
    if len(roots) != 1:
        raise SdkError(f'expected one SDK root in the package, found {len(roots)}')
    return roots[0]


def add_soname_links(root):
    """Create the `libX.so` and `libX.so.N` names the vendor installer would have created."""
    for library in root.glob('lib/*/lib*.so.*.*.*'):
        stem, version = library.name.split('.so.')
        for alias in (f'{stem}.so', f'{stem}.so.{version.split(".")[0]}'):
            link = library.with_name(alias)
            if not link.exists():
                link.symlink_to(library.name)


def version_tuple(text):
    parts = [int(part) for part in text.split('.')]
    return tuple(parts + [0] * (4 - len(parts)))


def check_version(root, pin):
    """Windows packages carry no Version.txt; everywhere else it must name the pinned version."""
    path = root / 'Version.txt'
    if not path.is_file():
        if pin['platform'] != 'windows':
            raise SdkError('installed SDK has no Version.txt')
        return
    found = VERSION.findall(path.read_text(encoding='utf-8', errors='replace'))
    if len(found) != 1 or version_tuple(found[0]) != version_tuple(pin['version']):
        raise SdkError(f'Version.txt does not name the pinned version {pin["version"]}')


def runtime_library(pin, target):
    """The core runtime library of `pin` for a Rust target, relative to the SDK root."""
    if target not in RUNTIMES:
        raise SdkError(f'no NDI runtime directory is known for target {target}')
    platform, directory, standard, advanced = RUNTIMES[target]
    if platform != pin['platform']:
        raise SdkError(f'target {target} cannot use a {pin["platform"]} package')
    return Path(directory) / (advanced if pin['edition'] == 'advanced' else standard)


def install(package, dest, *, archive=None, target=None, environ=None, pins=None):
    """Install `package` into `dest` and return the environment that selects it."""
    environ = os.environ if environ is None else environ
    pins = load_pins() if pins is None else pins
    if package not in pins:
        raise SdkError(f'unknown package {package}; known: {", ".join(sorted(pins))}')
    pin = pins[package]
    library = runtime_library(pin, target) if target else None
    dest = Path(dest).absolute()
    if dest.exists():
        raise SdkError(f'{dest} already exists')
    source = Path(archive) if archive else select_source(package, pin, environ)
    # Vendor tools never see acquisition credentials.
    secret = secret_variables(environ)
    tool_env = {name: value for name, value in environ.items() if name not in secret}
    dest.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.ndi-sdk-', dir=dest.parent) as staging:
        staging = Path(staging)
        local = staging / ARCHIVE_NAME[pin['platform']]
        if isinstance(source, Download):
            download(source, local)
        else:
            snapshot(source, local)
        verify(local, pin)
        unpacked = staging / 'sdk'
        extract(local, pin['platform'], unpacked, tool_env)
        reject_outside_links(unpacked)
        root = sdk_root(unpacked)
        if pin['platform'] == 'linux':
            add_soname_links(root)
        check_version(root, pin)
        values = {'NDI_SDK_DIR': dest / root.relative_to(unpacked)}
        if library:
            if not (root / library).is_file():
                raise SdkError(f'package has no {library.as_posix()}')
            values['NDI_RUNTIME_DIR_V6'] = dest / (root / library.parent).relative_to(unpacked)
        # The destination appears only once every check has passed.
        unpacked.rename(dest)
    return {name: str(path) for name, path in values.items()}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest='command', required=True)
    commands.add_parser('list', help='print the pinned package ids')
    command = commands.add_parser('install', help='verify and unpack one package')
    command.add_argument('package')
    command.add_argument('--dest', required=True, type=Path, help='directory to create')
    command.add_argument('--archive', type=Path, help='local archive instead of NDI_SDK_SOURCES_JSON')
    command.add_argument('--target', help='Rust target whose runtime directory to export')
    command.add_argument('--github-env', type=Path, help='append the exported variables to this file')
    args = parser.parse_args(argv)
    try:
        if args.command == 'list':
            for name, pin in sorted(load_pins().items()):
                print(f"{name}\t{pin['version']}\t{pin['edition']}\t{pin['platform']}")
            return 0
        values = install(args.package, args.dest, archive=args.archive, target=args.target)
    except SdkError as error:
        print(f'error: {error}', file=sys.stderr)
        return 1
    except Exception as error:  # noqa: BLE001 - nothing unexpected may print a value it carries
        # Only messages written for SdkError are known to be free of source locations
        # and credentials, so anything else reports its type and where it was raised.
        print(f'error: unexpected {type(error).__name__}', file=sys.stderr)
        for frame in traceback.extract_tb(error.__traceback__):
            print(f'  at {Path(frame.filename).name}:{frame.lineno} in {frame.name}', file=sys.stderr)
        return 1
    lines = ''.join(f'{name}={value}\n' for name, value in values.items())
    sys.stdout.write(lines)
    if args.github_env:
        with args.github_env.open('a', encoding='utf-8') as stream:
            stream.write(lines)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
