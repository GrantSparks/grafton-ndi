"""Native execution requires complete per-operation boundary and cleanup proof."""
from contextlib import redirect_stdout
from copy import deepcopy
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from aggregate import validate_cells
from common import digest
from common import FEATURE_SETS, native_configuration, native_modes, required_steps
from evidence_fixture import confinement_receipt, evidence_fixture
from macos_native import RECEIPT_CHECKS
from run import CellRun
from runtime_identity import parse_identity


class NativeAggregateTests(unittest.TestCase):
    def fixture(self, edition='advanced', suite='native'):
        plan, records, _ = evidence_fixture()
        record = next(record for record in records if record['cell']['profile'] == 'macos-arm64'
                      and record['cell']['edition'] == edition and record['cell']['suite'] == suite)
        plan['cells'] = [record['cell']]
        return plan, record

    def test_all_native_modes_require_complete_executed_results(self):
        for edition in ('standard', 'advanced'):
            plan, record = self.fixture(edition)
            self.assertEqual(validate_cells(plan, [record])['cells_passed'], 1)
            mutations = [lambda r: r.pop('native_suites'),
                         lambda r: r['native_suites'].pop(next(iter(r['native_suites']))),
                         lambda r: r['native_suites'].update(unreviewed={})]
            for mode in record['native_suites']:
                mutations += [lambda r, m=mode: r['native_suites'][m]['tests'].pop(),
                              lambda r, m=mode: r['native_suites'][m]['tests'][0].update(ignored=1, passed=0),
                              lambda r, m=mode: r['native_suites'][m]['tests'][0].update(status='skipped')]
            for mutate in mutations:
                changed = deepcopy(record); mutate(changed)
                with self.subTest(edition=edition), self.assertRaises(ValueError):
                    validate_cells(plan, [changed])

    def test_every_test_listing_and_runtime_probe_needs_its_own_bound_receipt(self):
        plan, record = self.fixture()
        paths = [('runtime_confinement',)]
        paths += [('sdk_regressions', 'tests', index, 'confinement_evidence')
                  for index in range(len(record['sdk_regressions']['tests']))]
        paths += [('sdk_regressions', 'listings', index, 'confinement_evidence')
                  for index in range(len(record['sdk_regressions']['listings']))]
        paths += [('native_suites', mode, 'tests', index, 'confinement_evidence')
                  for mode, report in record['native_suites'].items() for index in range(len(report['tests']))]
        paths += [('native_suites', mode, 'listings', index, 'confinement_evidence')
                  for mode, report in record['native_suites'].items() for index in range(len(report.get('listings', [])))]
        for path in paths:
            changed = deepcopy(record)
            owner = changed
            for key in path[:-1]:
                owner = owner[key]
            del owner[path[-1]]
            with self.subTest(path=path), self.assertRaises(ValueError):
                validate_cells(plan, [changed])

    def test_closed_receipt_rejects_partial_cleanup_unknown_data_and_wrong_identity(self):
        plan, record = self.fixture()
        changes = [lambda p, key=key: p['checks'].update({key: False}) for key in RECEIPT_CHECKS]
        changes += [lambda p: p['checks'].update(worker_reaped=1),
            lambda p: p['checks'].pop('fresh_https_recovered'),
            lambda p: p['checks'].update(unreviewed=True),
            lambda p: p.update(details_path='/private/host-detail'),
            lambda p: p.update(operation='different-test'),
            lambda p: p.update(binary_sha256='0' * 64),
            lambda p: p.update(schema=True), lambda p: p.update(status='skipped'),
            lambda p: p.update(worker_limit_seconds=31),
            lambda p: p.update(offline_limit_seconds=46),
            lambda p: p.update(offline_elapsed_seconds=45.1),
            lambda p: p.update(offline_elapsed_seconds=float('nan')),
            lambda p: p.update(offline_elapsed_seconds=True),
            lambda p: p.update(block_packets_delta=0),
            lambda p: p.update(block_packets_delta=True),
            lambda p: p.update(external_ipv6='passed'),
            lambda p: p.update(details_sha256='missing')]
        for mutate in changes:
            changed = deepcopy(record)
            mutate(changed['sdk_regressions']['tests'][0]['confinement_evidence'])
            with self.assertRaisesRegex(ValueError, 'macOS confinement'):
                validate_cells(plan, [changed])

    def test_mac_runtime_identity_is_confined_in_native_and_compat_cells(self):
        for suite in ('native', 'compat'):
            plan, record = self.fixture('standard', suite)
            for field in ('runtime_confinement', 'runtime_probe_sha256'):
                changed = deepcopy(record); changed.pop(field)
                with self.subTest(suite=suite, field=field), self.assertRaises(ValueError):
                    validate_cells(plan, [changed])
            changed = deepcopy(record)
            changed['runtime_confinement']['operation'] = record['sdk_regressions']['tests'][0]['test'] if suite == 'native' else 'list'
            with self.assertRaisesRegex(ValueError, 'macOS confinement'):
                validate_cells(plan, [changed])

    def test_each_listing_matches_its_actual_suite_binary(self):
        plan, record = self.fixture()
        for mutate in (
            lambda r: r['sdk_regressions'].pop('listings'),
            lambda r: r['sdk_regressions']['listings'].pop(),
            lambda r: r['sdk_regressions']['listings'].append(deepcopy(r['sdk_regressions']['listings'][0])),
            lambda r: r['sdk_regressions']['listings'][0].update(suite='unreviewed'),
            lambda r: r['sdk_regressions']['listings'][0].update(binary_sha256='0' * 64),
            lambda r: r['sdk_regressions'].update(confinement='macos-sandbox'),
        ):
            changed = deepcopy(record); mutate(changed)
            with self.assertRaises(ValueError):
                validate_cells(plan, [changed])


