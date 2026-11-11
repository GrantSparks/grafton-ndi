"""Policy, provenance and extraction checks; never require SDKs or networking."""
from copy import deepcopy
from collections import Counter
from pathlib import Path
import io
import json
import re
import sys
import tarfile
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from aggregate import validate_cells as validate
from common import ROOT, native_modes, profiles, read_json, required_steps
from evidence_fixture import confinement_receipt, native_report
from plan import make_plan, make_groups, representative_first, provisioning_status, COMPATIBILITY_BASELINES, VALIDATION_VERSIONS, PROFILE_NIGHTLY, SUPPORTED_PROFILES, UNSUPPORTED_PROFILES
from mobile import choose_simulator, check_app_resolution
from provision import extract_tar, provision
from native import REGRESSION_TESTS, regression_tests


class MatrixTests(unittest.TestCase):
    def test_diverse_representatives_start_the_required_matrix(self):
        cells = make_plan(profiles())
        expected = [('x86_64-linux-gnu', 'standard', 'native'),
            ('windows-x64', 'standard', 'native'), ('macos-arm64', 'standard', 'native'),
            ('ios-arm64', 'standard', 'mobile'), ('android-x86_64', 'standard', 'mobile'),
            ('aarch64-rpi4-linux-gnueabi', 'advanced', 'link'),
            ('windows-x86', 'advanced', 'link'), ('tvos-x86_64-sim', 'standard', 'mobile')]
        self.assertEqual([(c['profile'], c['edition'], c['suite']) for c in cells[:8]], expected)
        self.assertTrue(all(c['headers'] == c['runtime'] == 'current' for c in cells[:8]))

    def test_priority_changes_only_order_and_preserves_remaining_relative_order(self):
        cells = sorted(make_plan(profiles()), key=lambda cell: cell['id'], reverse=True)
        original = deepcopy(cells)
        ordered = representative_first(cells)
        self.assertEqual(cells, original)
        self.assertEqual({c['id']: c for c in ordered}, {c['id']: c for c in original})
        selected = {c['id'] for c in ordered[:8]}
        self.assertEqual(ordered[8:], [c for c in original if c['id'] not in selected])
        self.assertEqual(representative_first(ordered), ordered)
        self.assertTrue(all(any(c is old for old in cells) for c in ordered))
        with self.assertRaisesRegex(ValueError, 'representative matrix cells'):
            representative_first(ordered[1:])

    def test_matrix_outputs_are_isolated_from_acquisition_secrets(self):
        workflow = (ROOT / '.github/workflows/rust.yml').read_text()
        jobs = dict(re.findall(r'^  ([\w-]+):\n(.*?)(?=^  [\w-]+:|\Z)',
                               workflow.split('jobs:\n', 1)[1], re.M | re.S))
        # GitHub masks each nonempty line of a multiline secret, including
        # generic JSON braces. Even a public matrix cannot share that job.
        self.assertIn('--github-output', jobs['plan'])
        self.assertNotIn('secrets.', jobs['plan'])
        self.assertIn('secrets.NDI_SDK_SOURCES_JSON', jobs['sources'])
        self.assertNotIn('--github-output', jobs['sources'])
        self.assertNotIn('packages', jobs)
        self.assertIn('needs: [plan, sources]', jobs['sdk'])
        self.assertIn('needs.sources.result', jobs['required-ci'])

    def test_each_profile_edition_has_a_real_link_cell(self):
        rows = profiles()
        cells = make_plan(rows)
        expected = {(name, edition) for name in SUPPORTED_PROFILES for edition in rows[name]['editions'].split(',')}
        actual = {(c['profile'], c['edition']) for c in cells if c['suite'] in ('native', 'link', 'mobile')}
        self.assertEqual(expected, actual)
        self.assertLessEqual(len(cells), 256)
        self.assertEqual(len(cells), len({c['id'] for c in cells}))
        self.assertTrue(all(c['runner'] in {'ubuntu-24.04', 'ubuntu-24.04-arm', 'windows-2025', 'macos-15', 'macos-15-intel'} for c in cells))
        self.assertTrue(all('self-hosted' not in c['runner'] for c in cells))
        self.assertEqual(len(cells), 52)
        self.assertEqual(len(SUPPORTED_PROFILES), 20)
        self.assertEqual(len(UNSUPPORTED_PROFILES), 19)
        self.assertEqual(set(rows), SUPPORTED_PROFILES | UNSUPPORTED_PROFILES)
        self.assertFalse(SUPPORTED_PROFILES & UNSUPPORTED_PROFILES)
        self.assertFalse({c['profile'] for c in cells} & UNSUPPORTED_PROFILES)

    def test_new_artifact_requires_reviewed_support_policy(self):
        rows = profiles()
        rows['new-vendor-target'] = dict(rows['arm-rpi1-linux-gnueabihf'], id='new-vendor-target')
        with self.assertRaisesRegex(ValueError, 'explicit support policy'):
            make_plan(rows)

    def test_required_tools_use_public_pins_without_bsp_secrets(self):
        cells = make_plan(profiles())
        _, toolchains, issues = provisioning_status(cells, read_json(ROOT / 'ci/sdk-packages.json'), {})
        self.assertEqual(issues, [])
        self.assertEqual(set(toolchains), {f'arm-rpi{i}-linux-gnueabihf' for i in range(1, 5)})
        for value in toolchains.values():
            self.assertEqual(value['provider'], 'public-gnu')
            self.assertRegex(value['recipe_sha256'], r'^[a-f0-9]{64}$')
            self.assertTrue(value['packages'])
        rebuilt_std = [cell for cell in cells if cell['build_std']]
        self.assertEqual({cell['profile'] for cell in rebuilt_std}, {'tvos-x86_64-sim'})
        self.assertEqual({cell['toolchain'] for cell in rebuilt_std}, {PROFILE_NIGHTLY})
        self.assertRegex(PROFILE_NIGHTLY, r'^nightly-\d{4}-\d{2}-\d{2}$')

    def test_both_real_old_header_and_old_runtime_cells_exist(self):
        cells = make_plan(profiles())
        for version in COMPATIBILITY_BASELINES:
            for profile in ('x86_64-linux-gnu', 'windows-x64', 'macos-arm64'):
                pairs = {(c['headers'], c['runtime']) for c in cells if c['suite'] == 'compat' and c['profile'] == profile}
                self.assertEqual(pairs, {(version, 'current'), ('current', version)})

    def test_missing_retained_package_fails_without_reducing_declared_rows(self):
        cells = make_plan(profiles())
        packages = read_json(ROOT / 'ci/sdk-packages.json')
        packages.pop('6.3.2-standard-android')
        _, _, issues = provisioning_status(cells, packages, {})
        self.assertIn('6.3.2-standard-android: package metadata is absent', issues)
        self.assertEqual(cells, make_plan(profiles()))
        self.assertTrue(any(cell['header_package'] == '6.3.2-standard-android' for cell in cells))

    def test_missing_required_profile_fails_instead_of_shrinking_matrix(self):
        rows = profiles()
        rows.pop('windows-x86')
        with self.assertRaisesRegex(ValueError, 'missing mandatory'):
            make_plan(rows)

    def test_mobile_execution_is_explicit_and_package_checks_remain_required(self):
        cells = make_plan(profiles())
        execution = {('android-x86_64', 'standard'): 'emulator', ('tvos-x86_64-sim', 'standard'): 'simulator'}
        mobile = [c for c in cells if c['suite'] == 'mobile']
        self.assertEqual(len(mobile), 18)
        for cell in mobile:
            coverage = execution.get((cell['profile'], cell['edition']), 'package')
            self.assertEqual(cell['coverage'], coverage)
            self.assertIn('mobile-package', required_steps(cell))
            self.assertIn('build-library', required_steps(cell))
            self.assertNotIn('link-examples', required_steps(cell))
            self.assertEqual('mobile-runtime' in required_steps(cell), coverage != 'package')

    def test_explicit_retained_scope_does_not_infer_coverage_from_availability(self):
        cells = make_plan(profiles())
        self.assertEqual(COMPATIBILITY_BASELINES, ('6.3.2',))
        self.assertEqual(VALIDATION_VERSIONS, ('6.3.2', 'current'))
        self.assertEqual({cell[field] for cell in cells for field in ('headers', 'runtime')}, set(VALIDATION_VERSIONS))
        packages = read_json(ROOT / 'ci/sdk-packages.json')
        required = {cell[field] for cell in cells for field in ('header_package', 'runtime_package')}
        self.assertEqual(required, set(packages))
        self.assertEqual(len(packages), 12)
        for package in packages.values():
            self.assertRegex(package['sha256'], r'^[a-f0-9]{64}$')
            self.assertTrue(package['artifact_id'].endswith(package['sha256']))
        msrv = {(cell['profile'], cell['headers'], cell['toolchain']) for cell in cells if cell['suite'] == 'msrv'}
        self.assertEqual(msrv, {('x86_64-linux-gnu', version, '1.88.0') for version in VALIDATION_VERSIONS}
                         | {('android-arm64', '6.3.2', '1.88.0')})

    def test_required_scope_has_six_native_witnesses_and_sixteen_runner_groups(self):
        cells = make_plan(profiles())
        self.assertEqual(Counter(c['suite'] for c in cells),
            {'native': 6, 'link': 16, 'mobile': 18, 'compat': 6, 'msrv': 3, 'features': 2, 'semver': 1})
        self.assertEqual({(c['profile'], c['edition']) for c in cells if c['suite'] == 'native'},
            {(profile, edition) for profile in ('x86_64-linux-gnu', 'windows-x64', 'macos-arm64')
             for edition in ('standard', 'advanced')})
        groups = make_groups(cells)
        self.assertEqual([(group['id'], len(group['cells'])) for group in groups], [
            ('native-linux-standard', 1), ('native-windows-standard', 1), ('native-macos-standard', 1),
            ('native-linux-advanced', 1), ('native-windows-advanced', 1), ('native-macos-advanced', 1),
            ('build-linux-32', 10), ('build-linux-arm64', 2), ('build-windows-32', 2),
            ('build-apple-intel', 6), ('build-apple-devices', 6), ('build-android', 9),
            ('compat-linux', 2), ('compat-windows', 2), ('compat-macos', 2), ('quality-linux', 5)])
        original = deepcopy(cells)
        self.assertEqual(make_groups(cells), groups)
        self.assertEqual(cells, original)
        self.assertEqual(Counter(identity for group in groups for identity in group['cells']),
                         Counter(cell['id'] for cell in cells))
        by_id = {cell['id']: cell for cell in cells}
        owned = []
        for group in groups:
            members = [by_id[identity] for identity in group['cells']]
            self.assertEqual({cell['runner'] for cell in members}, {group['runner']})
            available = {cell[role] for cell in members for role in ('header_package', 'runtime_package')}
            self.assertLessEqual(set(group['availability_packages']), available)
            owned.extend(group['availability_packages'])
            self.assertEqual(group['needs_multilib'], group['id'] == 'build-linux-32')
            self.assertEqual(group['android'], group['id'] == 'build-android')
            self.assertEqual('emulator_api' in group, group['id'] == 'build-android')
            self.assertEqual('emulator_arch' in group, group['id'] == 'build-android')
        self.assertEqual(Counter(owned), Counter(read_json(ROOT / 'ci/sdk-packages.json').keys()))
        android = next(group for group in groups if group['id'] == 'build-android')
        self.assertEqual((android['emulator_api'], android['emulator_arch']), (35, 'x86_64'))

    def test_extended_scope_preserves_full_permutation_policy(self):
        cells = make_plan(profiles(), 'extended')
        self.assertEqual(len(cells), 82)
        self.assertEqual(Counter(c['suite'] for c in cells),
            {'native': 14, 'link': 8, 'mobile': 18, 'compat': 21, 'msrv': 16, 'features': 2, 'semver': 3})
        groups = make_groups(cells, 'extended')
        self.assertEqual(len(groups), 82)
        self.assertEqual([group['cells'] for group in groups], [[cell['id']] for cell in cells])
        native = {cell['profile'] for cell in cells if cell['suite'] == 'native'}
        self.assertEqual(native, {'x86_64-linux-gnu', 'i686-linux-gnu', 'aarch64-rpi4-linux-gnueabi',
                                 'windows-x64', 'windows-x86', 'macos-arm64', 'macos-x86_64'})
        for profile in native:
            self.assertEqual({(cell['headers'], cell['runtime']) for cell in cells
                              if cell['suite'] == 'compat' and cell['profile'] == profile},
                             {('6.3.2', '6.3.2'), ('6.3.2', 'current'), ('current', '6.3.2')})
        self.assertEqual({(cell['profile'], cell['edition']) for cell in cells
                          if cell['coverage'] in ('emulator', 'simulator')},
            {(profile, edition) for profile in ('android-x86', 'android-x86_64', 'ios-x86_64-sim', 'tvos-x86_64-sim')
             for edition in ('standard', 'advanced')})

    def test_unknown_scopes_and_ambiguous_group_membership_fail(self):
        cells = make_plan(profiles())
        with self.assertRaisesRegex(ValueError, 'unknown CI scope'):
            make_plan(profiles(), 'automatic')
        with self.assertRaisesRegex(ValueError, 'unknown CI scope'):
            make_groups(cells, 'automatic')
        with self.assertRaisesRegex(ValueError, 'unique identities'):
            make_groups(cells + [cells[0]])
        with self.assertRaisesRegex(ValueError, 'exactly one hosted runner'):
            make_groups([c for c in cells if c['profile'] != 'windows-x86'])
        bad = deepcopy(cells)
        next(c for c in bad if c['profile'] == 'visionos-arm64')['runner'] = 'macos-15-intel'
        with self.assertRaisesRegex(ValueError, 'exactly one hosted runner'):
            make_groups(bad)


