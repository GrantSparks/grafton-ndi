#!/usr/bin/env python3
"""Install one pinned NDI SDK package, or a driver for one, for CI or local development.

    python ci/sdk.py install current-standard-linux --dest /tmp/ndi \\
        --target x86_64-unknown-linux-gnu [--archive FILE] [--github-env FILE]
    python ci/sdk.py install-driver current-hx-driver-linux --sdk /tmp/ndi \\
        --target x86_64-unknown-linux-gnu [--archive FILE]
    python ci/sdk.py list

The archive comes from --archive or from the private source map in
NDI_SDK_SOURCES_JSON. Its size and SHA-256 are checked against
ci/sdk-packages.json before anything opens it. `install` prints the
NDI_SDK_DIR and NDI_RUNTIME_DIR_V6 values to use, and appends them to
--github-env when given. `install-driver` copies a driver's libraries beside
the runtime library of an SDK that `install` unpacked, after checking that
both come from the same vendor build, and prints the files it added.

Nothing this script prints includes a value from the source map or a credential:
the private store's repository, asset identifiers, URLs and local paths must not
reach a public log. Local archives are copied before use, so the vendor tools it
runs are never given such a path either.
"""
from __future__ import annotations

import argparse
import contextlib
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
# A driver package names its driver where an SDK names its edition. Its version
# is that of the SDK release it was built with.
DRIVER_FIELDS = PIN_FIELDS - {'edition'} | {'driver'}
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
# Seconds to wait after each refused attempt to move the finished tree into
# place on Windows, about 30 in all; nothing else retries.
RENAME_DELAYS = (1, 2, 4, 8, 15)
TRANSIENT_STATUS = {408, 500, 502, 503, 504}
INSTALLER_MARKER = re.compile(rb'^__NDI_ARCHIVE_BEGIN__\r?\n', re.MULTILINE)
SDK_INSTALLER = 'Install_NDI*.sh'
VERSION = re.compile(r'(?<![0-9])v(\d+(?:\.\d+){1,3})(?![0-9.])')
# The vendor build a Version.txt names, such as `git-7bdfb5361`.
BUILD = re.compile(r'(?<![\w-])git-([0-9a-f]+)(?![\w-])')


class Driver(NamedTuple):
    """A library the runtime loads from its own directory, or by name through the loader."""
    platform: str
    installer: str  # the self-extracting script in the download
    library: str  # the library the runtime loads
    dependencies: re.Pattern  # the names of the libraries that must sit beside it
    directories: dict  # Rust target -> the directory of the unpacked driver that holds its libraries


DRIVERS = {
    # The NDI|HX Driver for Linux decodes NDI|HX video for the runtime it is copied beside.
    'hx': Driver('linux', 'NDIHXDriverForLinux*.sh', 'libndihx.so', re.compile(r'lib\w+-ndi\.so\.\d+'),
                 {'x86_64-unknown-linux-gnu': 'x86_64-linux-gnu', 'i686-unknown-linux-gnu': 'i686-linux-gnu'}),
}


class SdkError(Exception):
    """A failure whose message is safe to print in a public log."""


class Download(NamedTuple):
    url: str
    headers: dict


def well_formed(pin):
    """Whether `pin` is an SDK package's or a driver package's entry, with sound values."""
    if not isinstance(pin, dict):
        return False
    if set(pin) == PIN_FIELDS:
        known = pin['platform'] in ARCHIVE_NAME and pin['edition'] in ('standard', 'advanced')
    elif set(pin) == DRIVER_FIELDS:
        driver = DRIVERS.get(pin['driver']) if isinstance(pin['driver'], str) else None
        known = driver is not None and pin['platform'] == driver.platform
    else:
        return False
    return (known and bool(re.fullmatch(r'\d+(\.\d+){1,3}', str(pin['version'])))
            and type(pin['size']) is int and pin['size'] > 0
            and bool(re.fullmatch(r'[0-9a-f]{64}', str(pin['sha256']))))


def load_pins(path=PINS):
    pins = json.loads(Path(path).read_text(encoding='utf-8'))
    for name, pin in pins.items():
        if not well_formed(pin):
            raise SdkError(f'{name}: malformed entry in {Path(path).name}')
    return pins


def kind(pin):
    """An SDK package's edition, or `<driver>-driver` for a driver package."""
    return pin['edition'] if 'edition' in pin else f"{pin['driver']}-driver"


def artifact_id(pin):
    """The content-qualified name the private source map may use instead of the package id."""
    return f"ndi-{pin['version']}-{kind(pin)}-{pin['platform']}-{pin['sha256']}"


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


