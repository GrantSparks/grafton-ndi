#!/usr/bin/env python3
"""Acquire one exact SDK locally; never cache or upload proprietary payloads."""
from __future__ import annotations
import argparse
import hashlib
import http.client
import json
import io
import os
from pathlib import Path
import re
import shutil
import socket
import ssl
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.request
import urllib.parse
from common import ROOT, digest, read_json, write_json


def extract_tar(archive, destination):
    destination.mkdir(parents=True, exist_ok=True)
    options = {'fileobj': archive} if hasattr(archive, 'read') else {'name': archive}
    with tarfile.open(mode='r:*', **options) as stream:
        if hasattr(tarfile, 'data_filter'):
            stream.extractall(destination, filter='data')
        else:
            # Python 3.10 local hosts lack the backported extraction filter.
            root = destination.resolve()
            for member in stream:
                target = (root / member.name).resolve()
                if not target.is_relative_to(root) or Path(member.name).is_absolute():
                    raise ValueError('archive member escapes extraction root')
                if not (member.isfile() or member.isdir() or member.issym() or member.islnk()):
                    raise ValueError('archive contains a special device or unsupported member')
                if member.issym() or member.islnk():
                    base = target.parent if member.issym() else root
                    if Path(member.linkname).is_absolute() or not (base / member.linkname).resolve().is_relative_to(root):
                        raise ValueError('archive link escapes extraction root')
                member.mode &= 0o777
                stream.extract(member, root)



# Soft transfer/retry progress budget. urllib's socket, DNS and redirect handling
# can outlive an individual check; this is not a hard wall-clock cancellation.
GET_BUDGET_SECONDS = 300
GET_RETRY_DELAYS = (1, 3)


class _AcquisitionRejected(ValueError):
    """A closed, locator-free failure that must never trigger a retry."""


class _NetworkFailure(Exception):
    """Keep transport exceptions out of diagnostics and distinct from local I/O."""
    def __init__(self, error):
        super().__init__()
        self.error = error


def _network_call(action):
    try:
        return action()
    except _AcquisitionRejected:
        raise
    except Exception as error:
        raise _NetworkFailure(error) from None


def _remaining(deadline):
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise _AcquisitionRejected('SDK acquisition retry budget exhausted')
    return remaining


def _transient_get_error(error):
    if isinstance(error, urllib.error.HTTPError):
        # 403/404 may conceal private-resource authorization failures. Rate
        # limiting (429) needs its own server-directed delay, not this short retry.
        return error.code in (408, 500, 502, 503, 504)
    if isinstance(error, urllib.error.URLError):
        error = error.reason
    if isinstance(error, ssl.SSLError):
        return False
    if isinstance(error, socket.gaierror):
        return error.errno == socket.EAI_AGAIN
    return isinstance(error, (TimeoutError, ConnectionResetError,
                             ConnectionAbortedError, http.client.IncompleteRead))


def _retry_get(action, failure):
    """Retry one GET transaction, sharing its progress budget across attempts."""
    deadline = time.monotonic() + GET_BUDGET_SECONDS
    for attempt in range(len(GET_RETRY_DELAYS) + 1):
        _remaining(deadline)
        try:
            return action(deadline)
        except _AcquisitionRejected:
            raise
        except _NetworkFailure as transport:
            error = transport.error
            transient = _transient_get_error(error)
            if isinstance(error, urllib.error.HTTPError):
                try:
                    error.close()
                except Exception:
                    raise ValueError(failure) from None
            if not transient or attempt == len(GET_RETRY_DELAYS):
                raise ValueError(failure) from None
            delay = GET_RETRY_DELAYS[attempt]
            if _remaining(deadline) <= delay:
                raise _AcquisitionRejected('SDK acquisition retry budget exhausted') from None
            time.sleep(delay)
        except Exception:
            # Local output/open/write/hash/cleanup errors are never retries,
            # even when a filesystem reports a transient-looking exception.
            raise ValueError(failure) from None