class AggregateTests(unittest.TestCase):
    def fixture(self, profile='x86_64-linux-gnu'):
        cell = next(c for c in make_plan(profiles()) if c['suite'] == 'native' and c['profile'] == profile and c['edition'] == 'standard')
        sha = 'a' * 64
        plan = dict(schema=1, commit='commit', tree='tree', suite_fingerprint='suite', lockfile_sha256='lock', dirty=False,
                    cells=[cell], packages={cell['header_package']: {'sha256': sha, 'artifact_id': 'fixture-artifact'}})
        record = dict(schema=1, commit='commit', tree='tree', suite_fingerprint='suite', lockfile_sha256='lock', dirty=False,
            cell=cell, status='passed', coverage='native', rustc='rustc fixture',
            runtime_binary_sha256='d' * 64, runtime_identity={
                'schema': 1, 'version': [6, 4, 0, 2], 'edition': cell['edition'], 'edition_verified': True},
            runtime_probe_sha256='a' * 64, runtime_confinement_kind=(
                'macos-pf-offline' if profile.startswith('macos-') else
                'windows-firewall' if profile.startswith('windows-') else 'linux-network-namespace'),
            headers={'package': cell['header_package'], 'archive_sha256': sha, 'artifact_id': 'fixture-artifact', 'inventory_sha256': 'e' * 64},
            runtime={'package': cell['runtime_package'], 'archive_sha256': sha, 'artifact_id': 'fixture-artifact', 'inventory_sha256': 'e' * 64},
            sdk_regressions=native_report(cell),
            native_suites={mode: native_report(cell, mode) for mode in native_modes(cell)},
            commands=[{'label': label, 'exit_code': 0} for label in required_steps(cell)])
        if profile.startswith('macos-'):
            record.update(runtime_probe_sha256='a' * 64,
                          runtime_confinement=confinement_receipt('runtime-identity'))
        return plan, record

    def test_complete_exact_provenance_passes(self):
        plan, record = self.fixture()
        self.assertEqual(validate(plan, [record])['cells_passed'], 1)

    def test_missing_duplicate_and_skipped_cells_fail(self):
        plan, record = self.fixture()
        for records in ([], [record, record], [{**record, 'status': 'skipped'}]):
            with self.subTest(records=len(records)), self.assertRaises(ValueError):
                validate(plan, records)

    def test_wrong_sdk_stale_commit_and_claimed_coverage_fail(self):
        plan, record = self.fixture()
        for field, value in [('commit', 'old'), ('suite_fingerprint', 'old'), ('lockfile_sha256', 'other'), ('coverage', 'abi-link'), ('rustc', ''), ('runtime_identity', ''), ('dirty', True)]:
            with self.subTest(field=field), self.assertRaises(ValueError):
                validate(plan, [{**record, field: value}])
        bad = deepcopy(record)
        bad['runtime']['archive_sha256'] = 'b' * 64
        with self.assertRaisesRegex(ValueError, 'archive differs'):
            validate(plan, [bad])

    def test_omitted_abi_or_native_regression_cannot_pass(self):
        plan, record = self.fixture()
        for removed in ('abi-probe', 'file-abi-probe', 'discovery-abi-probe', 'runtime-identity', 'sdk-regressions', 'sdk-api-coverage', 'native-file-negative'):
            bad = deepcopy(record)
            bad['commands'] = [c for c in bad['commands'] if c['label'] != removed]
            with self.subTest(removed=removed), self.assertRaises(ValueError):
                validate(plan, [bad])

    def test_native_regression_gate_requires_every_exact_executed_test(self):
        plan, record = self.fixture()
        mutations = [
            lambda e: e['tests'].pop(),
            lambda e: e['tests'].append(deepcopy(e['tests'][0])),
            lambda e: e['tests'][0].update(test='unreviewed_test'),
            lambda e: e['tests'][0].update(passed=0, ignored=1),
            lambda e: e['tests'][0].update(passed=True),
            lambda e: e['tests'][0].update(status='skipped'),
            lambda e: e['tests'][0].update(log_sha256=''),
            lambda e: e['tests'][0].update(binary_sha256=''),
            lambda e: e.update(confinement='macos-sandbox'),
            lambda e: e.update(test_count=e['test_count'] - 1),
            lambda e: e.update(suite_count=True),
        ]
        for index, mutate in enumerate(mutations):
            bad = deepcopy(record)
            mutate(bad['sdk_regressions'])
            with self.subTest(mutation=index), self.assertRaisesRegex(ValueError, 'native regression'):
                validate(plan, [bad])
        del record['sdk_regressions']
        with self.assertRaisesRegex(ValueError, 'native regression'):
            validate(plan, [record])

    def test_native_regression_applicability_is_explicit_for_each_host(self):
        for profile in ('macos-arm64', 'windows-x64'):
            with self.subTest(profile=profile):
                plan, record = self.fixture(profile)
                evidence = record['sdk_regressions']
                self.assertEqual((evidence['test_count'], evidence['suite_count']), (9, 4))
                self.assertEqual(validate(plan, [record])['cells_passed'], 1)
                extra = deepcopy(record)
                extra['sdk_regressions']['tests'].append({**evidence['tests'][0],
                    'suite': 'video_padding_sdk', 'test': REGRESSION_TESTS['video_padding_sdk'][0]})
                with self.assertRaisesRegex(ValueError, 'native regression'):
                    validate(plan, [extra])
                evidence['tests'].pop()
                with self.assertRaisesRegex(ValueError, 'native regression'):
                    validate(plan, [record])

    def test_cross_toolchain_must_match_frozen_plan(self):
        plan, record = self.fixture()
        record.pop('native_suites')
        cell = next(c for c in make_plan(profiles()) if c['requires_toolchain'])
        plan['cells'] = [cell]
        plan['packages'] = {cell['header_package']: {'sha256': 'a' * 64, 'artifact_id': 'fixture-artifact'}}
        receipts = [{'package': 'fixture-compiler', 'version': '1.0', 'architecture': 'amd64', 'sha256': 'f' * 64}]
        plan['toolchains'] = {cell['profile']: {'provider': 'public-gnu', 'recipe_sha256': 'b' * 64, 'packages': receipts}}
        record.update(cell=cell, coverage=cell['coverage'],
            headers={'package': cell['header_package'], 'archive_sha256': 'a' * 64, 'artifact_id': 'fixture-artifact', 'inventory_sha256': 'e' * 64},
            runtime={'package': cell['runtime_package'], 'archive_sha256': 'a' * 64, 'artifact_id': 'fixture-artifact', 'inventory_sha256': 'e' * 64},
            commands=[{'label': label, 'exit_code': 0} for label in required_steps(cell)])
        with self.assertRaisesRegex(ValueError, 'target toolchain'):
            validate(plan, [record])
        record.update(toolchain_provider='public-gnu', toolchain_recipe_sha256='b' * 64,
                      toolchain_packages=receipts, toolchain_config_sha256='c' * 64)
        self.assertEqual(validate(plan, [record])['cells_passed'], 1)
        record['toolchain_packages'] = []
        with self.assertRaisesRegex(ValueError, 'target toolchain'):
            validate(plan, [record])

    def test_mobile_library_build_cannot_replace_final_app_or_runtime_proof(self):
        plan, record = self.fixture()
        record.pop('native_suites')
        cell = next(c for c in make_plan(profiles())
                    if c['profile'] == 'tvos-x86_64-sim' and c['edition'] == 'standard')
        plan['cells'] = [cell]
        plan['packages'] = {cell['header_package']: {'sha256': 'a' * 64, 'artifact_id': 'fixture-artifact'}}
        record.update(cell=cell, coverage='simulator',
            headers={'package': cell['header_package'], 'archive_sha256': 'a' * 64,
                     'artifact_id': 'fixture-artifact', 'inventory_sha256': 'e' * 64},
            runtime={'package': cell['runtime_package'], 'archive_sha256': 'a' * 64,
                     'artifact_id': 'fixture-artifact', 'inventory_sha256': 'e' * 64},
            mobile={'coverage': 'simulator', 'binary_sha256': 'f' * 64},
            commands=[{'label': label, 'exit_code': 0} for label in (
                'rustc-version', 'build-library', 'abi-probe', 'discovery-abi-probe',
                'file-abi-probe', 'mobile-package', 'mobile-runtime')])
        self.assertEqual(validate(plan, [record])['cells_passed'], 1)
        for omitted in ('build-library', 'abi-probe', 'mobile-package', 'mobile-runtime'):
            mutated = deepcopy(record)
            mutated['commands'] = [c for c in mutated['commands'] if c['label'] != omitted]
            # Desktop examples cannot stand in for either mobile build stage.
            mutated['commands'].append({'label': 'link-examples', 'exit_code': 0})
            with self.subTest(omitted=omitted), self.assertRaisesRegex(ValueError, 'required commands'):
                validate(plan, [mutated])
        del record['mobile']
        with self.assertRaisesRegex(ValueError, 'mobile package or execution'):
            validate(plan, [record])

    def test_local_overrides_are_not_release_evidence(self):
        plan, record = self.fixture()
        record['headers']['local_override'] = True
        with self.assertRaises(ValueError):
            validate(plan, [record])


