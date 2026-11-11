"""Exercise provisioning identity, offline isolation and private-source boundaries."""
from copy import deepcopy
from pathlib import Path
import hashlib
import http.client
import io
import json
import os
import shutil
import socket
import ssl
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import urllib.request
import urllib.error

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import provision as provision_module
from aggregate import validate_availability
from plan import provisioning_status
from provision import (PrivateRedirectHandler, download_github_asset, github_asset,
    provision, resolved_package, sanitized_receipt, source_map, verify_installation, verify_private_github_repository, child_environment, inspect_installation)

KEY = 'current-standard-linux'


def fixture_archive(directory, *, version='6.4.0.2', core='libndi.so', header=b'fixture header\n'):
    archive = directory / 'private-source-secret.tar.gz'
    payload = io.BytesIO()
    with tarfile.open(fileobj=payload, mode='w:gz') as stream:
        files = {'SDK/include/Processing.NDI.Lib.h': header,
                 'SDK/Version.txt': ('NDI fixture v' + version + '\n').encode(),
                 'SDK/lib/x86_64-linux-gnu/' + core: b'fixture runtime bytes\n'}
        for name, value in files.items():
            member = tarfile.TarInfo(name); member.size = len(value)
            stream.addfile(member, io.BytesIO(value))
    installer = b'#!/bin/sh\nexit 17\n__NDI_ARCHIVE_BEGIN__\n' + payload.getvalue()
    with tarfile.open(archive, 'w:gz') as stream:
        member = tarfile.TarInfo('Install_NDI_fixture.sh'); member.size = len(installer)
        stream.addfile(member, io.BytesIO(installer))
    return archive


def fixture_catalog(archive):
    sha = hashlib.sha256(archive.read_bytes()).hexdigest()
    return {KEY: dict(version='6.4.0.2', edition='standard', platform='linux', sha256=sha,
                     archive_format='embedded-tar', version_evidence='version-txt', artifact_id=None)}


class OfflineProvisioningFixtures(unittest.TestCase):
    def setUp(self):
        # These archives are synthetic test bytes, not SDK access. Do not inherit
        # the runner's hosted-release gate or any acquisition credentials/locators.
        # The hosted gate itself has separate tests below with explicit inputs.
        environment = patch.dict(os.environ, {'GITHUB_ACTIONS': 'false'}, clear=True)
        environment.start()
        self.addCleanup(environment.stop)
        for name in ('download', 'download_github_asset'):
            network = patch('provision.' + name,
                            side_effect=AssertionError('offline fixtures must not access the network'))
            network.start()
            self.addCleanup(network.stop)


