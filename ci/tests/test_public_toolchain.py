"""Public cross-toolchain provenance and execution-boundary regressions."""
from copy import deepcopy
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import public_toolchain as tools

PROFILE = 'arm-rpi1-linux-gnueabihf'


class PublicToolchainTests(unittest.TestCase):
    def test_reviewed_recipe_identity_and_complete_receipts(self):
        recipes = json.loads(tools.MANIFEST.read_text())
        identities = [tools.recipe_identity(profile) for profile in recipes['profiles']]
        self.assertEqual(len(identities), 4)
        self.assertEqual(len(set(identities)), 4)
        receipts = tools.expected_package_receipts(PROFILE)
        self.assertEqual(len(receipts), len(recipes['packages']))
        self.assertTrue(all(set(p) == set(tools.RECEIPT_KEYS) for p in receipts))
        self.assertTrue({'libdbus-1-3', 'libsystemd0', 'libc6-dev-armhf-cross'}.issubset(
            p['package'] for p in receipts))
        with self.assertRaises(ValueError):
            tools.recipe_identity('armv6-unknown-linux-gnueabihf')

    def test_recipe_rejects_missing_pins_wrong_architecture_and_dependencies(self):
        original = json.loads(tools.MANIFEST.read_text())
        for mutation in ('hash', 'architecture', 'dependency', 'url'):
            recipe = deepcopy(original)
            if mutation == 'hash':
                del recipe['packages'][0]['sha256']
            elif mutation == 'architecture':
                recipe['packages'][0]['architecture'] = 'aarch64'
            elif mutation == 'dependency':
                recipe['packages'] = [p for p in recipe['packages'] if p['package'] != 'libdbus-1-3']
            else:
                recipe['packages'][0]['url'] = 'https://example.com/compiler.deb'
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as name:
                path = Path(name) / 'manifest.json'; path.write_text(json.dumps(recipe))
                with patch.object(tools, 'MANIFEST', path), self.assertRaises(ValueError):
                    tools.recipe_identity(PROFILE)

    def test_receipt_identity_changes_when_a_reviewed_pin_changes(self):
        original = tools.recipe_identity(PROFILE)
        recipe = json.loads(tools.MANIFEST.read_text())
        recipe['packages'][0]['sha256'] = '0' * 64
        with tempfile.TemporaryDirectory() as name:
            path = Path(name) / 'manifest.json'; path.write_text(json.dumps(recipe))
            with patch.object(tools, 'MANIFEST', path):
                self.assertNotEqual(original, tools.recipe_identity(PROFILE))

    def test_cached_package_is_rehashed_before_reuse(self):
        value = b'public fixture archive'
        package = {'sha256': hashlib.sha256(value).hexdigest(), 'size': len(value)}
        with tempfile.TemporaryDirectory() as name:
            cache = Path(name); path = cache / (package['sha256'] + '.deb')
            path.write_bytes(value)
            with patch.object(tools.urllib.request, 'urlopen') as network:
                self.assertEqual(tools._download(package, cache), path)
                path.write_bytes(b'X' * len(value))
                with self.assertRaises(ValueError):
                    tools._download(package, cache)
                network.assert_not_called()

    def test_archive_member_cannot_escape_via_traversal_or_symlink(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name) / 'root'; root.mkdir()
            (root / 'escape').symlink_to(Path(name))
            for member in ('../outside', '/absolute', 'escape/outside'):
                with self.subTest(member=member), self.assertRaises(ValueError):
                    tools._member_path(root, member)

    def test_cache_replacement_cannot_change_private_extraction_snapshot(self):
        value = b'public fixture archive'
        package = {'sha256': hashlib.sha256(value).hexdigest(), 'size': len(value)}
        with tempfile.TemporaryDirectory() as name:
            root = Path(name); archive = root / 'shared-cache.deb'
            archive.write_bytes(value)
            snapshot = tools._snapshot_package(package, archive, root / 'snapshot.deb')
            archive.write_bytes(b'X' * len(value))
            self.assertEqual(snapshot.read_bytes(), value)
            with self.assertRaises(ValueError):
                tools._snapshot_package(package, archive, root / 'corrupted-snapshot.deb')

    def test_download_does_not_remove_another_writers_temporary_file(self):
        value = b'public fixture archive'
        package = {'sha256': hashlib.sha256(value).hexdigest(), 'size': len(value),
                   'url': 'https://deb.debian.org/debian/pool/main/f/fixture/fixture.deb'}
        response = io.BytesIO(value); response.url = package['url']
        with tempfile.TemporaryDirectory() as name:
            root = Path(name); other = root / (package['sha256'] + '.part')
            other.write_bytes(b'another active download')
            with patch.object(tools.urllib.request, 'urlopen', return_value=response):
                self.assertEqual(tools._download(package, root).read_bytes(), value)
            self.assertEqual(other.read_bytes(), b'another active download')
            self.assertEqual(list(root.glob('*.part')), [other])

    def test_extracts_package_data_without_executing_maintainer_script(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name); package = root / 'package'
            (package / 'DEBIAN').mkdir(parents=True)
            (package / 'DEBIAN/control').write_text(
                'Package: public-tool-fixture\nVersion: 1\nArchitecture: all\n'
                'Maintainer: Test <test@example.invalid>\nDescription: fixture\n')
            marker = root / 'maintainer-script-ran'
            (package / 'DEBIAN/postinst').write_text('#!/bin/sh\ntouch ' + str(marker) + '\n')
            (package / 'DEBIAN/postinst').chmod(0o755)
            (package / 'usr/share').mkdir(parents=True)
            (package / 'usr/share/payload').write_text('public data')
            archive = root / 'fixture.deb'
            subprocess.run(['dpkg-deb', '--build', str(package), str(archive)],
                           check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            tools._extract_deb({'package': 'public-tool-fixture', 'version': '1',
                                'architecture': 'all'}, archive, root / 'output')
            self.assertEqual((root / 'output/usr/share/payload').read_text(), 'public data')
            self.assertFalse(marker.exists())

    def test_downloaded_compiler_receives_no_sdk_acquisition_credentials(self):
        secrets = {'NDI_SDK_ARCHIVE_TOKEN': 'secret', 'CUSTOM_TOKEN': 'secret-custom',
                   'PRIVATE_URL': 'https://example.invalid/secret',
                   'NDI_SDK_SOURCES_JSON': json.dumps({'schema': 1, 'packages': {
                       'current-standard-linux': {'github_release_asset': {
                           'repo': 'owner/private', 'asset_id': 123, 'token_env': 'CUSTOM_TOKEN'}},
                       '6.3.2-standard-linux': {'url_env': 'PRIVATE_URL'}}})}
        class StopAtCompiler(Exception):
            pass
        with tempfile.TemporaryDirectory() as name, patch.dict(os.environ, secrets):
            with patch.object(tools.subprocess, 'run', side_effect=StopAtCompiler) as compiler:
                with self.assertRaises(StopAtCompiler):
                    tools._self_test(Path(name))
                environment = compiler.call_args.kwargs['env']
                self.assertTrue(set(secrets).isdisjoint(environment))
                self.assertIn('PATH', environment)

    def test_existing_bundle_is_never_trusted_as_verified(self):
        with tempfile.TemporaryDirectory() as name:
            (Path(name) / 'ci-toolchain.json').write_text('{}')
            with self.assertRaisesRegex(ValueError, 'output must be empty'):
                tools.prepare(PROFILE, name)


if __name__ == '__main__':
    unittest.main()