def extract_embedded(archive, destination, installer=SDK_INSTALLER):
    """Unpack a Linux or Android download.

    The download is a tarball holding one self-extracting script named like
    `installer` whose tail, after a marker line, is the package's tarball. The
    script is not executed.
    """
    with tarfile.open(archive, mode='r:*') as outer:
        scripts = [member for member in outer.getmembers()
                   if member.isfile() and fnmatch.fnmatch(Path(member.name).name, installer)]
        if len(scripts) != 1:
            raise SdkError(f'archive must contain exactly one {installer}')
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


def publish(unpacked, dest, *, windows=None, sleep=time.sleep):
    """Move the finished tree to `dest`.

    Right after the silent vendor installer on Windows its executables can stay
    locked for a while (a virus scanner reading what it wrote, a child process
    still exiting), and renaming a directory that holds them then fails with
    PermissionError. That is retried with a growing delay, on Windows only; any
    other platform, and any other error, fails at once.
    """
    windows = sys.platform == 'win32' if windows is None else windows
    for delay in (RENAME_DELAYS if windows else ()):
        try:
            unpacked.rename(dest)
            return
        except PermissionError:
            sleep(delay)
    try:
        unpacked.rename(dest)
    except PermissionError as error:
        if not windows:
            raise
        raise SdkError(f'the unpacked SDK could not be moved into place; access stayed denied for '
                       f'{sum(RENAME_DELAYS)} s (a virus scanner or the installer may still hold its files)') from error


def pinned(package, pins, *, driver):
    """The pin of `package`, which must be a driver package exactly when `driver` is true."""
    pins = load_pins() if pins is None else pins
    if package not in pins:
        raise SdkError(f'unknown package {package}; known: {", ".join(sorted(pins))}')
    pin = pins[package]
    if driver and 'driver' not in pin:
        raise SdkError(f'{package} is an SDK package; install it with the install command')
    if not driver and 'driver' in pin:
        raise SdkError(f'{package} is a driver package; install it with the install-driver command')
    return pin


@contextlib.contextmanager
def staged(parent, prefix):
    """A new private directory under `parent` that is removed afterwards, if it can be.

    Removing it must never decide the outcome: on Windows the installer's own
    files can still be locked then, and the error would replace the real one,
    or fail an installation that has already succeeded. The directory is made
    and removed here and not by tempfile.TemporaryDirectory, whose handling of
    cleanup errors differs between Python versions.
    """
    staging = Path(tempfile.mkdtemp(prefix=prefix, dir=parent))
    try:
        yield staging
    finally:
        try:
            shutil.rmtree(staging)
        except OSError:
            pass  # a leftover directory is harmless; the outcome stands


def fetch(source, pin, staging):
    """Copy or download the package into `staging` and check it against its pin before anything opens it."""
    local = staging / ARCHIVE_NAME[pin['platform']]
    if isinstance(source, Download):
        download(source, local)
    else:
        snapshot(source, local)
    verify(local, pin)
    return local


def install(package, dest, *, archive=None, target=None, environ=None, pins=None):
    """Install `package` into `dest` and return the environment that selects it."""
    environ = os.environ if environ is None else environ
    pin = pinned(package, pins, driver=False)
    library = runtime_library(pin, target) if target else None
    dest = Path(dest).absolute()
    if dest.exists():
        raise SdkError(f'{dest} already exists')
    source = Path(archive) if archive else select_source(package, pin, environ)
    # Vendor tools never see acquisition credentials.
    secret = secret_variables(environ)
    tool_env = {name: value for name, value in environ.items() if name not in secret}
    dest.parent.mkdir(parents=True, exist_ok=True)
    with staged(dest.parent, '.ndi-sdk-') as staging:
        local = fetch(source, pin, staging)
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
            # The test process must verify the library it actually loaded, not
            # infer identity from the archive or the directory it was given.
            values['GRAFTON_NDI_EXPECTED_RUNTIME'] = dest / (root / library).relative_to(unpacked)
            values['GRAFTON_NDI_EXPECTED_VERSION'] = '.'.join(map(str, version_tuple(pin['version'])))
            values['GRAFTON_NDI_EXPECTED_EDITION'] = pin['edition']
        # The destination appears only once every check has passed.
        publish(unpacked, dest)
    return {name: str(path) for name, path in values.items()}


def build_of(version_file, what):
    """The one vendor build that a Version.txt names."""
    text = version_file.read_text(encoding='utf-8', errors='replace') if version_file.is_file() else ''
    found = set(BUILD.findall(text))
    if len(found) != 1:
        raise SdkError(f'{what} Version.txt does not name one vendor build')
    return found.pop()