class ProvisioningTests(OfflineProvisioningFixtures):
    def test_transactional_offline_install_and_relocation(self):
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name)
            archive = fixture_archive(directory); catalog = fixture_catalog(archive)
            target = directory / 'installed'
            sources = {'schema': 1, 'packages': {KEY: {'archive': str(archive)}}}
            with patch('provision.download') as network:
                result = provision(KEY, target, catalog=catalog, sources=sources, offline=True)
                network.assert_not_called()
            self.assertEqual(result['version'], '6.4.0.2')
            self.assertEqual(result['schema'], 2)
            self.assertFalse(any(directory.glob('.installed-staging-*')))
            receipt = json.loads((target / 'provision.json').read_text())
            self.assertNotIn('root', receipt)
            self.assertNotIn('private-source-secret', json.dumps(receipt))
            moved = directory / 'relocated'; target.rename(moved)
            reused = provision(KEY, moved, catalog=catalog, offline=True, reuse=True)
            self.assertEqual(reused['root'], str(moved / 'unpacked/SDK'))
            self.assertEqual(result['inventory_sha256'], reused['inventory_sha256'])
            self.assertNotIn(str(directory), json.dumps(sanitized_receipt(reused)))

    def test_caller_path_replacement_cannot_change_verified_snapshot(self):
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name); archive = fixture_archive(directory); catalog = fixture_catalog(archive)
            altered_dir = directory / 'altered'; altered_dir.mkdir()
            altered = fixture_archive(altered_dir, header=b'unreviewed different header\n')
            real_extract = provision_module.extract_sdk
            observed = []
            def replace_caller(path, output, platform, **kwargs):
                observed.append(path)
                self.assertNotEqual(path, archive)
                shutil.copyfile(altered, archive)
                return real_extract(path, output, platform, **kwargs)
            with patch('provision.extract_sdk', side_effect=replace_caller):
                result = provision(KEY, directory / 'installed', archive=archive, catalog=catalog, offline=True)
            self.assertEqual((Path(result['root']) / 'include/Processing.NDI.Lib.h').read_bytes(), b'fixture header\n')
            self.assertNotEqual(hashlib.sha256(archive.read_bytes()).hexdigest(), catalog[KEY]['sha256'])
            self.assertEqual(len(observed), 1)
            verify_installation(KEY, directory / 'installed', catalog=catalog)

    def test_private_snapshot_mutation_before_extract_cannot_publish_receipt(self):
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name); archive = fixture_archive(directory); catalog = fixture_catalog(archive)
            altered_dir = directory / 'altered'; altered_dir.mkdir()
            altered = fixture_archive(altered_dir, header=b'unreviewed different header\n')
            real_extract = provision_module.extract_sdk
            def replace_snapshot(path, output, platform, **kwargs):
                path.chmod(0o600)
                shutil.copyfile(altered, path)
                return real_extract(path, output, platform, **kwargs)
            target = directory / 'installed'
            with patch('provision.extract_sdk', side_effect=replace_snapshot), self.assertRaisesRegex(ValueError, 'snapshot changed'):
                provision(KEY, target, archive=archive, catalog=catalog, offline=True)
            self.assertFalse(target.exists())
            self.assertFalse(any(directory.glob('.installed-staging-*')))

    def test_explicit_private_map_credentials_are_removed_from_extractor_environment(self):
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name); archive = fixture_archive(directory); catalog = fixture_catalog(archive)
            private_map = directory / 'private-sources.json'
            private_map.write_text(json.dumps({'schema': 1, 'packages': {
                KEY: {'github_release_asset': {'repo': 'owner/private', 'asset_id': 1, 'token_env': 'CUSTOM_READER'}},
                'other-package': {'url_env': 'CUSTOM_URL'}}}))
            real_extract = provision_module.extract_sdk
            def inspect_environment(path, output, platform, **kwargs):
                self.assertNotIn('CUSTOM_READER', kwargs['env'])
                self.assertNotIn('CUSTOM_URL', kwargs['env'])
                return real_extract(path, output, platform, **kwargs)
            with patch.dict(os.environ, {'CUSTOM_READER': 'secret', 'CUSTOM_URL': 'https://secret.invalid/'}), \
                 patch('provision.download_github_asset', side_effect=lambda source, expected, destination: shutil.copyfile(archive, destination)), \
                 patch('provision.extract_sdk', side_effect=inspect_environment):
                provision(KEY, directory / 'installed', catalog=catalog, sources=private_map)

    def test_reuse_detects_content_and_inventory_mutations(self):
        for change in ('header', 'runtime', 'version_metadata', 'extra_file', 'remove_file'):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as name:
                directory = Path(name); archive = fixture_archive(directory); catalog = fixture_catalog(archive)
                target = directory / 'installed'
                result = provision(KEY, target, archive=archive, catalog=catalog, offline=True)
                root = Path(result['root'])
                if change == 'header': (root / 'include/Processing.NDI.Lib.h').write_text('changed')
                if change == 'runtime': (root / 'lib/x86_64-linux-gnu/libndi.so').write_text('changed')
                if change == 'version_metadata': (root / 'Version.txt').write_text('changed metadata v6.4.0.2\n')
                if change == 'extra_file': (root / 'include/extra.h').write_text('unexpected')
                if change == 'remove_file': (root / 'lib/x86_64-linux-gnu/libndi.so').unlink()
                with self.assertRaises(ValueError):
                    verify_installation(KEY, target, catalog=catalog)

    def test_wrong_version_or_edition_never_publishes_partial_install(self):
        for options in ({'version': '6.4.0.20'}, {'core': 'libndi_advanced.so'}):
            with self.subTest(options=options), tempfile.TemporaryDirectory() as name:
                directory = Path(name); archive = fixture_archive(directory, **options)
                target = directory / 'installed'
                with self.assertRaises(ValueError):
                    provision(KEY, target, archive=archive, catalog=fixture_catalog(archive), offline=True)
                self.assertFalse(target.exists())
                self.assertFalse(any(directory.glob('.installed-staging-*')))

    def test_fresh_installation_with_only_dangling_runtime_is_unavailable(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name); (root / 'include').mkdir(); (root / 'lib').mkdir()
            (root / 'include/Processing.NDI.Lib.h').write_text('fixture header')
            (root / 'Version.txt').write_text('NDI fixture v6.4.0.2')
            (root / 'lib/libndi.so').symlink_to('missing-runtime.so')
            package = {'edition': 'standard', 'platform': 'linux', 'version': '6.4.0.2', 'version_evidence': 'version-txt'}
            with self.assertRaisesRegex(ValueError, 'dangling'):
                inspect_installation(root, package)

    def test_wrong_archive_and_catalog_relabel_are_rejected(self):
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name); archive = fixture_archive(directory); catalog = fixture_catalog(archive)
            with self.assertRaisesRegex(ValueError, 'contradicts'):
                resolved_package(KEY, catalog, '0' * 64)
            catalog[KEY]['artifact_id'] = 'wrong-identity'
            with self.assertRaisesRegex(ValueError, 'artifact ID'):
                resolved_package(KEY, catalog)
            catalog[KEY]['artifact_id'] = None
            archive.write_bytes(b'wrong archive')
            with self.assertRaisesRegex(ValueError, 'SHA256 differs'):
                provision(KEY, directory / 'installed', archive=archive, catalog=catalog, offline=True)
            self.assertFalse((directory / 'installed').exists())

    def test_offline_github_source_never_attempts_network(self):
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name); archive = fixture_archive(directory); catalog = fixture_catalog(archive)
            sources = {'schema': 1, 'packages': {KEY: {'github_release_asset': {'repo': 'owner/private', 'asset_id': 42}}}}
            with patch('provision.download_github_asset') as network:
                with self.assertRaisesRegex(ValueError, 'offline'):
                    provision(KEY, directory / 'installed', catalog=catalog, sources=sources, offline=True)
                network.assert_not_called()

    def test_source_descriptors_reject_ambiguity_and_raw_tokens(self):
        bad = [ {'archive': '/tmp/a', 'url_env': 'URL'}, {'url': 'https://secret.invalid/'},
                {'github_release_asset': {'repo': 'owner/private', 'asset_id': 4, 'token': 'secret'}},
                {'archive': 'relative/path'} ]
        for entry in bad:
            with self.subTest(entry=list(entry)), self.assertRaises(ValueError):
                source_map({'schema': 1, 'packages': {KEY: entry}})