class PrivateRedirectHandler(urllib.request.HTTPRedirectHandler):
    def __init__(self, deadline=None):
        self.deadline = deadline

    def redirect_request(self, request, fp, code, message, headers, new_url):
        if urllib.parse.urlsplit(new_url).scheme.lower() != 'https':
            raise ValueError('SDK redirects must remain HTTPS')
        if self.deadline is not None:
            # urllib carries the original request timeout into its next open.
            request.timeout = min(request.timeout, _remaining(self.deadline))
        redirected = super().redirect_request(request, fp, code, message, headers, new_url)
        if redirected is not None:
            # A GitHub asset redirect carries its own signed URL. Never forward
            # a repository token, even to another GitHub-owned hostname.
            for name in list(redirected.headers) + list(redirected.unredirected_hdrs):
                if name.lower() in ('authorization', 'cookie', 'proxy-authorization'):
                    redirected.remove_header(name)
        return redirected


def _copy_response(response, output, deadline, limit=None):
    # read1 avoids waiting for a whole large buffer when a peer trickles bytes.
    # Socket timeouts remain per blocking operation; deadline checks also stop
    # a continuously progressing transfer from renewing its retry budget.
    length = response.headers.get('Content-Length')
    expected = int(length) if length is not None else None
    total = 0
    while True:
        _remaining(deadline)
        block = _network_call(lambda: response.read1(64 * 1024))
        _remaining(deadline)
        if not block:
            if expected is not None and total < expected:
                raise _NetworkFailure(http.client.IncompleteRead(b''))
            return
        total += len(block)
        if limit is not None and total > limit:
            raise _AcquisitionRejected('private GitHub repository metadata exceeds its size bound')
        output.write(block)


def download(url, expected, destination, *, headers=None, _deadline=None):
    if not re.fullmatch(r'[0-9a-fA-F]{64}', expected or ''):
        raise ValueError('an exact verified archive SHA256 is required')
    if not url.startswith('https://'):
        raise ValueError('SDK source must be an HTTPS URL')
    destination.parent.mkdir(parents=True, exist_ok=True)

    def attempt(deadline):
        request = urllib.request.Request(url, headers={'User-Agent': 'grafton-ndi-ci', **(headers or {})})
        opener = urllib.request.build_opener(PrivateRedirectHandler(deadline))
        try:
            timeout = min(120, _remaining(deadline))
            with _network_call(lambda: opener.open(request, timeout=timeout)) as response, destination.open('wb') as output:
                _copy_response(response, output, deadline)
            actual = digest(destination)
            if actual != expected.lower():
                raise _AcquisitionRejected(f'archive SHA256 mismatch: expected {expected.lower()}, got {actual}')
        except BaseException:
            # A retry restarts the original URL from byte zero, never a partial
            # file or signed redirect. Failure to remove it is also fail-fast.
            destination.unlink(missing_ok=True)
            raise

    # Private asset acquisition supplies one outer transaction budget. Do not
    # nest another retry loop inside it or reveal raw transport diagnostics.
    if _deadline is not None:
        return attempt(_deadline)
    return _retry_get(attempt, 'SDK download failed; source details are withheld')


def sdk_root(path):
    candidates = sorted({p.parent.parent for p in path.rglob('Processing.NDI.Lib.h')
                         if p.parent.name.lower() == 'include'})
    if len(candidates) != 1:
        raise ValueError(f'expected one extracted SDK root, found {len(candidates)}')
    return candidates[0]


