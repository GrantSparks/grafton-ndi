import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import ssl
import sys
import tarfile
import tempfile
import unittest
import urllib.error
import urllib.request

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import sdk  # noqa: E402

TARGET = 'x86_64-unknown-linux-gnu'


def tarball(members, compression='gz'):
    """Build a tar archive from (name, content) pairs or prepared TarInfo objects."""
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode='w:' + compression) as archive:
        for member in members:
            if isinstance(member, tarfile.TarInfo):
                archive.addfile(member)
                continue
            name, content = member
            info = tarfile.TarInfo(name)
            info.size = len(content)
            archive.addfile(info, io.BytesIO(content))
    return buffer.getvalue()


def linux_download(members=None, script='Install_NDI_SDK_v6_Linux.sh'):
    """A download shaped like the vendor's: a tarball holding one self-extracting script."""
    if members is None:
        root = 'NDI SDK for Linux'
        members = [
            (f'{root}/include/Processing.NDI.Lib.h', b'#pragma once\n'),
            (f'{root}/lib/x86_64-linux-gnu/libndi.so.6.9.1', b'ELF'),
            (f'{root}/Version.txt', b'NDI 2026-01-01 git-0000000 v6.9.1.0\n'),
        ]
    installer = b'#!/bin/sh\nexit 1\n__NDI_ARCHIVE_BEGIN__\n' + tarball(members)
    return tarball([(script, installer)])


def link(name, target, kind=tarfile.SYMTYPE):
    info = tarfile.TarInfo(name)
    info.type = kind
    info.linkname = target
    return info


