import contextlib
import hashlib
import io
import json
import http.client
import os
from pathlib import Path
import shutil
import ssl
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock
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
    def test_checked_in_pins_cover_standard_and_advanced_baselines_per_platform(self):
        pins = sdk.load_pins()
        for platform in ('linux', 'android', 'apple', 'windows'):
            names = {f'{prefix}-{platform}' for prefix in ('6.3.2-standard', '6.3.2-advanced', 'current-standard', 'current-advanced')}
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

    def test_github_asset_honours_a_named_token_variable(self):
        asset = {'github_release_asset': {'repo': 'o/r', 'asset_id': 1, 'token_env': 'OTHER'}}
        self.assertEqual(self.select({'pkg': asset}, OTHER='t').headers['Authorization'], 'Bearer t')
        with self.assertRaisesRegex(sdk.SdkError, 'archive read token is not set') as failure:
            self.select({'pkg': asset}, NDI_SDK_ARCHIVE_TOKEN='unused')
        self.assertNotIn('OTHER', str(failure.exception))

    def test_url_and_archive_entries(self):
        source = self.select({'pkg': {'url_env': 'SDK_URL'}}, SDK_URL='https://example.invalid/sdk')
        self.assertEqual(source, sdk.Download('https://example.invalid/sdk', {}))
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
    SOURCE = sdk.Download('https://private.invalid/asset', {'Authorization': 'Bearer t'})

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
        self.assertEqual(values, {
            'NDI_SDK_DIR': str(root), 'NDI_RUNTIME_DIR_V6': str(runtime),
            'GRAFTON_NDI_EXPECTED_RUNTIME': str(runtime / 'libndi.so.6'),
            'GRAFTON_NDI_EXPECTED_VERSION': '6.9.1.0',
            'GRAFTON_NDI_EXPECTED_EDITION': 'standard',
        })
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

    def test_link_that_escapes_once_a_later_member_lands_is_never_installed(self):
        # `p` dangles when it is checked, because `q` does not exist yet: its target
        # collapses to the root. Once `q` lands, `p` leads to a directory outside.
        outside = self.work / 'outside'
        outside.mkdir()
        depth = len(self.work.parts) + 2
        root = 'NDI SDK for Linux'
        members = [(f'{root}/include/Processing.NDI.Lib.h', b''),
                   (f'{root}/lib/x86_64-linux-gnu/libndi.so.6.9.1', b''),
                   (f'{root}/Version.txt', b'v6.9.1.0'),
                   link('p', '/'.join(['q'] * depth + ['..'] * depth) + str(outside)),
                   link('q', '.')]
        try:
            self.install(linux_download(members))
        except (sdk.SdkError, tarfile.TarError):
            pass  # Python 3.11 reaches the check of the finished tree
        else:
            sdk.reject_outside_links(self.work / 'sdk')  # newer tarfile filters rewrite the link to stay inside
        self.assertFalse(any(path.is_symlink() and not path.resolve().is_relative_to(self.work / 'sdk')
                             for path in self.work.rglob('*')))
        self.assertEqual(list(outside.iterdir()), [])

    def test_finished_tree_may_not_contain_a_link_that_leads_out_of_it(self):
        tree = self.work / 'tree'
        (tree / 'lib').mkdir(parents=True)
        (tree / 'lib' / 'libndi.so.6.9.1').write_bytes(b'')
        (tree / 'lib' / 'libndi.so').symlink_to('libndi.so.6.9.1')
        (tree / 'lib' / 'up').symlink_to('..')
        (tree / 'dangling').symlink_to('lib/absent')
        sdk.reject_outside_links(tree)
        outside = self.work / 'outside'
        outside.mkdir()
        # The last target stays inside when collapsed as text, and leaves through the `up` link.
        for name, target in (('absolute', outside), ('relative', Path('..') / '..' / 'outside'),
                             ('through-a-link', Path('up') / '..' / 'outside')):
            with self.subTest(name):
                escaping = tree / 'lib' / name
                escaping.symlink_to(target)
                with self.assertRaisesRegex(sdk.SdkError, 'leads out of it'):
                    sdk.reject_outside_links(tree)
                escaping.unlink()
        sdk.reject_outside_links(tree)

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
        # Every variable the map names is withheld, not only the ones this package uses.
        sources = {'schema': 1, 'packages': {
            'test-linux': {'url_env': 'PRIVATE_URL'},
            'another': {'github_release_asset': {'repo': 'o/r', 'asset_id': 1, 'token_env': 'OTHER_TOKEN'}}}}
        environ = {'NDI_SDK_SOURCES_JSON': json.dumps(sources), 'NDI_SDK_ARCHIVE_TOKEN': 't', 'OTHER_TOKEN': 't',
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


@contextlib.contextmanager
def captured_output():
    """Collect everything written to standard output and error, by this process and its children."""
    captured = []
    python_out, python_err = io.StringIO(), io.StringIO()
    with tempfile.TemporaryFile() as sink:
        sys.stdout.flush()
        sys.stderr.flush()
        saved = os.dup(1), os.dup(2)
        os.dup2(sink.fileno(), 1)
        os.dup2(sink.fileno(), 2)
        try:
            with contextlib.redirect_stdout(python_out), contextlib.redirect_stderr(python_err):
                yield captured
        finally:
            for descriptor, original in zip((1, 2), saved):
                os.dup2(original, descriptor)
                os.close(original)
            sink.seek(0)
            captured.append(python_out.getvalue() + python_err.getvalue() + sink.read().decode('utf-8', 'replace'))


# Every private value used below contains this word, or is this number.
SENTINEL, ASSET_ID = 'sentinel', 987654321
STORE = 'private-store-sentinel'
URL = 'https://host-sentinel.invalid/path-sentinel?signature=sig-sentinel'
TOKEN = 'token-sentinel'
GITHUB_ASSET = {'github_release_asset': {'repo': 'owner-sentinel/repo-sentinel', 'asset_id': ASSET_ID,
                                         'token_env': 'TOKEN_ENV_SENTINEL'}}
URL_ENTRY = {'url_env': 'URL_ENV_SENTINEL'}


def refusal(code):
    return urllib.error.HTTPError(URL, code, f'refused for {URL} with {TOKEN}', {}, None)


class Publish(Workspace):
    def source(self):
        source = mock.Mock(spec=Path)
        return source

    def attempt(self, outcomes, **options):
        source, sleeps = self.source(), []
        source.rename.side_effect = outcomes
        try:
            sdk.publish(source, self.work / 'dest', sleep=sleeps.append, **options)
        finally:
            self.renames, self.sleeps = source.rename.call_count, sleeps

    def test_windows_retries_a_transient_lock_with_growing_delays(self):
        self.attempt([PermissionError(13, 'locked'), PermissionError(13, 'locked'), None], windows=True)
        self.assertEqual((self.renames, self.sleeps), (3, list(sdk.RENAME_DELAYS[:2])))

    def test_windows_gives_up_after_about_thirty_seconds_and_says_so(self):
        with self.assertRaises(sdk.SdkError) as failure:
            self.attempt(PermissionError(13, 'locked'), windows=True)
        self.assertEqual(self.sleeps, list(sdk.RENAME_DELAYS))
        self.assertEqual(self.renames, len(sdk.RENAME_DELAYS) + 1)
        self.assertEqual(sum(sdk.RENAME_DELAYS), 30)
        self.assertIn('could not be moved into place', str(failure.exception))
        self.assertNotIn(str(self.work), str(failure.exception))

    def test_other_errors_are_not_retried(self):
        with self.assertRaises(FileNotFoundError):
            self.attempt(FileNotFoundError(2, 'gone'), windows=True)
        self.assertEqual((self.renames, self.sleeps), (1, []))

    def test_other_platforms_do_not_retry(self):
        with self.assertRaises(PermissionError):
            self.attempt(PermissionError(13, 'denied'), windows=False)
        self.assertEqual((self.renames, self.sleeps), (1, []))

    def test_the_platform_is_detected_by_default(self):
        for platform, retried in (('win32', True), ('linux', False), ('darwin', False)):
            with self.subTest(platform=platform), mock.patch.object(sdk.sys, 'platform', platform):
                with self.assertRaises((sdk.SdkError, PermissionError)):
                    self.attempt(PermissionError(13, 'denied'))
                self.assertEqual(self.renames > 1, retried)

    def test_install_goes_through_publish_and_cleanup_errors_are_ignored(self):
        data = linux_download()
        real = tempfile.TemporaryDirectory
        options = {}

        def recording(*args, **kwargs):
            options.update(kwargs)
            return real(*args, **kwargs)
        with mock.patch.object(sdk.tempfile, 'TemporaryDirectory', recording):
            self.install(data)
        self.assertIs(options.get('ignore_cleanup_errors'), True)

    def test_a_failed_cleanup_neither_fails_an_install_nor_hides_its_error(self):
        data = linux_download()
        real_rmtree = shutil.rmtree

        def locked(path, *args, **kwargs):
            if Path(path).name.startswith('.ndi-sdk-'):
                raise PermissionError(13, 'locked')
            return real_rmtree(path, *args, **kwargs)
        with mock.patch('shutil.rmtree', locked):
            with self.assertRaisesRegex(sdk.SdkError, 'Version.txt'):
                self.install(data, pins=self.pins(data, version='6.9.2'))
            self.assertIn('NDI_SDK_DIR', self.install(data))


class NothingPrivateIsPrinted(Workspace):
    """No failure may print a value from the source map, from a variable it names, or the token."""

    def setUp(self):
        super().setUp()
        self.data = linux_download()
        self.junk = b'not an archive'
        self.archive = self.work / STORE / 'package'
        self.archive.parent.mkdir()
        self.tools = self.work / 'tools'
        self.tools.mkdir()

    def junk_pin(self, platform):
        return {'platform': platform, 'size': len(self.junk), 'sha256': hashlib.sha256(self.junk).hexdigest()}

    def tool(self, name, script):
        path = self.tools / name
        path.write_text(script)
        path.chmod(path.stat().st_mode | stat.S_IXUSR)

    def failed_install(self, entry=None, *, sources=None, environ=None, pin=None, platform=None, outcomes=(),
                       patches=()):
        """Install through the command line with `entry` as the package's source; return what was printed."""
        if sources is None:
            # Other packages' entries name variables too, and those must stay private as well.
            sources = json.dumps({'schema': 1, 'packages': {
                'test-linux': entry, 'another': GITHUB_ASSET, 'a-third': URL_ENTRY}})
        environ = {'NDI_SDK_SOURCES_JSON': sources, 'NDI_SDK_ARCHIVE_TOKEN': TOKEN, 'TOKEN_ENV_SENTINEL': TOKEN,
                   'URL_ENV_SENTINEL': URL, **(environ or {})}
        pins = self.pins(self.data, **(pin or {}))
        opener = FakeOpener(*outcomes)
        with contextlib.ExitStack() as stack:
            stack.enter_context(mock.patch.dict(os.environ, {name: value for name, value in environ.items() if value}))
            for name in (name for name, value in environ.items() if not value):
                os.environ.pop(name, None)
            stack.enter_context(mock.patch.object(sdk, 'load_pins', lambda: pins))
            stack.enter_context(mock.patch.object(sdk, 'RETRY_DELAYS', (0, 0)))
            stack.enter_context(mock.patch.object(sdk.urllib.request, 'build_opener', lambda *handlers: opener))
            if platform:
                stack.enter_context(mock.patch.object(sys, 'platform', platform))
            for patch in patches:
                stack.enter_context(patch)
            captured = stack.enter_context(captured_output())
            status = sdk.main(['install', 'test-linux', '--dest', str(self.work / 'dest')])
        self.assertEqual(status, 1)
        self.assertFalse((self.work / 'dest').exists())
        return captured[0]

    def assert_nothing_private(self, output):
        self.assertIn('error: ', output)
        self.assertNotIn(SENTINEL, output.lower())
        self.assertNotIn(str(ASSET_ID), output)

    def test_local_archive_failures(self):
        local = {'archive': str(self.archive)}
        cases = {
            'missing': lambda: None,
            'a directory': self.archive.mkdir,
            'another size': lambda: self.archive.write_bytes(b'x'),
            'other bytes': lambda: self.archive.write_bytes(bytes(len(self.data))),
        }
        for name, prepare in cases.items():
            with self.subTest(name):
                prepare()
                self.assert_nothing_private(self.failed_install(local))
                if self.archive.is_dir():
                    self.archive.rmdir()

    def test_unreadable_archive_reports_its_type_and_place_only(self):
        self.archive.write_bytes(self.junk)
        output = self.failed_install({'archive': str(self.archive)}, pin=self.junk_pin('linux'))
        self.assert_nothing_private(output)
        self.assertRegex(output, r'error: unexpected ReadError\n  at sdk\.py:\d+ in main\n')

    def test_vendor_tool_failures(self):
        self.archive.write_bytes(self.junk)
        local = {'archive': str(self.archive)}
        path = {'PATH': f'{self.tools}{os.pathsep}{os.environ["PATH"]}'}
        # The stand-in prints its arguments and its whole environment before failing.
        self.tool('pkgutil', '#!/bin/sh\necho "pkgutil: cannot expand $*" >&2\nenv >&2\nexit 3\n')
        output = self.failed_install(local, pin=self.junk_pin('apple'), platform='darwin', environ=path)
        self.assert_nothing_private(output)
        self.assertIn('pkgutil: cannot expand --expand-full ', output)
        self.assertIn('error: pkgutil failed with exit status 3\n', output)
        with self.subTest('not installed'):
            output = self.failed_install(local, pin=self.junk_pin('apple'), platform='darwin',
                                         environ={'PATH': str(self.work / 'empty')})
            self.assert_nothing_private(output)
            self.assertIn('error: pkgutil could not be started (No such file or directory)\n', output)
        with self.subTest('installer cannot run'):
            output = self.failed_install(local, pin=self.junk_pin('windows'), platform='win32')
            self.assert_nothing_private(output)
            self.assertIn('error: the vendor installer could not be started (', output)

    def test_vendor_tool_timeout_names_the_tool_only(self):
        with mock.patch.object(sdk.subprocess, 'run', side_effect=subprocess.TimeoutExpired([STORE, TOKEN], 300)):
            with self.assertRaises(sdk.SdkError) as failure:
                sdk.run_tool('the vendor installer', [STORE, TOKEN], {}, timeout=300)
        self.assertEqual(str(failure.exception), 'the vendor installer did not finish within 300 seconds')

    def test_download_failures(self):
        cases = {
            'variable unset': dict(entry=URL_ENTRY, environ={'URL_ENV_SENTINEL': ''}),
            'not https': dict(entry=URL_ENTRY, environ={'URL_ENV_SENTINEL': URL.replace('https', 'http')}),
            'refused': dict(entry=URL_ENTRY, outcomes=[refusal(404)]),
            'unavailable': dict(entry=URL_ENTRY, outcomes=[refusal(503)] * 3),
            'no connection': dict(entry=URL_ENTRY, outcomes=[urllib.error.URLError(OSError(f'no route to {URL}'))] * 3),
            'tls': dict(entry=URL_ENTRY, outcomes=[urllib.error.URLError(ssl.SSLError(1, f'bad certificate {URL}'))]),
            'dropped': dict(entry=URL_ENTRY, outcomes=[http.client.IncompleteRead(TOKEN.encode())] * 3),
            'other content': dict(entry=URL_ENTRY, outcomes=[b'other content']),
            'token unset': dict(entry=GITHUB_ASSET, environ={'TOKEN_ENV_SENTINEL': ''}),
            'asset refused': dict(entry=GITHUB_ASSET, outcomes=[refusal(401)]),
        }
        for name, case in cases.items():
            with self.subTest(name):
                self.assert_nothing_private(self.failed_install(case.pop('entry'), **case))

    def test_malformed_source_maps(self):
        asset = GITHUB_ASSET['github_release_asset']
        entries = {
            'two kinds': {'archive': str(self.archive), **URL_ENTRY},
            'unknown kind': {'ftp': URL},
            'relative archive': {'archive': f'{STORE}/package'},
            'not an object': URL,
            'bad repository': {'github_release_asset': {**asset, 'repo': 'owner-sentinel/repo sentinel'}},
            'bad asset id': {'github_release_asset': {**asset, 'asset_id': 'asset-sentinel'}},
            'bad token variable': {'github_release_asset': {**asset, 'token_env': [TOKEN]}},
        }
        for name, entry in entries.items():
            with self.subTest(name):
                self.assert_nothing_private(self.failed_install(entry))
        whole = json.dumps({'schema': 1, 'packages': {'test-linux': URL_ENTRY}})
        ambiguous = {'test-linux': URL_ENTRY, sdk.artifact_id(self.pins(self.data)['test-linux']): GITHUB_ASSET}
        for name, sources in {'cut short': whole[:-9], 'another schema': whole.replace('1', '2', 1),
                              'ambiguous': json.dumps({'schema': 1, 'packages': ambiguous})}.items():
            with self.subTest(name):
                self.assert_nothing_private(self.failed_install(sources=sources))

    def test_unexpected_failure_reports_its_type_and_place_only(self):
        self.archive.write_bytes(self.data)
        broken = mock.patch.object(sdk, 'sdk_root', side_effect=RuntimeError(f'{STORE} {TOKEN} {URL}'))
        output = self.failed_install({'archive': str(self.archive)}, patches=[broken])
        self.assert_nothing_private(output)
        self.assertRegex(output, r'\Aerror: unexpected RuntimeError\n(  at \w+\.py:\d+ in \w+\n)+\Z')


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
        outputs = self.work / 'github-output'
        arguments = ('install', 'test-linux', '--dest', str(self.work / 'sdk'), '--archive', str(self.archive(data)),
                     '--target', TARGET, '--github-env', str(exported), '--github-output', str(outputs))
        status, stdout, _ = self.run_main(*arguments)
        self.assertEqual(status, 0)
        self.assertEqual(exported.read_text(), 'EXISTING=1\n' + stdout)
        self.assertEqual(outputs.read_text(), stdout)
        self.assertNotIn(b'\r', outputs.read_bytes())
        self.assertRegex(stdout, r'\ANDI_SDK_DIR=.+/sdk/NDI SDK for Linux\nNDI_RUNTIME_DIR_V6=.+/x86_64-linux-gnu\n'
                         r'GRAFTON_NDI_EXPECTED_RUNTIME=.+/libndi.so.6\n'
                         r'GRAFTON_NDI_EXPECTED_VERSION=6.9.1.0\nGRAFTON_NDI_EXPECTED_EDITION=standard\n\Z')
        status, stdout, stderr = self.run_main(*arguments)
        self.assertEqual((status, stdout), (1, ''))
        self.assertRegex(stderr, r'\Aerror: .+ already exists\n\Z')


if __name__ == '__main__':
    unittest.main()