def extract_sdk(archive, output, platform, *, env=None):
    unpacked = output / 'unpacked'
    if unpacked.exists():
        raise ValueError('refusing to extract over an existing SDK directory')
    if platform in ('linux', 'android'):
        installers = output / 'installers'
        extract_tar(archive, installers)
        scripts = list(installers.rglob('Install_NDI*.sh'))
        if len(scripts) != 1:
            raise ValueError('SDK archive must contain exactly one embedded installer')
        payload = scripts[0].read_bytes()
        marker = re.search(rb'^__NDI_ARCHIVE_BEGIN__\r?\n', payload, re.MULTILINE)
        if not marker:
            raise ValueError('embedded SDK archive marker absent')
        extract_tar(io.BytesIO(payload[marker.end():]), unpacked)
    elif platform == 'apple':
        # pkgutil expands payloads without installing into /Library or running scripts.
        subprocess.run(['pkgutil', '--expand-full', str(archive), str(unpacked)], check=True, env=env if env is not None else child_environment())
    elif platform == 'windows':
        if sys.platform != 'win32':
            raise ValueError('Windows SDK provisioning requires a Windows host')
        unpacked.mkdir(parents=True)
        subprocess.run([str(archive), '/VERYSILENT', '/SP-', '/SUPPRESSMSGBOXES',
                        '/NORESTART', '/NOCANCEL', f'/DIR={unpacked}'], check=True, timeout=300, env=env if env is not None else child_environment())
    else:
        raise ValueError('unknown package platform')
    root = sdk_root(unpacked)
    if platform == 'linux':
        for library in root.glob('lib/*/lib*.so.*.*.*'):
            name = library.name.split('.so.')[0] + '.so'
            major = library.name.split('.so.')[1].split('.')[0]
            for alias in (name, name + '.' + major):
                target = library.with_name(alias)
                if not target.exists():
                    target.symlink_to(library.name)
    return root


RECEIPT_SCHEMA = 2


def catalog_packages(catalog=None):
    data = read_json(Path(catalog)) if isinstance(catalog, (str, Path)) else catalog
    return data if data is not None else read_json(ROOT / 'ci/sdk-packages.json')


def resolved_package(key, catalog=None, expected_override=None):
    packages = catalog_packages(catalog)
    if key not in packages:
        raise ValueError(f'unknown SDK package ID {key}')
    package = dict(packages[key])
    for field in ('version', 'edition', 'platform', 'archive_format', 'version_evidence'):
        if not package.get(field):
            raise ValueError(f'{key}: package catalog is missing {field}')
    if not isinstance(package['version'], str) or not re.fullmatch(r'\d+(?:\.\d+){1,3}', package['version']):
        raise ValueError(f'{key}: package version must contain two to four numeric components')
    if package['edition'] not in ('standard', 'advanced'):
        raise ValueError(f'{key}: unsupported edition')
    formats = {'linux': 'embedded-tar', 'android': 'embedded-tar', 'apple': 'apple-pkg', 'windows': 'windows-installer'}
    if package.get('archive_format') != formats.get(package['platform']):
        raise ValueError(f'{key}: archive format does not match its platform')
    expected = package.get('sha256') or expected_override
    if not re.fullmatch(r'[0-9a-fA-F]{64}', expected or ''):
        raise ValueError(f'{key}: verified SHA256 is not configured; baseline cannot be skipped')
    expected = expected.lower()
    if expected_override and expected_override.lower() != expected:
        raise ValueError(f'{key}: configured SHA256 contradicts the reviewed catalog')
    artifact = 'ndi-' + package['version'] + '-' + package['edition'] + '-' + package['platform'] + '-' + expected
    if package.get('artifact_id') and package['artifact_id'] != artifact:
        raise ValueError(f'{key}: immutable artifact ID contradicts package content identity')
    package.update(sha256=expected, artifact_id=artifact)
    return package