class GitHubAssetBoundaryTests(unittest.TestCase):
    def test_authentication_only_goes_to_constructed_github_endpoint(self):
        with patch.dict(os.environ, {'NDI_SDK_ARCHIVE_TOKEN': 'private-token'}), patch('provision.download') as download, patch('provision.verify_private_github_repository') as private:
            private.return_value = None
            download_github_asset({'repo': 'owner/private', 'asset_id': 42}, 'a' * 64, Path('/unused'))
            args, kwargs = download.call_args
            self.assertEqual(private.call_args.args, ('owner/private', 'private-token'))
            self.assertEqual(private.call_args.kwargs['_deadline'], kwargs['_deadline'])
            self.assertEqual(args[0], 'https://api.github.com/repos/owner/private/releases/assets/42')
            self.assertEqual(kwargs['headers']['Authorization'], 'Bearer private-token')
            self.assertEqual(kwargs['headers']['Accept'], 'application/octet-stream')
        for value in ({'repo': 'https://evil.invalid/a', 'asset_id': 42},
                      {'repo': 'owner/../private', 'asset_id': 42},
                      {'repo': 'owner/private', 'asset_id': True}):
            with self.assertRaises(ValueError): github_asset(value)

    def test_public_or_mismatched_repository_is_rejected_before_asset_fetch(self):
        for metadata in ({'private': False, 'visibility': 'public', 'full_name': 'owner/private'},
                         {'private': True, 'visibility': 'private', 'full_name': 'other/private'}):
            with self.subTest(metadata=metadata), patch('provision.urllib.request.build_opener') as build:
                response = build.return_value.open.return_value.__enter__.return_value
                response.geturl.return_value = 'https://api.github.com/repos/owner/private'
                response.headers.get.return_value = None
                response.read1.side_effect = [json.dumps(metadata).encode(), b'']
                with self.assertRaisesRegex(ValueError, 'exact configured private'):
                    verify_private_github_repository('owner/private', 'private-token')

    def test_compilers_do_not_inherit_sdk_acquisition_credentials(self):
        env = {'PATH': '/bin', 'NDI_SDK_DIR': '/private/sdk', 'NDI_HEADER_URL': 'https://secret.invalid/',
               'PRIVATE_READER': 'secret-token', 'NDI_SDK_ARCHIVE_TOKEN': 'other-secret',
               'NDI_SDK_SOURCES_JSON': json.dumps({'schema': 1, 'packages': {KEY: {
                   'github_release_asset': {'repo': 'owner/private', 'asset_id': 3, 'token_env': 'PRIVATE_READER'}}}})}
        clean = child_environment(env)
        self.assertEqual(clean, {'PATH': '/bin', 'NDI_SDK_DIR': '/private/sdk'})
        self.assertIn('PRIVATE_READER', env)

    def test_redirects_strip_secrets_and_reject_http(self):
        request = urllib.request.Request('https://api.github.com/repos/owner/private/releases/assets/42',
            headers={'Authorization': 'Bearer private-token', 'Cookie': 'private-cookie', 'Accept': 'application/octet-stream'})
        handler = PrivateRedirectHandler()
        redirected = handler.redirect_request(request, None, 302, 'Found', {}, 'https://release-assets.githubusercontent.com/signed')
        self.assertNotIn('Authorization', redirected.headers)
        self.assertNotIn('Cookie', redirected.headers)
        self.assertEqual(redirected.headers['Accept'], 'application/octet-stream')
        with self.assertRaisesRegex(ValueError, 'HTTPS'):
            handler.redirect_request(request, None, 302, 'Found', {}, 'http://example.invalid/asset')