def driver_libraries(unpacked, driver, target):
    """The directory of the unpacked driver that holds the libraries for `target`, and their names.

    Every file there must be the driver library or one of its dependencies, so
    that nothing the runtime needs is left behind and nothing else is copied.
    """
    roots = sorted({library.parent.parent for library in unpacked.rglob(driver.library)
                    if (library.parent.parent / 'Version.txt').is_file()})
    if len(roots) != 1:
        raise SdkError(f'expected one driver root in the package, found {len(roots)}')
    directory = roots[0] / driver.directories[target]
    names = sorted(path.name for path in directory.iterdir()) if directory.is_dir() else []
    if driver.library not in names:
        raise SdkError(f'the driver has no {driver.directories[target]}/{driver.library}')
    for name in names:
        path = directory / name
        if path.is_symlink() or not path.is_file():
            raise SdkError(f'the driver\'s {name} is not a regular file')
        if name != driver.library and not driver.dependencies.fullmatch(name):
            raise SdkError(f'the driver\'s {driver.directories[target]} holds an unexpected file {name}')
    return roots[0], directory, names


def install_driver(package, sdk, *, target, archive=None, environ=None, pins=None):
    """Copy the libraries of the driver `package` beside the runtime library of the SDK in `sdk`.

    `sdk` is the directory `install` unpacked an SDK into, or the SDK root
    itself. The driver must come from the same vendor build as that SDK, and
    none of its files may exist in the runtime directory yet. Returns the files
    it added.
    """
    environ = os.environ if environ is None else environ
    pin = pinned(package, pins, driver=True)
    driver = DRIVERS[pin['driver']]
    if target not in driver.directories:
        raise SdkError(f'the {package} driver has no libraries for target {target}')
    sdk = Path(sdk).absolute()
    if not sdk.is_dir():
        raise SdkError(f'{sdk} is not a directory')
    root = sdk_root(sdk)
    # The driver carries no version number, only the build it came from. The
    # SDK must be the pinned release and name the same build.
    check_version(root, pin)
    sdk_build = build_of(root / 'Version.txt', 'the SDK\'s')
    runtime = root / RUNTIMES[target][1]
    if not any((runtime / name).is_file() for name in RUNTIMES[target][2:]):
        raise SdkError(f'the SDK has no runtime library in {RUNTIMES[target][1]}')
    source = Path(archive) if archive else select_source(package, pin, environ)
    with staged(None, '.ndi-driver-') as staging:
        local = fetch(source, pin, staging)
        unpacked = staging / 'driver'
        extract_embedded(local, unpacked, driver.installer)
        reject_outside_links(unpacked)
        driver_root, directory, names = driver_libraries(unpacked, driver, target)
        driver_build = build_of(driver_root / 'Version.txt', 'the driver\'s')
        if driver_build != sdk_build:
            raise SdkError(f'the driver is build git-{driver_build} and the SDK build git-{sdk_build}; '
                           f'a driver pin installs only beside the SDK release of its own build')
        present = [name for name in names if os.path.lexists(runtime / name)]
        if present:
            raise SdkError(f'the runtime directory already has {", ".join(present)}')
        installed = []
        try:
            for name in names:
                # `x` refuses a file that appeared since the check above.
                with (directory / name).open('rb') as library, (runtime / name).open('xb') as copy:
                    installed.append(runtime / name)
                    shutil.copyfileobj(library, copy, 1 << 20)
                shutil.copymode(directory / name, runtime / name)
        except BaseException:
            # Half a driver beside a runtime is worse than none.
            for path in installed:
                path.unlink(missing_ok=True)
            raise
    return [str(path) for path in installed]


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
    command.add_argument('--github-output', type=Path, help='append the selected SDK values as step outputs')
    command = commands.add_parser('install-driver', help='verify a driver package and copy it beside a runtime')
    command.add_argument('package')
    command.add_argument('--sdk', required=True, type=Path,
                         help='the directory install unpacked the SDK into, or the SDK root')
    command.add_argument('--target', required=True, help='Rust target whose runtime directory receives the driver')
    command.add_argument('--archive', type=Path, help='local archive instead of NDI_SDK_SOURCES_JSON')
    args = parser.parse_args(argv)
    try:
        if args.command == 'list':
            for name, pin in sorted(load_pins().items()):
                print(f"{name}\t{pin['version']}\t{kind(pin)}\t{pin['platform']}")
            return 0
        if args.command == 'install-driver':
            installed = install_driver(args.package, args.sdk, target=args.target, archive=args.archive)
            sys.stdout.write(''.join(f'installed {path}\n' for path in installed))
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
    for destination in (args.github_env, args.github_output):
        if destination:
            # These records are also read by Bash in compatibility jobs on
            # Windows; native CRLF would become part of the exported paths.
            with destination.open('a', encoding='utf-8', newline='\n') as stream:
                stream.write(lines)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