def source_map(sources=None):
    if sources is None:
        return {}
    data = read_json(Path(sources)) if isinstance(sources, (str, Path)) else sources
    if not isinstance(data, dict) or data.get('schema') != 1 or set(data) != {'schema', 'packages'} or not isinstance(data['packages'], dict):
        raise ValueError('private source map must have schema1 and a packages object')
    for entry in data['packages'].values():
        if not isinstance(entry, dict) or set(entry) not in ({'archive'}, {'url_env'}, {'github_release_asset'}):
            raise ValueError('each private source entry must select exactly archive, url_env or github_release_asset')
        if 'archive' in entry and (not isinstance(entry['archive'], str) or not Path(entry['archive']).is_absolute()):
            raise ValueError('private archive sources must be absolute paths')
        if 'url_env' in entry and (not isinstance(entry['url_env'], str) or not re.fullmatch(r'[A-Z_][A-Z0-9_]*', entry['url_env'])):
            raise ValueError('private URL sources must name an environment variable')
        if 'github_release_asset' in entry:
            github_asset(entry['github_release_asset'])
    return data['packages']


def github_asset(value):
    if not isinstance(value, dict) or not {'repo', 'asset_id'}.issubset(value) or set(value) - {'repo', 'asset_id', 'token_env'}:
        raise ValueError('GitHub asset source requires repo, asset_id and optional token_env')
    repo = value['repo']
    if not isinstance(repo, str) or not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repo) or any(part in ('.', '..') for part in repo.split('/')):
        raise ValueError('GitHub asset source requires a valid owner/repository identifier')
    if type(value['asset_id']) is not int or value['asset_id'] <= 0:
        raise ValueError('GitHub asset source requires a positive numeric asset ID')
    token_env = value.get('token_env', 'NDI_SDK_ARCHIVE_TOKEN')
    if not isinstance(token_env, str) or not re.fullmatch(r'[A-Z_][A-Z0-9_]*', token_env):
        raise ValueError('GitHub asset token must be supplied through a named environment variable')
    return dict(value, token_env=token_env)


def verify_private_github_repository(repo, token, *, _deadline=None):
    # This request carries the token only to the exact GitHub API hostname.
    url = 'https://api.github.com/repos/' + repo
    def attempt(deadline):
        request = urllib.request.Request(url, headers={'User-Agent': 'grafton-ndi-ci',
            'Authorization': 'Bearer ' + token, 'Accept': 'application/vnd.github+json',
            'X-GitHub-Api-Version': '2026-03-10'})
        opener = urllib.request.build_opener(PrivateRedirectHandler(deadline))
        timeout = min(30, _remaining(deadline))
        with _network_call(lambda: opener.open(request, timeout=timeout)) as response:
            if urllib.parse.urlsplit(response.geturl()).hostname != 'api.github.com':
                raise _AcquisitionRejected('repository metadata redirected outside the GitHub API')
            output = io.BytesIO()
            _copy_response(response, output, deadline, limit=2 * 1024 * 1024)
            data = json.loads(output.getvalue())
        if (not isinstance(data, dict) or data.get('private') is not True
                or data.get('visibility') != 'private'
                or data.get('full_name', '').lower() != repo.lower()):
            raise _AcquisitionRejected('GitHub SDK archive source must be the exact configured private repository')

    if _deadline is not None:
        return attempt(_deadline)
    return _retry_get(attempt, 'private GitHub archive repository metadata could not be verified')


def download_github_asset(source, expected, destination):
    source = github_asset(source)
    token = os.environ.get(source['token_env'], '')
    if not token:
        raise ValueError('private GitHub asset read token is not configured (' + source['token_env'] + ')')
    url = 'https://api.github.com/repos/' + source['repo'] + '/releases/assets/' + str(source['asset_id'])
    parsed = urllib.parse.urlsplit(url)
    if parsed.scheme != 'https' or parsed.hostname != 'api.github.com' or parsed.port not in (None, 443):
        raise ValueError('private GitHub asset authentication is restricted to api.github.com')
    def attempt(deadline):
        verify_private_github_repository(source['repo'], token, _deadline=deadline)
        download(url, expected, destination, headers={'Authorization': 'Bearer ' + token,
            'Accept': 'application/octet-stream', 'X-GitHub-Api-Version': '2026-03-10'}, _deadline=deadline)
    return _retry_get(attempt, 'private GitHub SDK acquisition failed; source details are withheld')