class RuntimeProbeAdapterTests(unittest.TestCase):
    def fixture(self, directory):
        plan, records, _ = evidence_fixture()
        cell = next(record['cell'] for record in records if record['cell']['profile'] == 'macos-arm64'
                    and record['cell']['suite'] == 'native')
        probe = directory / 'runtime-probe'; probe.write_text('SDK-free executable fixture')
        runner = CellRun(cell, directory / 'output')
        runner.env['NDI_SDK_ARCHIVE_TOKEN'] = 'private-credential-fixture'
        return runner, probe

    def test_mac_probe_exports_only_structured_identity_and_preserves_private_log(self):
        with tempfile.TemporaryDirectory() as name:
            runner, probe = self.fixture(Path(name))
            edition = runner.cell['edition']
            payload = 'private runtime diagnostic payload'
            completed = subprocess.CompletedProcess([str(probe)], 0,
                stdout=payload + '\nNDI_RUNTIME_IDENTITY=1|6.4.0.2|' + edition + '|true\n',
                stderr='private runtime stderr\n')
            completed.confinement_evidence = confinement_receipt('runtime-identity', digest(probe))
            with patch('macos_native.execute', return_value=completed) as executor:
                runner.runtime_probe(probe, macos=True)
            args, kwargs = executor.call_args
            self.assertEqual(args[:2], (probe, []))
            self.assertEqual(kwargs, {'operation': 'runtime-identity'})
            self.assertNotIn('NDI_SDK_ARCHIVE_TOKEN', args[3])
            self.assertEqual(runner.result['runtime_identity'], {
                'schema': 1, 'version': [6, 4, 0, 2], 'edition': edition, 'edition_verified': True})
            self.assertNotIn(payload, json.dumps(runner.result))
            self.assertNotIn(completed.stderr.strip(), json.dumps(runner.result))
            self.assertEqual((runner.output / 'runtime-identity.log').read_text(),
                             completed.stdout + completed.stderr)
            self.assertEqual(runner.result['runtime_probe_sha256'], digest(probe))
            self.assertEqual(runner.result['runtime_confinement'], completed.confinement_evidence)
            self.assertEqual(runner.commands[0]['label'], 'runtime-identity')
            self.assertEqual(runner.commands[0]['log_sha256'], digest(runner.output / 'runtime-identity.log'))

    def test_failed_mac_probe_retains_private_output_without_exporting_payload(self):
        with tempfile.TemporaryDirectory() as name:
            runner, probe = self.fixture(Path(name))
            payload = 'private runtime diagnostic payload'
            completed = subprocess.CompletedProcess([str(probe)], 125, stdout=payload, stderr='')
            completed.confinement_evidence = None
            public = io.StringIO()
            with patch('macos_native.execute', return_value=completed), redirect_stdout(public), self.assertRaises(ValueError):
                runner.runtime_probe(probe, macos=True)
            self.assertNotIn(payload, public.getvalue())
            self.assertEqual((runner.output / 'runtime-identity.log').read_text(), payload)
            self.assertEqual(runner.commands[0]['exit_code'], 125)
            self.assertIsNone(runner.result['runtime_confinement'])

    def test_linux_and_windows_probes_use_existing_native_isolation(self):
        for platform, kind in (('linux', 'linux-network-namespace'), ('win32', 'windows-firewall')):
            with self.subTest(platform=platform), tempfile.TemporaryDirectory() as name:
                runner, probe = self.fixture(Path(name))
                completed = subprocess.CompletedProcess([str(probe)], 0,
                    stdout='NDI_RUNTIME_IDENTITY=1|6.4.0.2|' + runner.cell['edition'] + '|true\n', stderr='')
                with patch.object(sys, 'platform', platform), patch('native.isolate', return_value=completed) as isolated:
                    runner.runtime_probe(probe, macos=False)
                self.assertEqual(isolated.call_args.args[:2], (probe, []))
                self.assertNotIn('NDI_SDK_ARCHIVE_TOKEN', isolated.call_args.args[3])
                self.assertEqual(runner.result['runtime_confinement_kind'], kind)
                self.assertEqual(runner.result['runtime_probe_sha256'], digest(probe))

    def test_malformed_successful_probe_fails_without_exporting_output(self):
        with tempfile.TemporaryDirectory() as name:
            runner, probe = self.fixture(Path(name))
            payload = 'NDI_RUNTIME_IDENTITY=private-runtime-payload\n'
            completed = subprocess.CompletedProcess([str(probe)], 0, stdout=payload, stderr='')
            completed.confinement_evidence = confinement_receipt('runtime-identity', digest(probe))
            with patch('macos_native.execute', return_value=completed), self.assertRaises(ValueError) as failure:
                runner.runtime_probe(probe, macos=True)
            self.assertNotIn('private-runtime-payload', str(failure.exception))
            self.assertNotIn('runtime_identity', runner.result)
            self.assertEqual((runner.output / 'runtime-identity.log').read_text(), payload)