class Workspace(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.work = Path(directory.name)

    def archive(self, data):
        path = self.work / 'download.tar.gz'
        path.write_bytes(data)
        return path

    def pins(self, data, **overrides):
        pin = {'version': '6.9.1', 'edition': 'standard', 'platform': 'linux',
               'size': len(data), 'sha256': hashlib.sha256(data).hexdigest(), **overrides}
        return {'test-linux': pin}

    def install(self, data, *, pins=None, target=TARGET, environ=None, archive=True):
        return sdk.install('test-linux', self.work / 'sdk', archive=self.archive(data) if archive else None,
                           target=target, environ={} if environ is None else environ,
                           pins=self.pins(data) if pins is None else pins)


class Pins(unittest.TestCase):
    def test_checked_in_pins_cover_three_packages_per_platform(self):
        pins = sdk.load_pins()
        for platform in ('linux', 'android', 'apple', 'windows'):
            names = {f'{prefix}-{platform}' for prefix in ('6.3.2-standard', 'current-standard', 'current-advanced')}
            self.assertLessEqual(names, set(pins))
            for name in names:
                self.assertEqual(pins[name]['platform'], platform)
                self.assertIn(pins[name]['edition'], name)
        self.assertEqual(len({pin['sha256'] for pin in pins.values()}), len(pins))

    def test_malformed_pin_is_rejected(self):
        good = {'version': '6.4.0.2', 'edition': 'standard', 'platform': 'linux', 'size': 1, 'sha256': 'a' * 64}
        for field, value in (('sha256', 'A' * 64), ('size', 0), ('size', '1'), ('platform', 'bsd'),
                             ('edition', 'pro'), ('version', 'six')):
            with self.subTest(field=field, value=value), tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / 'pins.json'
                path.write_text(json.dumps({'p': {**good, field: value}}))
                with self.assertRaises(sdk.SdkError):
                    sdk.load_pins(path)

    def test_artifact_id_is_content_qualified(self):
        pin = {'version': '6.4.0.2', 'edition': 'advanced', 'platform': 'apple', 'sha256': 'f' * 64}
        self.assertEqual(sdk.artifact_id(pin), 'ndi-6.4.0.2-advanced-apple-' + 'f' * 64)


class Sources(unittest.TestCase):
    PIN = {'version': '6.4.0.2', 'edition': 'standard', 'platform': 'linux', 'size': 1, 'sha256': 'b' * 64}

    def select(self, packages, **environ):
        environ.setdefault('NDI_SDK_SOURCES_JSON', json.dumps({'schema': 1, 'packages': packages}))
        return sdk.select_source('pkg', self.PIN, environ)

    def test_github_asset_is_fetched_from_the_api_with_the_token(self):
        asset = {'github_release_asset': {'repo': 'owner/store', 'asset_id': 42}}
        for key in ('pkg', sdk.artifact_id(self.PIN)):
            with self.subTest(key=key):
                source = self.select({key: asset}, NDI_SDK_ARCHIVE_TOKEN='secret')
                self.assertEqual(source.url, 'https://api.github.com/repos/owner/store/releases/assets/42')
                self.assertEqual(source.headers['Authorization'], 'Bearer secret')
                self.assertEqual(source.headers['Accept'], 'application/octet-stream')
                self.assertEqual(source.secret_env, ('NDI_SDK_ARCHIVE_TOKEN',))

    def test_github_asset_honours_a_named_token_variable(self):
        asset = {'github_release_asset': {'repo': 'o/r', 'asset_id': 1, 'token_env': 'OTHER'}}
        self.assertEqual(self.select({'pkg': asset}, OTHER='t').headers['Authorization'], 'Bearer t')
        with self.assertRaisesRegex(sdk.SdkError, 'OTHER'):
            self.select({'pkg': asset}, NDI_SDK_ARCHIVE_TOKEN='unused')

    def test_url_and_archive_entries(self):
        source = self.select({'pkg': {'url_env': 'SDK_URL'}}, SDK_URL='https://example.invalid/sdk')
        self.assertEqual((source.url, source.headers, source.secret_env),
                         ('https://example.invalid/sdk', {}, ('SDK_URL',)))
        self.assertEqual(self.select({'pkg': {'archive': '/store/sdk.tar.gz'}}), Path('/store/sdk.tar.gz'))

    def test_invalid_configuration_is_rejected(self):
        asset = {'repo': 'o/r', 'asset_id': 1}
        cases = {
            'unset': dict(environ={'NDI_SDK_SOURCES_JSON': ''}, packages={}),
            'not json': dict(environ={'NDI_SDK_SOURCES_JSON': '{'}, packages={}),
            'wrong schema': dict(environ={'NDI_SDK_SOURCES_JSON': '{"schema": 2, "packages": {}}'}, packages={}),
            'missing package': dict(packages={'other': {'archive': '/a'}}),
            'ambiguous aliases': dict(packages={'pkg': {'archive': '/a'}, sdk.artifact_id(self.PIN): {'archive': '/a'}}),
            'two kinds': dict(packages={'pkg': {'archive': '/a', 'url_env': 'U'}}),
            'unknown kind': dict(packages={'pkg': {'ftp': 'x'}}),
            'relative archive': dict(packages={'pkg': {'archive': 'sdk.tar.gz'}}),
            'plain http': dict(packages={'pkg': {'url_env': 'U'}}, environ={'U': 'http://example.invalid/sdk'}),
            'url not set': dict(packages={'pkg': {'url_env': 'U'}}),
            'bad repo': dict(packages={'pkg': {'github_release_asset': {**asset, 'repo': 'o/r/../x'}}}),
            'bad asset id': dict(packages={'pkg': {'github_release_asset': {**asset, 'asset_id': '1'}}}),
            'no token': dict(packages={'pkg': {'github_release_asset': asset}}),
        }
        for name, case in cases.items():
            with self.subTest(name), self.assertRaises(sdk.SdkError):
                self.select(case['packages'], **case.get('environ', {}))


class FakeResponse(io.BytesIO):
    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


class FakeOpener:
    """Replays a list of outcomes: bytes are a response body, exceptions are raised."""

    def __init__(self, *outcomes):
        self.outcomes = list(outcomes)
        self.requests = []

    def open(self, request, timeout):
        self.requests.append(request)
        outcome = self.outcomes.pop(0)
        if isinstance(outcome, BaseException):
            raise outcome
        return FakeResponse(outcome)


def http_error(code):
    return urllib.error.HTTPError('https://private.invalid/asset', code, 'reason', {}, None)


class Downloads(Workspace):
    SOURCE = sdk.Download('https://private.invalid/asset', {'Authorization': 'Bearer t'}, ())

    def fetch(self, *outcomes):
        opener, sleeps = FakeOpener(*outcomes), []
        destination = self.work / 'out'
        try:
            sdk.download(self.SOURCE, destination, opener=opener, sleep=sleeps.append)
        finally:
            self.sleeps, self.requests = sleeps, opener.requests
        return destination.read_bytes()

    def test_transient_failures_are_retried_twice(self):
        self.assertEqual(self.fetch(http_error(503), urllib.error.URLError(TimeoutError()), b'payload'), b'payload')
        self.assertEqual(self.sleeps, list(sdk.RETRY_DELAYS))
        self.assertEqual(self.requests[-1].get_header('Authorization'), 'Bearer t')

    def test_retries_are_bounded(self):
        with self.assertRaisesRegex(sdk.SdkError, 'HTTP 502'):
            self.fetch(http_error(502), http_error(502), http_error(502), b'never read')
        self.assertEqual(len(self.requests), 3)

    def test_authorisation_and_tls_failures_are_not_retried(self):
        for outcome in (http_error(404), http_error(403), http_error(429), urllib.error.URLError(ssl.SSLError())):
            with self.subTest(outcome=outcome):
                with self.assertRaises(sdk.SdkError) as failure:
                    self.fetch(outcome, b'never read')
                self.assertEqual((len(self.requests), self.sleeps), (1, []))
                self.assertNotIn('private.invalid', str(failure.exception))

    def test_redirect_drops_credentials_and_must_stay_https(self):
        handler = sdk.CredentialFreeRedirects()
        request = urllib.request.Request('https://api.github.com/x', headers={
            'Authorization': 'Bearer t', 'Cookie': 'c', 'Accept': 'application/octet-stream'})
        redirected = handler.redirect_request(request, None, 302, 'Found', {}, 'https://objects.invalid/signed')
        self.assertEqual(redirected.full_url, 'https://objects.invalid/signed')
        self.assertEqual(dict(redirected.header_items()), {'Accept': 'application/octet-stream'})
        with self.assertRaises(sdk.SdkError):
            handler.redirect_request(request, None, 302, 'Found', {}, 'http://objects.invalid/signed')


class Install(Workspace):
    def test_linux_package_is_unpacked_without_running_the_installer(self):
        values = self.install(linux_download())
        root = self.work / 'sdk' / 'NDI SDK for Linux'
        runtime = root / 'lib' / 'x86_64-linux-gnu'
        self.assertEqual(values, {'NDI_SDK_DIR': str(root), 'NDI_RUNTIME_DIR_V6': str(runtime)})
        self.assertTrue((root / 'include' / 'Processing.NDI.Lib.h').is_file())
        for alias in ('libndi.so', 'libndi.so.6'):
            self.assertEqual(os.readlink(runtime / alias), 'libndi.so.6.9.1')
        self.assertEqual(sorted(path.name for path in self.work.iterdir()), ['download.tar.gz', 'sdk'])

    def test_runtime_directory_is_optional(self):
        self.assertEqual(list(self.install(linux_download(), target=None)), ['NDI_SDK_DIR'])

    def test_source_map_archive_is_used_when_no_archive_is_given(self):
        data = linux_download()
        sources = {'schema': 1, 'packages': {'test-linux': {'archive': str(self.archive(data))}}}
        values = self.install(data, archive=False, environ={'NDI_SDK_SOURCES_JSON': json.dumps(sources)})
        self.assertTrue(Path(values['NDI_RUNTIME_DIR_V6'], 'libndi.so.6').exists())

    def test_archive_must_match_the_pin_before_it_is_opened(self):
        data = linux_download()
        for field, value in (('size', len(data) + 1), ('sha256', '0' * 64)):
            with self.subTest(field=field), self.assertRaisesRegex(sdk.SdkError, 'the pin says'):
                self.install(data, pins=self.pins(data, **{field: value}))
            self.assertFalse((self.work / 'sdk').exists())

    def test_wrong_content_is_rejected(self):
        root = 'NDI SDK for Linux'
        header = (f'{root}/include/Processing.NDI.Lib.h', b'')
        library = (f'{root}/lib/x86_64-linux-gnu/libndi.so.6.9.1', b'')
        version = (f'{root}/Version.txt', b'v6.9.1.0')
        cases = {
            'other version': [header, library, (f'{root}/Version.txt', b'v6.9.2.0')],
            'no version file': [header, library],
            'no runtime for the target': [header, version],
            'no header': [library, version],
            'two roots': [header, library, version, ('other/include/Processing.NDI.Lib.h', b'')],
        }
        for name, members in cases.items():
            with self.subTest(name), self.assertRaises(sdk.SdkError):
                self.install(linux_download(members))
            self.assertFalse((self.work / 'sdk').exists())

    def test_edition_selects_the_runtime_library(self):
        data = linux_download()
        with self.assertRaisesRegex(sdk.SdkError, 'libndi_advanced.so.6'):
            self.install(data, pins=self.pins(data, edition='advanced'))

    def test_unsafe_archive_members_are_rejected(self):
        device = tarfile.TarInfo('NDI SDK for Linux/device')
        device.type = tarfile.CHRTYPE
        cases = {
            'parent path': [('../escape', b'')],
            'absolute path': [('/tmp/escape', b'')],
            'symlink outside': [link('NDI SDK for Linux/link', '../../outside')],
            'hard link outside': [link('NDI SDK for Linux/link', '../outside', tarfile.LNKTYPE)],
            'path through symlink': [link('inside', '..'), ('inside/escape', b'')],
            'device': [device],
        }
        for name, members in cases.items():
            with self.subTest(name), self.assertRaises((sdk.SdkError, tarfile.TarError)):
                self.install(linux_download(members))
            self.assertEqual(sorted(path.name for path in self.work.iterdir()), ['download.tar.gz'])

    def test_installer_script_must_be_unique_and_carry_the_marker(self):
        payload = tarball([('x', b'')])
        for name, data in {
            'no script': tarball([('README', b'')]),
            'two scripts': tarball([('Install_NDI_A.sh', payload), ('Install_NDI_B.sh', payload)]),
            'no marker': tarball([('Install_NDI_SDK.sh', b'#!/bin/sh\n' + payload)]),
        }.items():
            with self.subTest(name), self.assertRaises(sdk.SdkError):
                self.install(data)

    def test_target_and_destination_are_checked_before_any_work(self):
        data = linux_download()
        for target in ('x86_64-pc-windows-msvc', 'riscv64gc-unknown-linux-gnu'):
            with self.subTest(target=target), self.assertRaises(sdk.SdkError):
                sdk.install('test-linux', self.work / 'sdk', archive=self.work / 'absent', target=target,
                            environ={}, pins=self.pins(data))
        (self.work / 'sdk').mkdir()
        with self.assertRaisesRegex(sdk.SdkError, 'already exists'):
            self.install(data)
        with self.assertRaisesRegex(sdk.SdkError, 'unknown package'):
            sdk.install('absent', self.work / 'other', environ={}, pins=self.pins(data))

    def test_vendor_tools_do_not_receive_acquisition_secrets(self):
        data = linux_download()
        sources = {'schema': 1, 'packages': {'test-linux': {'url_env': 'PRIVATE_URL'}}}
        environ = {'NDI_SDK_SOURCES_JSON': json.dumps(sources), 'NDI_SDK_ARCHIVE_TOKEN': 't',
                   'PRIVATE_URL': 'https://private.invalid/sdk', 'PATH': '/bin'}
        seen = {}
        original = (sdk.download, sdk.extract)

        def restore():
            sdk.download, sdk.extract = original
        self.addCleanup(restore)
        sdk.download = lambda source, destination: destination.write_bytes(data)

        def extract(archive, platform, destination, tool_environ):
            seen.update(tool_environ)
            original[1](archive, platform, destination, tool_environ)
        sdk.extract = extract
        self.install(data, archive=False, environ=environ)
        self.assertEqual(seen, {'PATH': '/bin'})


class CommandLine(Workspace):
    def run_main(self, *arguments):
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            status = sdk.main(list(arguments))
        return status, stdout.getvalue(), stderr.getvalue()

    def test_list_names_every_pin(self):
        status, stdout, _ = self.run_main('list')
        self.assertEqual(status, 0)
        self.assertEqual([line.split('\t')[0] for line in stdout.splitlines()], sorted(sdk.load_pins()))

    def test_install_exports_variables_and_reports_failures_without_a_traceback(self):
        data = linux_download()
        original = sdk.load_pins
        self.addCleanup(setattr, sdk, 'load_pins', original)
        sdk.load_pins = lambda: self.pins(data)
        exported = self.work / 'github-env'
        exported.write_text('EXISTING=1\n')
        arguments = ('install', 'test-linux', '--dest', str(self.work / 'sdk'), '--archive', str(self.archive(data)),
                     '--target', TARGET, '--github-env', str(exported))
        status, stdout, _ = self.run_main(*arguments)
        self.assertEqual(status, 0)
        self.assertEqual(exported.read_text(), 'EXISTING=1\n' + stdout)
        self.assertRegex(stdout, r'\ANDI_SDK_DIR=.+/sdk/NDI SDK for Linux\nNDI_RUNTIME_DIR_V6=.+/x86_64-linux-gnu\n\Z')
        status, stdout, stderr = self.run_main(*arguments)
        self.assertEqual((status, stdout), (1, ''))
        self.assertRegex(stderr, r'\Aerror: .+ already exists\n\Z')


if __name__ == '__main__':
    unittest.main()