def environment_sources(environment=None):
    value = (os.environ if environment is None else environment).get('NDI_SDK_SOURCES_JSON', '')
    if not value:
        return None
    try:
        return json.loads(value)
    except (ValueError, TypeError):
        raise ValueError('NDI_SDK_SOURCES_JSON is not valid JSON') from None


def child_environment(environment=None, *, sources=None):
    """SDK acquisition credentials must not reach compilers or vendor installers."""
    source = os.environ if environment is None else environment
    result = dict(source)
    keys = {'NDI_SDK_ARCHIVE_TOKEN', 'NDI_SDK_SOURCES_JSON', 'NDI_SDK_SOURCE_URL',
            'NDI_HEADER_URL', 'NDI_RUNTIME_URL', 'NDI_TOOLCHAIN_URL', 'NDI_SDK_APP_PRIVATE_KEY'}
    keys.update(key for key in source if key.startswith('NDI_') and key.endswith('_URL'))
    entries = list(source_map(environment_sources(source)).values())
    if sources is not None:
        entries += list(source_map(sources).values())
    for entry in entries:
        if 'url_env' in entry:
            keys.add(entry['url_env'])
        if 'github_release_asset' in entry:
            keys.add(entry['github_release_asset'].get('token_env', 'NDI_SDK_ARCHIVE_TOKEN'))
    for key in keys:
        result.pop(key, None)
    return result


def resolve_source(key, package, url='', archive=None, *, sources=None, offline=False):
    if archive is not None and url:
        raise ValueError(f'{key}: choose one archive or URL source')
    entries = source_map(sources)
    selected = [entries[k] for k in (key, package['artifact_id']) if k in entries]
    if len(selected) > 1:
        raise ValueError(f'{key}: private source map has ambiguous alias and artifact entries')
    if selected and (archive is not None or url):
        raise ValueError(f'{key}: explicit source conflicts with the private source map')
    if selected:
        entry = selected[0]
        if 'archive' in entry:
            archive = Path(entry['archive'])
        elif 'github_release_asset' in entry:
            if offline:
                raise ValueError(f'{key}: offline provisioning requires a local archive')
            return None, {'github_release_asset': github_asset(entry['github_release_asset'])}
        else:
            url = os.environ.get(entry['url_env'], '')
    if archive is not None:
        archive = Path(archive).resolve()
        if not archive.is_file():
            raise ValueError(f'{key}: configured local archive is unavailable')
        return archive, ''
    if offline:
        raise ValueError(f'{key}: offline provisioning requires a local archive')
    if not url:
        raise ValueError(f'{key}: no SDK source is configured')
    if not url.startswith('https://'):
        raise ValueError(f'{key}: SDK source must use HTTPS')
    return None, url


def installed_inventory(root):
    root = root.resolve()
    entries = []
    trees = [p for p in root.iterdir() if p.is_dir() and p.name.lower() in ('include', 'lib', 'bin')]
    for tree in trees:
        for path in sorted(tree.rglob('*')):
            if not path.is_file() and not path.is_symlink():
                continue
            try:
                resolved = path.resolve(strict=True)
            except (OSError, RuntimeError):
                raise ValueError('installed SDK contains a dangling or cyclic artifact link') from None
            if not resolved.is_relative_to(root):
                raise ValueError('installed SDK inventory contains an external symlink')
            item = {'path': path.relative_to(root).as_posix()}
            if path.is_symlink():
                item.update(kind='symlink', target=os.readlink(path))
            elif path.is_file():
                item.update(kind='file', bytes=path.stat().st_size, sha256=digest(path))
            else:
                raise ValueError('installed SDK contains a dangling artifact link')
            entries.append(item)
    version_file = root / 'Version.txt'
    if version_file.is_file():
        entries.append({'path': 'Version.txt', 'kind': 'file', 'bytes': version_file.stat().st_size, 'sha256': digest(version_file)})
    entries.sort(key=lambda x: x['path'])
    if not any(x['path'].lower().endswith('/processing.ndi.lib.h') for x in entries):
        raise ValueError('installed SDK lacks its umbrella header')
    return entries