class RuntimeIdentityTests(unittest.TestCase):
    def test_closed_identity_ignores_private_output_and_preserves_verification_fact(self):
        for verified in (True, False):
            output = ('private SDK banner\nNDI_RUNTIME_IDENTITY=1|6.3.2.0|standard|'
                      + str(verified).lower() + '\nprivate SDK stderr\n')
            self.assertEqual(parse_identity(output, edition='standard'), {
                'schema': 1, 'version': [6, 3, 2, 0], 'edition': 'standard', 'edition_verified': verified})

    def test_missing_duplicate_malformed_or_contradictory_markers_fail_closed(self):
        valid = 'NDI_RUNTIME_IDENTITY=1|6.4.0.2|standard|true'
        for output in ('private text', valid + '\n' + valid,
                       valid + '\n' + valid.replace('standard', 'advanced'),
                       'private prefix ' + valid, valid + ' private suffix',
                       valid.replace('6.4.0.2', '6.4.0'),
                       valid.replace('6.4.0.2', '6.04.0.2'),
                       valid.replace('6.4.0.2', '4294967296.4.0.2'),
                       valid.replace('true', 'private-payload'),
                       valid.replace('standard', 'advanced')):
            with self.subTest(output=output), self.assertRaises(ValueError) as failure:
                parse_identity(output, edition='standard')
            self.assertNotIn('private-payload', str(failure.exception))

    def test_aggregate_rejects_raw_or_extended_runtime_identity(self):
        plan, records, _ = evidence_fixture()
        record = next(r for r in records if r['cell']['suite'] == 'native')
        plan['cells'] = [record['cell']]
        variants = ['raw SDK output', None]
        for field, value in (('schema', True), ('version', [6, 4, True, 2]),
                             ('version', [6, 4, 0, 4294967296]),
                             ('edition', 'private-payload'), ('edition_verified', 1),
                             ('raw_version', 'private SDK text')):
            identity = deepcopy(record['runtime_identity']); identity[field] = value
            variants.append(identity)
        variants.append({**record['runtime_identity'], 'edition':
                         'advanced' if record['cell']['edition'] == 'standard' else 'standard'})
        for identity in variants:
            changed = deepcopy(record); changed['runtime_identity'] = identity
            with self.subTest(identity=identity), self.assertRaises(ValueError):
                validate_cells(plan, [changed])