class MobilePolicyTests(unittest.TestCase):
    def test_wrong_or_unavailable_simulator_cannot_substitute(self):
        devices = {'com.apple.CoreSimulator.SimRuntime.tvOS-18-0': [
            {'name': 'Apple TV', 'udid': 'tvos', 'isAvailable': True}]}
        with self.assertRaisesRegex(ValueError, 'no available'):
            choose_simulator(devices, 'ios')
        self.assertEqual(choose_simulator(devices, 'tvos')[1]['udid'], 'tvos')

    def test_app_cannot_resolve_new_dependency_versions(self):
        with tempfile.TemporaryDirectory() as directory:
            old, new = Path(directory) / 'old.lock', Path(directory) / 'new.lock'
            old.write_text('[[package]]\nname="dependency"\nversion="1.0.0"\n')
            new.write_text(old.read_text() + '[[package]]\nname="ndi-ci-smoke"\nversion="0.0.0"\n')
            check_app_resolution(old, new)
            new.write_text('[[package]]\nname="dependency"\nversion="2.0.0"\n')
            with self.assertRaisesRegex(ValueError, 'outside the frozen'):
                check_app_resolution(old, new)


class ExtractionTests(unittest.TestCase):
    def archive(self, name, link=None):
        content = io.BytesIO()
        with tarfile.open(fileobj=content, mode='w') as stream:
            item = tarfile.TarInfo(name)
            if link:
                item.type = tarfile.SYMTYPE
                item.linkname = link
                stream.addfile(item)
            else:
                item.size = 2
                stream.addfile(item, io.BytesIO(b'ok'))
        content.seek(0)
        return content

    def test_regular_payload_extracts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            extract_tar(self.archive('sdk/include/test.h'), root)
            self.assertEqual((root / 'sdk/include/test.h').read_bytes(), b'ok')

    def test_parent_traversal_and_external_symlink_are_rejected(self):
        for archive in (self.archive('../outside'), self.archive('sdk/link', '/tmp/outside')):
            with tempfile.TemporaryDirectory() as directory, self.assertRaises((ValueError, tarfile.TarError)):
                extract_tar(archive, Path(directory))

    def test_unverified_catalog_entry_fails_before_acquisition(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, 'verified SHA256'):
                provision('unverified-fixture', Path(directory), 'https://example.invalid/unused', catalog={
                    'unverified-fixture': {'version': '6.3.2', 'edition': 'standard', 'platform': 'linux',
                    'archive_format': 'embedded-tar', 'version_evidence': 'version-txt', 'sha256': None}})


if __name__ == '__main__':
    unittest.main()
