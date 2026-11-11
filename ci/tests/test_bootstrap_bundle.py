"""Portable bootstrap tests use authored fixtures and no GitHub or vendor SDK."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import bootstrap_bundle
import prepare_store
import provision
import sdk_store
from test_sdk_store import fixture_archive


class BootstrapTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='ndi-bootstrap-tests-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.store = self.root/'SDK store $() literal'
        linux, linux_package = fixture_archive(self.root, 'linux.tar.gz')
        android, android_package = fixture_archive(self.root, 'android.tar.gz', target_platform='android')
        windows = self.root/'windows.exe'
        windows.write_bytes(b'Not executable. Independent retained Windows fixture.')
        windows_package = provision.resolved_package('windows', {'windows': {
            'version': '6.4.0.2', 'edition': 'standard', 'platform': 'windows',
            'archive_format': 'windows-installer', 'version_evidence': 'archive-sha256',
            'sha256': prepare_store.digest(windows)}})
        self.catalog = {'linux': linux_package, 'android': android_package, 'windows': windows_package}
        self.archives = dict(enumerate((android, linux, windows), start=101))
        self.sources = {'schema': 1, 'packages': {key: {'github_release_asset': {
            'repo': 'fixture-private/sdk-archives', 'asset_id': i}}
            for i, key in enumerate(sorted(self.catalog), start=101)}}
        self.environment = mock.patch.dict(os.environ, {
            'GITHUB_ACTIONS': 'false', 'NDI_SDK_ARCHIVE_TOKEN': 'fixture-acquisition-secret',
            'GH_TOKEN': 'fixture-gh-secret', 'GITHUB_TOKEN': 'fixture-github-secret',
            'PYTHONPATH': '/nonexistent-fixture-injection', 'BASH_ENV': '/nonexistent-fixture-script'})
        self.environment.start()
        self.addCleanup(self.environment.stop)

    def bundle(self):
        archive = self.root/'bundle.tar.gz'
        bootstrap_bundle.create_bundle(self.sources, archive, catalog=self.catalog)
        bundle = self.root/'bundle'
        bundle.mkdir()
        with tarfile.open(archive) as stream:
            for member in stream:
                self.assertTrue(member.isfile())
                self.assertIn(member.name, prepare_store.FILES | {'bootstrap.json'})
                (bundle/member.name).write_bytes(stream.extractfile(member).read())
        return bundle

    def download(self, source, expected, destination):
        self.assertEqual(os.environ['NDI_SDK_ARCHIVE_TOKEN'], 'fixture-acquisition-secret')
        shutil.copyfile(self.archives[source['asset_id']], destination)
        self.assertEqual(prepare_store.digest(destination), expected)

    def rewrite_manifest_hash(self, bundle, name):
        path = bundle/'bootstrap.json'
        data = json.loads(path.read_text())
        data['files'][name] = prepare_store.digest(bundle/name)
        path.write_text(json.dumps(data))

    def test_deterministic_bundle_is_exact_and_contains_only_locators(self):
        one, two = self.root/'one.tar.gz', self.root/'two.tar.gz'
        first = bootstrap_bundle.create_bundle(self.sources, one, catalog=self.catalog)
        second = bootstrap_bundle.create_bundle(self.sources, two, catalog=self.catalog)
        self.assertEqual(one.read_bytes(), two.read_bytes())
        self.assertEqual(first, second)
        self.assertEqual(first['install_packages'], ['android', 'linux'])
        with tarfile.open(one) as stream:
            self.assertEqual(set(stream.getnames()), prepare_store.FILES | {'bootstrap.json'})
            self.assertTrue(all(member.isfile() and member.mode == 0o600 for member in stream))
            data = stream.extractfile('sources.json').read()
            self.assertNotIn(b'fixture-acquisition-secret', data)
            self.assertNotIn(str(self.root).encode(), data)
        self.assertEqual(one.stat().st_mode & 0o777, 0o600)

    def test_canonical_catalog_selects_twelve_archives_and_six_installations(self):
        catalog = provision.catalog_packages(bootstrap_bundle.HERE/'sdk-packages.json')
        sources = {'schema': 1, 'packages': {key: {'github_release_asset': {
            'repo': 'fixture-private/sdk-archives', 'asset_id': i + 1}}
            for i, key in enumerate(catalog)}}
        result = bootstrap_bundle.create_bundle(sources, self.root/'all.tar.gz', catalog=catalog)
        self.assertEqual(len(result['packages']), 12)
        self.assertEqual(len(result['install_packages']), 6)

    def test_rejects_nonportable_or_ambiguous_sources_and_checkout_output(self):
        for entry in ({'archive': str(self.root/'archive')}, {'url_env': 'FIXTURE_URL'},
                      {'github_release_asset': {'repo': 'fixture-private/sdk', 'asset_id': 1,
                                                'token_env': 'OTHER_TOKEN'}}):
            sources = json.loads(json.dumps(self.sources))
            sources['packages']['linux'] = entry
            with self.subTest(entry=entry), self.assertRaises(ValueError):
                bootstrap_bundle.create_bundle(sources, self.root/'bad.tar.gz', catalog=self.catalog)
        sources = json.loads(json.dumps(self.sources))
        sources['packages'][self.catalog['linux']['artifact_id']] = sources['packages']['linux']
        with self.assertRaisesRegex(ValueError, 'exactly one'):
            bootstrap_bundle.create_bundle(sources, self.root/'bad.tar.gz', catalog=self.catalog)
        with self.assertRaisesRegex(ValueError, 'outside the source checkout'):
            bootstrap_bundle.create_bundle(self.sources, bootstrap_bundle.HERE/'private.tar.gz', catalog=self.catalog)

    def test_modified_helper_symlink_or_extra_file_rejected_before_side_effects(self):
        bundle = self.bundle()
        original = (bundle/'common.py').read_bytes()
        with mock.patch.object(provision, 'download_github_asset') as download:
            with (bundle/'common.py').open('ab') as output:
                output.write(b'\n# unexpected mutation\n')
            with self.assertRaisesRegex(ValueError, 'SHA256'):
                prepare_store.prepare(bundle, self.store)
            (bundle/'common.py').write_bytes(original)
            (bundle/'unexpected').write_text('not reviewed')
            with self.assertRaisesRegex(ValueError, 'seven'):
                prepare_store.prepare(bundle, self.store)
            (bundle/'unexpected').unlink()
            (bundle/'common.py').unlink()
            (bundle/'common.py').symlink_to(bootstrap_bundle.HERE/'common.py')
            with self.assertRaisesRegex(ValueError, 'nonregular'):
                prepare_store.prepare(bundle, self.store)
            download.assert_not_called()
        self.assertFalse(self.store.exists())

    def test_manifest_cannot_skip_packages_or_android_installation(self):
        bundle = self.bundle()
        manifest_path = bundle/'bootstrap.json'
        manifest = json.loads(manifest_path.read_text())
        manifest['install_packages'] = ['linux']
        manifest_path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, 'Linux and Android'):
            prepare_store.prepare(bundle, self.store)
        manifest['install_packages'] = ['android', 'linux']
        manifest['packages'] = ['linux', 'windows']
        manifest_path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, 'package set'):
            prepare_store.prepare(bundle, self.store)

    def test_complete_store_strips_child_secrets_and_reruns_without_network(self):
        bundle = self.bundle()
        actual_run = subprocess.run
        commands = []

        def run(command, **kwargs):
            commands.append(command)
            for key in ('NDI_SDK_ARCHIVE_TOKEN', 'GH_TOKEN', 'GITHUB_TOKEN', 'PYTHONPATH', 'BASH_ENV'):
                self.assertNotIn(key, kwargs['env'])
            return actual_run(command, **kwargs)

        with (mock.patch.object(provision, 'download_github_asset', side_effect=self.download) as download,
              mock.patch.object(prepare_store.subprocess, 'run', side_effect=run)):
            result = prepare_store.prepare(bundle, self.store)
        self.assertEqual(download.call_count, 3)
        self.assertEqual(result['installed_artifacts'], 2)
        index = sdk_store.load_index(self.store)
        self.assertEqual(len(index['archives']), 3)
        self.assertEqual(len(index['artifacts']), 2)
        self.assertEqual(sum('populate' in command for command in commands), 2)
        self.assertFalse(list(self.store.rglob('sources.json')))
        self.assertFalse(list(self.store.rglob('__pycache__')))
        for path in self.store.rglob('*.json'):
            value = path.read_text()
            self.assertNotIn('fixture-private', value)
            self.assertNotIn('fixture-acquisition-secret', value)
            self.assertNotIn(str(bundle), value)
        with mock.patch.object(provision, 'download_github_asset', side_effect=AssertionError('network forbidden')):
            self.assertEqual(prepare_store.prepare(bundle, self.store), result)
        # Execute the portable runner in a fresh interpreter with no checkout in
        # PYTHONPATH: imports, store tools and reuse must all work independently.
        result = actual_run([sys.executable, str(bundle/'prepare_store.py'), '--store', str(self.store)],
                            capture_output=True, text=True, env=prepare_store.store_environment(self.sources, provision))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_partial_acquisition_failure_reuses_only_verified_retained_content(self):
        bundle = self.bundle()
        calls = []

        def interrupted(source, expected, destination):
            calls.append(source['asset_id'])
            if len(calls) == 2:
                raise ValueError('independent interruption')
            self.download(source, expected, destination)

        with mock.patch.object(provision, 'download_github_asset', side_effect=interrupted):
            with self.assertRaisesRegex(ValueError, 'interruption'):
                prepare_store.prepare(bundle, self.store)
        self.assertEqual(len(sdk_store.load_index(self.store)['archives']), 1)
        with mock.patch.object(provision, 'download_github_asset', side_effect=self.download) as download:
            prepare_store.prepare(bundle, self.store)
        self.assertEqual(download.call_count, 2)
        entry = next(iter(sdk_store.load_index(self.store)['archives'].values()))
        (self.store/entry['path']).write_bytes(b'tampered retained content')
        with mock.patch.object(provision, 'download_github_asset') as download:
            with self.assertRaisesRegex(ValueError, 'changed|SHA256|size'):
                prepare_store.prepare(bundle, self.store)
            download.assert_not_called()

    def test_wrong_download_hash_and_bundle_inside_store_fail_closed(self):
        bundle = self.bundle()
        with mock.patch.object(provision, 'download_github_asset', side_effect=lambda source, expected, path: path.write_bytes(b'wrong bytes')):
            with self.assertRaises(subprocess.CalledProcessError):
                prepare_store.prepare(bundle, self.store)
        self.assertFalse((self.store/'index.json').exists())
        with self.assertRaisesRegex(ValueError, 'outside the SDK store'):
            prepare_store.prepare(bundle, bundle/'store')


if __name__ == '__main__':
    unittest.main()