def inventory_digest(entries):
    return hashlib.sha256(json.dumps(entries, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def inspect_installation(root, package):
    inventory = installed_inventory(root)
    names = [Path(entry['path']).name.lower() for entry in inventory]
    core = 'ndi_advanced' if package['edition'] == 'advanced' else 'ndi'
    if package['platform'] == 'windows':
        token = 'processing.ndi.lib.advanced.' if package['edition'] == 'advanced' else 'processing.ndi.lib.'
        present = any(n.startswith(token) and n.endswith('.dll') and (package['edition'] == 'advanced' or '.advanced.' not in n) for n in names)
    else:
        pattern = r'lib' + core + r'(?:\.so(?:\.[0-9.]+)?|\.dylib|_(?:ios|tvos|visionos)\.a)'
        present = any(re.fullmatch(pattern, n) for n in names)
    if not present:
        raise ValueError('installed SDK has no runtime for its selected edition')
    path = root / 'Version.txt'
    observed = None
    if path.exists():
        versions = re.findall(r'(?<![0-9])v(\d+(?:\.\d+){1,3})(?![0-9.])', path.read_text(encoding='utf-8'))
        if len(versions) != 1:
            raise ValueError('Version.txt does not contain one unambiguous SDK version')
        observed = versions[0]
        normalize = lambda value: tuple(int(v) for v in value.split('.')) + (0,) * (4 - len(value.split('.')))
        if normalize(observed) != normalize(package['version']):
            raise ValueError('installed SDK exact version differs from the selected package')
    elif package['version_evidence'] != 'archive-sha256':
        raise ValueError('selected SDK requires Version.txt version evidence')
    return observed, inventory


def sanitized_receipt(receipt):
    fields = ('schema', 'package', 'artifact_id', 'archive_sha256', 'edition', 'platform',
              'version', 'version_evidence', 'inventory_sha256')
    return {field: receipt[field] for field in fields if field in receipt}


def verify_installation(key, output, *, catalog=None, expected_override=None):
    package = resolved_package(key, catalog, expected_override)
    receipt = read_json(Path(output) / 'provision.json')
    required = {'schema': RECEIPT_SCHEMA, 'package': key, 'artifact_id': package['artifact_id'],
                'archive_sha256': package['sha256'], 'edition': package['edition'], 'platform': package['platform']}
    if any(receipt.get(k) != v for k, v in required.items()):
        raise ValueError(f'{key}: installed receipt differs from requested artifact identity')
    relative = Path(receipt.get('root_relative', ''))
    root = (Path(output) / relative).resolve()
    if relative.is_absolute() or not root.is_relative_to(Path(output).resolve()) or not root.is_dir():
        raise ValueError(f'{key}: installed SDK root is outside its receipt directory')
    version, inventory = inspect_installation(root, package)
    if version != receipt.get('version') or inventory != receipt.get('inventory') or inventory_digest(inventory) != receipt.get('inventory_sha256'):
        raise ValueError(f'{key}: installed SDK content changed; replace it from its verified archive')
    return dict(receipt, root=str(root))


def provision(key, output, url='', expected_override=None, archive=None, *, catalog=None, sources=None, offline=False, reuse=False):
    package = resolved_package(key, catalog, expected_override)
    if os.environ.get('GITHUB_ACTIONS') == 'true' and os.environ.get('NDI_CI_RELEASED') != 'true':
        raise ValueError('hosted SDK acquisition remains disabled until publication is authorized')
    output = Path(output).resolve()
    if output.exists():
        if reuse:
            return verify_installation(key, output, catalog=catalog, expected_override=expected_override)
        raise ValueError(f'{key}: destination already exists; use verified reuse or a new destination')
    archive, url = resolve_source(key, package, url or '', archive, sources=sources, offline=offline)
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.' + output.name + '-staging-', dir=output.parent) as staging:
        stage = Path(staging)
        caller_archive = archive
        archive = stage / ('installer.exe' if package['platform'] == 'windows' else 'installer.pkg' if package['platform'] == 'apple' else 'installer.tar.gz')
        if caller_archive is None:
            if isinstance(url, dict):
                download_github_asset(url['github_release_asset'], package['sha256'], archive)
            else:
                download(url, package['sha256'], archive)
        else:
            # Open the caller-controlled input once, then hash and consume only
            # this private snapshot. A later caller-path replacement is irrelevant.
            with caller_archive.open('rb') as source, archive.open('xb') as target:
                shutil.copyfileobj(source, target)
            if digest(archive) != package['sha256']:
                raise ValueError(f'{key}: local archive SHA256 differs from the selected package')
        archive.chmod(0o500 if package['platform'] == 'windows' else 0o400)
        root = extract_sdk(archive, stage, package['platform'], env=child_environment(sources=sources))
        if digest(archive) != package['sha256']:
            raise ValueError(f'{key}: private archive snapshot changed during extraction')
        # Retention belongs to the SDK store's content-addressed archive vault.
        # Neither this extraction snapshot nor the expanded installer is needed.
        archive.chmod(0o600)
        archive.unlink()
        if (stage / 'installers').is_dir():
            shutil.rmtree(stage / 'installers')
        version, inventory = inspect_installation(root, package)
        relative = root.relative_to(stage)
        receipt = {'schema': RECEIPT_SCHEMA, 'package': key, 'artifact_id': package['artifact_id'],
            'archive_sha256': package['sha256'], 'edition': package['edition'], 'platform': package['platform'],
            'version': version, 'version_evidence': 'version-txt' if version else 'archive-sha256',
            'root_relative': relative.as_posix(), 'inventory': inventory, 'inventory_sha256': inventory_digest(inventory)}
        write_json(stage / 'provision.json', receipt)
        # The final directory appears only after every verification has succeeded.
        stage.rename(output)
    return dict(receipt, root=str(output / relative))


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--package', required=True)
    p.add_argument('--output', required=True, type=Path)
    p.add_argument('--archive', type=Path, help='local installer; exact archive hash is still required')
    p.add_argument('--catalog', type=Path, help='reviewed package catalog; defaults to the checked-in catalog')
    p.add_argument('--sources', type=Path, help='private schema1 source map; never written into a receipt')
    p.add_argument('--offline', action='store_true')
    p.add_argument('--reuse', action='store_true', help='rehash and verify an already populated destination')
    p.add_argument('--receipt-output', type=Path, help='write bounded availability evidence without private paths')
    args = p.parse_args()
    try:
        result = provision(args.package, args.output, os.environ.get('NDI_SDK_SOURCE_URL', ''),
            os.environ.get('NDI_SDK_SOURCE_SHA256'), args.archive, catalog=args.catalog,
            sources=args.sources if args.sources else environment_sources(), offline=args.offline, reuse=args.reuse)
    except (ValueError, OSError, subprocess.SubprocessError, tarfile.TarError) as error:
        detail = str(error) if isinstance(error, ValueError) else type(error).__name__
        if args.receipt_output:
            write_json(args.receipt_output, {'schema': RECEIPT_SCHEMA, 'package': args.package, 'status': 'unavailable', 'reason': detail})
        print(f'Provisioning failed: {detail}', file=sys.stderr)
        return 1
    if args.receipt_output:
        write_json(args.receipt_output, dict(sanitized_receipt(result), status='available'))
    print(f"Verified {result['package']} ({result['artifact_id']})")
    return 0

if __name__ == '__main__':
    raise SystemExit(main())
