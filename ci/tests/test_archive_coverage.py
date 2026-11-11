"""Only explicit active package identities can satisfy archive retention."""
import contextlib
import copy
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import archive_coverage
import sdk_store
from common import ROOT, digest, write_json


class ArchiveCoverageTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='ndi-archive-coverage-')
        self.root = Path(self.temporary.name)
        self.store = self.root / 'private store'
        self.requirements = archive_coverage.load_requirements(ROOT / 'ci/archive-requirements.json')

    def tearDown(self):
        self.temporary.cleanup()

    def package(self, version, tag, *, edition='standard', platform='windows'):
        # Independent fixture bytes: retention must not run a vendor installer.
        formats = {'windows': 'windows-installer', 'apple': 'apple-pkg',
                   'linux': 'embedded-tar', 'android': 'embedded-tar'}
        path = self.root / (tag + '.archive')
        path.write_bytes(('independent full archive fixture: ' + tag).encode())
        package = {'version': version, 'edition': edition, 'platform': platform,
                   'sha256': digest(path), 'archive_format': formats[platform],
                   'version_evidence': 'archive-sha256'}
        return path, package

    def row(self, report, key='6.3.2-standard-windows'):
        return next(row for row in report['requirements'] if row['package'] == key)

    def test_active_policy_is_explicitly_twelve_packages_and_matches_catalog(self):
        report = archive_coverage.coverage(self.requirements, {})
        expected = {f'6.3.2-standard-{p}' for p in self.requirements['platforms']}
        expected |= {f'current-{e}-{p}' for e in ('standard', 'advanced') for p in self.requirements['platforms']}
        self.assertEqual({r['package'] for r in report['requirements']}, expected)
        self.assertEqual(set(json.loads((ROOT/'ci/sdk-packages.json').read_text())), expected)
        self.assertEqual(report['desired_intersections'], 12)
        self.assertEqual(report['verified_intersections'], 0)
        self.assertFalse(report['complete'])

    def test_a_catalog_hash_without_retained_bytes_is_missing(self):
        _, package = self.package('6.3.2', 'pinned-only')
        report = archive_coverage.coverage(self.requirements, {'6.3.2-standard-windows': package})
        row = self.row(report)
        self.assertEqual(row['state'], 'missing')
        self.assertEqual(row['reason'], 'reviewed-archive-not-retained-and-verified')

    def test_all_twelve_complete_archives_satisfy_policy_without_installation(self):
        files, catalog = {}, {}
        for key, expected in archive_coverage.required_packages(self.requirements).items():
            files[key], catalog[key] = self.package(expected['version'], key,
                edition=expected['edition'], platform=expected['platform'])
        for key, archive in files.items():
            sdk_store.retain(self.store, key, archive=archive, catalog=catalog)
        report = archive_coverage.coverage(self.requirements, catalog, self.store)
        self.assertTrue(report['complete'])
        self.assertEqual(report['verified_intersections'], 12)
        self.assertEqual(report['retained_archive_count'], 12)
        artifacts = [r['artifacts'][0] for r in report['requirements']]
        self.assertTrue(all(a['complete_original_archive'] and not a['installed'] for a in artifacts))
        self.assertTrue(all(a['architecture_inventory'] == 'not-assessed-by-this-retention-audit' for a in artifacts))
        self.assertNotIn(str(self.root), json.dumps(report))

    def test_old_archive_cannot_satisfy_a_changed_catalog_pin(self):
        first, first_package = self.package('6.3.2', 'old-build')
        rebuilt, rebuilt_package = self.package('6.3.2', 'reviewed-rebuild')
        key = '6.3.2-standard-windows'
        sdk_store.retain(self.store, key, archive=first, catalog={key: first_package})
        report = archive_coverage.coverage(self.requirements, {key: rebuilt_package}, self.store)
        self.assertEqual(report['verified_intersections'], 0)
        self.assertEqual(len(report['outside_scope_artifacts']), 1)
        sdk_store.retain(self.store, key, archive=rebuilt, catalog={key: rebuilt_package})
        report = archive_coverage.coverage(self.requirements, {key: rebuilt_package}, self.store)
        self.assertEqual(report['verified_intersections'], 1)
        self.assertEqual(report['retained_archive_count'], 2)
        self.assertEqual(self.row(report)['artifacts'][0]['archive_sha256'], rebuilt_package['sha256'])
        # The store's old catalog snapshot alone cannot invent an active pin.
        self.assertEqual(archive_coverage.coverage(self.requirements, {}, self.store)['verified_intersections'], 0)

    def test_another_fourth_component_requires_an_explicit_policy_update(self):
        _, package = self.package('6.4.0.3', 'future-build')
        with self.assertRaisesRegex(ValueError, 'contradicts the active retention policy'):
            archive_coverage.coverage(self.requirements, {'current-standard-windows': package})
        self.assertEqual(archive_coverage.version_components('6.3.2'), archive_coverage.version_components('6.3.2.0'))
        changed = copy.deepcopy(self.requirements)
        changed['releases'][1]['version'] = '6.5.0.1'
        with self.assertRaisesRegex(ValueError, 'contradicts its family'):
            archive_coverage.load_requirements(changed)

    def test_changed_archive_cannot_produce_a_green_report(self):
        archive, package = self.package('6.3.2', 'tamper')
        key = '6.3.2-standard-windows'
        sdk_store.retain(self.store, key, archive=archive, catalog={key: package})
        retained = next(iter(sdk_store.load_index(self.store)['archives'].values()))
        (self.store / retained['path']).write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, 'retained archive changed'):
            archive_coverage.coverage(self.requirements, {key: package}, self.store)

    def test_other_releases_and_baseline_advanced_do_not_expand_policy(self):
        catalog, files = {}, {}
        for key, version, edition in [('historical', '6.1.1', 'standard'),
                                      ('future', '6.99.0.1', 'standard'),
                                      ('baseline-advanced', '6.3.2', 'advanced')]:
            files[key], catalog[key] = self.package(version, key, edition=edition)
        for key, archive in files.items():
            sdk_store.retain(self.store, key, archive=archive, catalog=catalog)
        report = archive_coverage.coverage(self.requirements, catalog, self.store)
        self.assertEqual(report['desired_intersections'], 12)
        self.assertEqual(report['verified_intersections'], 0)
        self.assertEqual(len(report['outside_scope_artifacts']), 3)
        self.assertEqual(report['outside_scope_catalog_packages'], sorted(catalog))

    def test_cli_missing_coverage_fails_unless_report_only_is_explicit(self):
        catalog = self.root / 'catalog.json'
        write_json(catalog, {})
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(archive_coverage.main(['--catalog', str(catalog)]), 2)
            self.assertEqual(archive_coverage.main(['--catalog', str(catalog), '--allow-missing']), 0)


if __name__ == '__main__':
    unittest.main()