class HostedReleaseGateTests(unittest.TestCase):
    def test_unreleased_hosted_acquisition_and_reuse_fail_before_source_access(self):
        for release in (None, '', 'false', 'TRUE', '1'):
            for mode in ('offline', 'remote', 'reuse'):
                with self.subTest(release=release, mode=mode), tempfile.TemporaryDirectory() as name:
                    directory = Path(name)
                    archive = fixture_archive(directory); catalog = fixture_catalog(archive)
                    target = directory / 'installed'
                    environment = {'GITHUB_ACTIONS': 'true'}
                    if release is not None:
                        environment['NDI_CI_RELEASED'] = release
                    options = {'archive': archive, 'offline': True}
                    if mode == 'remote':
                        options = {'url': 'https://example.invalid/never-requested'}
                    elif mode == 'reuse':
                        target.mkdir()
                        options = {'reuse': True}
                    with patch.dict(os.environ, environment, clear=True), \
                         patch('provision.resolve_source') as source, \
                         patch('provision.verify_installation') as reuse, \
                         patch('provision.download') as download, \
                         patch('provision.download_github_asset') as github:
                        with self.assertRaisesRegex(ValueError, 'hosted SDK acquisition remains disabled'):
                            provision(KEY, target, catalog=catalog, **options)
                        source.assert_not_called()
                        reuse.assert_not_called()
                        download.assert_not_called()
                        github.assert_not_called()
                    self.assertFalse(any(directory.glob('.installed-staging-*')))
                    self.assertEqual(target.exists(), mode == 'reuse')

    def test_explicit_hosted_release_allows_a_verified_offline_fixture(self):
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name)
            archive = fixture_archive(directory); catalog = fixture_catalog(archive)
            with patch.dict(os.environ, {'GITHUB_ACTIONS': 'true', 'NDI_CI_RELEASED': 'true'}, clear=True), \
                 patch('provision.download') as download, \
                 patch('provision.download_github_asset') as github:
                receipt = provision(KEY, directory / 'installed', archive=archive, catalog=catalog, offline=True)
                self.assertEqual(receipt['archive_sha256'], catalog[KEY]['sha256'])
                self.assertEqual(receipt['version'], '6.4.0.2')
                download.assert_not_called()
                github.assert_not_called()