class CoreCoverageTests(unittest.TestCase):
    def test_native_compat_and_feature_cells_require_each_core_configuration(self):
        plan, records, _ = evidence_fixture()
        for record in records:
            cell = record['cell']
            if cell['suite'] not in ('native', 'compat', 'features'):
                continue
            labels = [label for label in native_modes(cell) if label.startswith('core')]
            self.assertEqual(labels, ['core-' + str(i) for i in range(len(FEATURE_SETS))]
                             if cell['suite'] == 'features' else ['core'])
            subplan = {**plan, 'cells': [cell]}
            self.assertEqual(validate_cells(subplan, [record])['cells_passed'], 1)
            for label in labels:
                mode, selection = native_configuration(label)
                self.assertIn('native-' + label, required_steps(cell))
                report = record['native_suites'][label]
                self.assertEqual((report['mode'], report['feature_set'], report['test_count'], report['suite_count']),
                                 (mode, selection, 39, 6))
                for mutate in (lambda r: r['native_suites'].pop(label),
                               lambda r: r['native_suites'][label]['tests'].pop(),
                               lambda r: r['native_suites'][label].update(feature_set='other'),
                               lambda r: r['native_suites'][label]['tests'][0].update(ignored=1)):
                    changed = deepcopy(record); mutate(changed)
                    with self.subTest(cell=cell['id'], label=label), self.assertRaises(ValueError):
                        validate_cells(subplan, [changed])

    def test_core_cli_label_and_feature_binding_is_preserved_in_cell_report(self):
        with tempfile.TemporaryDirectory() as temporary:
            plan, records, _ = evidence_fixture()
            cell = next(record['cell'] for record in records if record['cell']['suite'] == 'features')
            runner = CellRun(cell, Path(temporary) / 'output')
            def command(label, args):
                output = Path(args[args.index('--output') + 1]); output.mkdir()
                (output / 'native.json').write_text(json.dumps({'marker': label}))
            with patch.object(runner, 'command', side_effect=command) as invoked:
                for label in native_modes(cell):
                    runner.native_suite(label)
            for call, label in zip(invoked.call_args_list, native_modes(cell)):
                self.assertEqual(call.args[0], 'native-' + label)
                args = call.args[1]
                self.assertEqual(args[args.index('--mode') + 1], 'core')
                self.assertEqual(args[args.index('--feature-set') + 1], native_configuration(label)[1])
            self.assertEqual(set(runner.result['native_suites']), set(native_modes(cell)))


class PureTestRuntimeGuardTests(unittest.TestCase):
    def test_ordinary_command_gets_fresh_empty_runtime_without_changing_native_environment(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            plan, records, _ = evidence_fixture()
            runner = CellRun(records[0]['cell'], directory / 'output')
            selected = directory / 'selected-runtime'; selected.mkdir()
            runner.env.update(NDI_RUNTIME_DIR_V6=str(selected), NDI_EXPECTED_RUNTIME=str(selected / 'runtime'),
                              NDI_SDK_ARCHIVE_TOKEN='private-token')
            observed = []
            def child(argv, **kwargs):
                env = kwargs['env']
                unavailable = Path(env['NDI_RUNTIME_DIR_V6'])
                self.assertNotEqual(unavailable, selected)
                self.assertTrue(unavailable.is_dir())
                self.assertEqual(list(unavailable.iterdir()), [])
                self.assertNotIn('NDI_SDK_ARCHIVE_TOKEN', env)
                self.assertEqual(env['NDI_EXPECTED_RUNTIME'], str(selected / 'runtime'))
                observed.append(unavailable)
                return subprocess.CompletedProcess(argv, 0)
            with patch('run.subprocess.run', side_effect=child):
                runner.pure_tests('tests', ['cargo', 'test', '--all-features'])
                runner.pure_tests('features-0', ['cargo', 'test', '--no-default-features'])
            self.assertEqual(len(set(observed)), 2)
            self.assertEqual(runner.env['NDI_RUNTIME_DIR_V6'], str(selected))
            self.assertEqual([command['label'] for command in runner.commands], ['tests', 'features-0'])

    def test_reused_guard_directory_and_failed_child_cannot_mutate_selected_runtime(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            plan, records, _ = evidence_fixture()
            runner = CellRun(records[0]['cell'], directory / 'output')
            runner.env['NDI_RUNTIME_DIR_V6'] = 'selected-runtime'
            with patch('run.subprocess.run', return_value=subprocess.CompletedProcess([], 1)), \
                    patch('run.summarize_log', return_value={'schema': 1, 'reasons': ['test-failed']}), \
                    redirect_stdout(io.StringIO()), self.assertRaises(ValueError):
                runner.pure_tests('tests', ['cargo', 'test'])
            self.assertEqual(runner.env['NDI_RUNTIME_DIR_V6'], 'selected-runtime')
            with patch('run.subprocess.run') as child, self.assertRaises(FileExistsError):
                runner.pure_tests('tests', ['cargo', 'test'])
            child.assert_not_called()


if __name__ == '__main__':
    unittest.main()