class PreflightTests(OfflineProvisioningFixtures):
    def test_missing_sources_and_digests_are_collected_without_secret_values(self):
        with tempfile.TemporaryDirectory() as name:
            archive = fixture_archive(Path(name)); catalog = fixture_catalog(archive)
            catalog['6.0-standard-linux'] = {**catalog[KEY], 'version': '6.0', 'sha256': None}
            cells = [{'header_package': KEY, 'runtime_package': '6.0-standard-linux',
                      'requires_toolchain': False, 'build_std': False}]
            _, _, issues = provisioning_status(cells, catalog, {}, require_sources=True)
            self.assertEqual(len(issues), 3)
            self.assertTrue(any(KEY in issue for issue in issues))
            self.assertTrue(any('6.0-standard-linux' in issue and 'SHA256' in issue for issue in issues))
            _, _, issues = provisioning_status(cells, catalog,
                {'NDI_SDK_CURRENT_STANDARD_LINUX_URL': 'http://private-secret.invalid/token'}, require_sources=True)
            self.assertNotIn('private-secret', json.dumps(issues))

    def test_availability_gate_requires_all_exact_sanitized_receipts(self):
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name); archive = fixture_archive(directory); catalog = fixture_catalog(archive)
            receipt = provision(KEY, directory / 'installed', archive=archive, catalog=catalog, offline=True)
            package = resolved_package(KEY, catalog)
            binding = dict(scope='required', commit='a' * 40, tree='b' * 40, dirty=False,
                           suite_fingerprint='c' * 64, lockfile_sha256='d' * 64)
            plan = {**binding, 'cells': [{'header_package': KEY, 'runtime_package': KEY}],
                    'packages': {KEY: package}, 'availability': [{'package': KEY}],
                    'groups': [{'id': 'fixture', 'availability_packages': [KEY]}]}
            report = dict(sanitized_receipt(receipt), status='available', **binding,
                          group='fixture', plan_sha256='e' * 64)
            # Exercise receipt-level validation with a real tiny installation;
            # full canonical plans/all twelve packages are covered separately.
            self.assertEqual(validate_availability(plan, [report], fingerprint='e' * 64), 1)
            for records in ([], [report, report], [{**report, 'status': 'unavailable'}],
                            [{**report, 'archive_sha256': '0' * 64}], [{**report, 'root': '/private/path'}]):
                with self.assertRaises(ValueError): validate_availability(plan, records, fingerprint='e' * 64)


class TransportResponse:
    """A synthetic HTTP body; fixtures never open a socket."""
    def __init__(self, data=b'', *, chunks=None, length=None, on_read=None):
        self.chunks = list(chunks) if chunks is not None else [data, b'']
        self.headers = {'Content-Length': str(len(data) if length is None else length)}
        self.on_read = on_read
        self.closed = False

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.closed = True

    def geturl(self):
        return 'https://api.github.com/repos/owner/private'

    def read1(self, amount):
        if self.on_read:
            self.on_read()
        result = self.chunks.pop(0)
        if isinstance(result, Exception):
            raise result
        return result


class TransportRetryTests(unittest.TestCase):
    def setUp(self):
        self.clock = 100.0
        self.delays = []
        clock = patch('provision.time.monotonic', side_effect=lambda: self.clock)
        sleep = patch('provision.time.sleep', side_effect=self.sleep)
        network = patch('provision.urllib.request.build_opener')
        for replacement in (clock, sleep, network):
            replacement.start()
            self.addCleanup(replacement.stop)
        self.build = provision_module.urllib.request.build_opener
        self.opener = self.build.return_value
        self.opener.open.side_effect = AssertionError('fixture must configure every GET')
        environment = patch.dict(os.environ, {'NDI_SDK_ARCHIVE_TOKEN': 'fixture-private-token'}, clear=True)
        environment.start()
        self.addCleanup(environment.stop)
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.output = Path(self.directory.name) / 'archive'
        self.payload = b'complete synthetic archive bytes'
        self.expected = hashlib.sha256(self.payload).hexdigest()

    def sleep(self, delay):
        self.delays.append(delay)
        self.clock += delay

    def download(self):
        return provision_module.download('https://private.invalid/original-secret', self.expected, self.output)

    def github(self):
        return download_github_asset({'repo': 'owner/private', 'asset_id': 42}, self.expected, self.output)

    def metadata(self, private=True):
        return TransportResponse(json.dumps({'private': private, 'visibility': 'private' if private else 'public',
                                              'full_name': 'owner/private'}).encode())

    def http_error(self, status):
        return urllib.error.HTTPError('https://private.invalid/SECRET', status, 'SECRET', {}, io.BytesIO(b'SECRET'))

    def test_transient_status_restarts_original_url_and_succeeds(self):
        error = self.http_error(503)
        self.opener.open.side_effect = [error, TransportResponse(self.payload)]
        self.download()
        self.assertEqual(self.delays, [1])
        self.assertEqual(self.output.read_bytes(), self.payload)
        self.assertTrue(error.fp.closed)
        self.assertEqual([call.args[0].full_url for call in self.opener.open.call_args_list],
                         ['https://private.invalid/original-secret'] * 2)

    def test_transient_error_taxonomy_is_explicit(self):
        transient = [TimeoutError(), ConnectionResetError(), ConnectionAbortedError(),
                     http.client.RemoteDisconnected(), http.client.IncompleteRead(b'partial'),
                     socket.gaierror(socket.EAI_AGAIN, 'temporary')]
        for error in transient:
            for wrapped in (error, urllib.error.URLError(error)):
                with self.subTest(error=type(error).__name__, wrapped=type(wrapped).__name__):
                    self.assertTrue(provision_module._transient_get_error(wrapped))
        for status in (408, 500, 502, 503, 504):
            self.assertTrue(provision_module._transient_get_error(self.http_error(status)))

    def test_retry_exhaustion_is_three_attempts_and_redacts_transport_details(self):
        self.opener.open.side_effect = [urllib.error.URLError(TimeoutError('SECRET')) for _ in range(3)]
        with self.assertRaises(ValueError) as error:
            self.download()
        self.assertEqual(str(error.exception), 'SDK download failed; source details are withheld')
        self.assertEqual(self.opener.open.call_count, 3)
        self.assertEqual(self.delays, [1, 3])
        self.assertFalse(self.output.exists())

    def test_partial_body_is_discarded_before_retry(self):
        partial = TransportResponse(chunks=[b'partial', ConnectionResetError('SECRET')], length=100)
        self.opener.open.side_effect = [partial, TransportResponse(self.payload)]
        self.download()
        self.assertTrue(partial.closed)
        self.assertEqual(self.output.read_bytes(), self.payload)
        self.assertEqual(self.delays, [1])

    def test_explicit_transport_truncation_retries_but_hash_mismatch_does_not(self):
        self.opener.open.side_effect = [TransportResponse(b'short', length=100), TransportResponse(self.payload)]
        self.download()
        self.assertEqual(self.delays, [1])
        self.opener.open.reset_mock()
        self.delays.clear()
        self.opener.open.side_effect = [TransportResponse(b'wrong complete response')]
        with self.assertRaisesRegex(ValueError, 'SHA256 mismatch'):
            self.download()
        self.assertEqual(self.opener.open.call_count, 1)
        self.assertEqual(self.delays, [])
        self.assertFalse(self.output.exists())

    def test_auth_tls_filesystem_and_unknown_errors_are_fail_fast(self):
        errors = [self.http_error(code) for code in (400, 401, 403, 404, 429, 501)]
        errors += [ssl.SSLCertVerificationError('SECRET'), ssl.SSLError('SECRET'),
                   urllib.error.URLError(ssl.SSLCertVerificationError('SECRET')),
                   socket.gaierror(socket.EAI_NONAME, 'SECRET'),
                   urllib.error.URLError('SECRET timeout string is not a typed error'),
                   PermissionError('SECRET'), OSError('SECRET'), ValueError('SECRET')]
        for failure in errors:
            with self.subTest(error=type(failure).__name__):
                self.opener.open.reset_mock()
                self.opener.open.side_effect = [failure]
                with self.assertRaises(ValueError) as error:
                    self.download()
                self.assertNotIn('SECRET', str(error.exception))
                self.assertEqual(self.opener.open.call_count, 1)
                self.assertEqual(self.delays, [])

    def test_real_destination_write_failure_is_not_retried(self):
        self.opener.open.side_effect = [TransportResponse(self.payload)]
        with patch.object(Path, 'open', side_effect=PermissionError('SECRET path')):
            with self.assertRaises(ValueError) as error:
                self.download()
        self.assertNotIn('SECRET', str(error.exception))
        self.assertEqual(self.opener.open.call_count, 1)
        self.assertEqual(self.delays, [])

    def test_transient_shaped_local_io_failures_are_not_retried(self):
        for stage in ('open', 'write', 'digest', 'unlink'):
            for error_type in (TimeoutError, ConnectionResetError, ConnectionAbortedError):
                with self.subTest(stage=stage, error=error_type.__name__):
                    self.opener.open.reset_mock()
                    self.opener.open.side_effect = [TransportResponse(self.payload)]
                    error = error_type('SECRET local filesystem location')
                    if stage == 'open':
                        failure = patch.object(Path, 'open', side_effect=error)
                    elif stage == 'write':
                        class LocalOutput(io.BytesIO):
                            def write(self, data):
                                raise error
                        failure = patch.object(Path, 'open', return_value=LocalOutput())
                    elif stage == 'digest':
                        failure = patch('provision.digest', side_effect=error)
                    else:
                        self.opener.open.side_effect = [TimeoutError('SECRET network')]
                        failure = patch.object(Path, 'unlink', side_effect=error)
                    with failure, self.assertRaises(ValueError) as observed:
                        self.download()
                    self.assertNotIn('SECRET', str(observed.exception))
                    self.assertEqual(self.opener.open.call_count, 1)
                    self.assertEqual(self.delays, [])

    def test_private_retry_rechecks_privacy_without_nested_attempts(self):
        self.opener.open.side_effect = [self.metadata(), self.http_error(503),
                                       self.metadata(), TransportResponse(self.payload)]
        self.github()
        self.assertEqual(self.delays, [1])
        self.assertEqual([call.args[0].full_url for call in self.opener.open.call_args_list], [
            'https://api.github.com/repos/owner/private',
            'https://api.github.com/repos/owner/private/releases/assets/42'] * 2)
        for call in self.opener.open.call_args_list:
            self.assertEqual(call.args[0].headers['Authorization'], 'Bearer fixture-private-token')
        self.opener.open.reset_mock()
        self.delays.clear()
        self.opener.open.side_effect = [TimeoutError('SECRET')] * 3
        with self.assertRaises(ValueError):
            self.github()
        self.assertEqual(self.opener.open.call_count, 3)
        self.assertEqual(self.delays, [1, 3])

    def test_changed_privacy_stops_before_next_asset_get(self):
        self.opener.open.side_effect = [self.metadata(), self.http_error(503), self.metadata(private=False)]
        with self.assertRaisesRegex(ValueError, 'exact configured private'):
            self.github()
        self.assertEqual(self.opener.open.call_count, 3)
        self.assertEqual(self.delays, [1])
        self.assertFalse(self.output.exists())

    def test_malformed_private_metadata_is_not_retried(self):
        self.opener.open.side_effect = [TransportResponse(b'not JSON SECRET')]
        with self.assertRaises(ValueError) as error:
            self.github()
        self.assertNotIn('SECRET', str(error.exception))
        self.assertEqual(self.opener.open.call_count, 1)
        self.assertEqual(self.delays, [])

    def test_budget_does_not_restart_after_error_or_stream_progress(self):
        def too_late(*args, **kwargs):
            self.clock += provision_module.GET_BUDGET_SECONDS - 0.5
            raise TimeoutError('SECRET')
        self.opener.open.side_effect = too_late
        with self.assertRaisesRegex(ValueError, 'retry budget'):
            self.download()
        self.assertEqual(self.opener.open.call_count, 1)
        self.assertEqual(self.delays, [])
        self.opener.open.reset_mock()
        self.opener.open.side_effect = [TransportResponse(self.payload,
            on_read=lambda: setattr(self, 'clock', self.clock + provision_module.GET_BUDGET_SECONDS))]
        with self.assertRaisesRegex(ValueError, 'retry budget'):
            self.download()
        self.assertEqual(self.opener.open.call_count, 1)
        self.assertFalse(self.output.exists())

    def test_request_and_redirect_timeouts_use_remaining_budget(self):
        self.opener.open.side_effect = [TransportResponse(self.payload)]
        with patch('provision.GET_BUDGET_SECONDS', 10):
            self.download()
        self.assertEqual(self.opener.open.call_args.kwargs['timeout'], 10)
        request = urllib.request.Request('https://api.github.com/repos/owner/private/releases/assets/42',
            headers={'Authorization': 'Bearer SECRET'})
        request.timeout = 120
        handler = PrivateRedirectHandler(self.clock + 5)
        redirected = handler.redirect_request(request, None, 302, 'Found', {}, 'https://cdn.invalid/signed')
        self.assertEqual(request.timeout, 5)
        self.assertNotIn('Authorization', redirected.headers)


if __name__ == '__main__':
    unittest.main()
