"""Required media regressions must execute exactly once inside confinement."""
from pathlib import Path
import contextlib
import io
import json
import os
import re
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import native
import macos_native
from common import FEATURE_SETS, feature_flags
from run import CellRun
from sandbox_probe import TIMEOUT_SECONDS
from diagnostics import PublicSources, summarize_text


def result(stdout, code=0):
    return subprocess.CompletedProcess([], code, stdout, '')


def receipt(binary, operation):
    return {'schema': 1, 'status': 'passed', 'operation': operation,
        'binary_sha256': native.digest(binary), 'worker_limit_seconds': 30,
        'offline_limit_seconds': 45, 'offline_elapsed_seconds': 3.5,
        'block_packets_delta': 2, 'external_ipv6': 'not-probed',
        'checks': {key: True for key in macos_native.RECEIPT_CHECKS}, 'details_sha256': 'd' * 64}


def artifact(root, suite):
    binary = root / 'debug/deps' / suite
    binary.parent.mkdir(parents=True, exist_ok=True)
    binary.write_bytes(b'executable fixture')
    return {'reason': 'compiler-artifact', 'profile': {'test': True},
            'target': {'name': suite, 'kind': ['test']}, 'executable': str(binary)}


class NativeRegressionTests(unittest.TestCase):
    def test_actual_utf8_and_malformed_diagnostic_bytes_never_drop_captured_streams(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            child = directory / 'child.py'
            payload = 'PRIVATE_DIAGNOSTIC \u2717 \u274c\n'.encode('utf-8') + b'bad-byte:\xff\n'
            summary = b'test result: ok. 1 passed; 0 failed; 0 ignored;\n'
            child.write_text('import os, sys\nos.write(1, ' + repr(payload + summary)
                + ')\nos.write(2, b"PRIVATE_STDERR:\\xff\\n")\nsys.exit(int(sys.argv[1]))\n', encoding='utf-8')
            for exit_code in (0, 7):
                completed = native.capture([sys.executable, str(child), str(exit_code)], timeout=10)
                self.assertIsInstance(completed.stdout, str)
                self.assertIsInstance(completed.stderr, str)
                self.assertIn('\u2717 \u274c', completed.stdout)
                self.assertIn(r'bad-byte:\xff', completed.stdout)
                self.assertIn(r'PRIVATE_STDERR:\xff', completed.stderr)
                self.assertNotIn('Exception in thread', completed.stderr)
                self.assertEqual(completed.returncode, exit_code)
                log = directory / f'child-{exit_code}.log'
                native.write_test_log(log, completed, 'owned_pipeline_returns_original_buffers_and_metadata')
                self.assertEqual(log.read_text(encoding='utf-8'), completed.stdout + completed.stderr)
                if exit_code == 0:
                    native.require_one_pass(completed, 'fixture')
                else:
                    with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()), \
                            self.assertRaises(ValueError):
                        native.require_one_pass(completed, 'fixture')

    def test_log_write_missing_stream_is_other_failure_and_interrupts_propagate(self):
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / 'child.log'
            name = 'owned_pipeline_returns_original_buffers_and_metadata'
            missing = subprocess.CompletedProcess([], 0, None, '')
            with contextlib.redirect_stdout(io.StringIO()) as captured, self.assertRaises(TypeError):
                native.write_test_log(log, missing, name)
            summary = summarize_text(captured.getvalue(), PublicSources(native.ROOT))
            self.assertEqual(summary['native_context'], [{'phase': 'log-write', 'test': name, 'kind': 'other'}])
            self.assertEqual(summary['failed_tests'], [])
            with patch.object(Path, 'write_text', side_effect=KeyboardInterrupt), \
                    contextlib.redirect_stdout(io.StringIO()) as captured, self.assertRaises(KeyboardInterrupt):
                native.write_test_log(log, result('fixture'), name)
            self.assertEqual(captured.getvalue(), '')

    def test_private_failed_output_can_be_forwarded_from_a_legacy_encoded_stream(self):
        sink = io.BytesIO()
        stream = io.TextIOWrapper(sink, encoding='cp1252')
        try:
            native.private_output(stream, '\u2717 \u274c literal \\xff')
            stream.flush()
            self.assertEqual(sink.getvalue().decode('utf-8'), '\u2717 \u274c literal \\xff\n')
        finally:
            stream.close()

    def test_log_write_oserror_preserves_context_without_claiming_test_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'PRIVATE_MISSING_DIRECTORY' / 'private.log'
            name = 'owned_pipeline_returns_original_buffers_and_metadata'
            with contextlib.redirect_stdout(io.StringIO()) as captured, self.assertRaises(FileNotFoundError):
                native.write_test_log(path, result('PRIVATE_SDK_PAYLOAD'), name)
            summary = summarize_text(captured.getvalue(), PublicSources(native.ROOT))
            self.assertEqual(summary['native_context'], [{'phase': 'log-write', 'test': name, 'kind': 'io'}])
            self.assertEqual(summary['failed_tests'], [])
            self.assertEqual(summary['reasons'], ['native-log-write-failed'])
            self.assertNotIn('PRIVATE', captured.getvalue())
            self.assertNotIn(str(path), json.dumps(summary))

    def test_log_write_unicode_error_retains_only_closed_encoding_context(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'private.log'
            name = 'owned_pipeline_returns_original_buffers_and_metadata'
            write_text = Path.write_text
            def ascii_default(path, text, **kwargs):
                self.assertEqual(kwargs, {'encoding': 'utf-8'})
                return write_text(path, text, encoding='ascii')
            with patch.object(Path, 'write_text', ascii_default), \
                    contextlib.redirect_stdout(io.StringIO()) as captured, self.assertRaises(UnicodeEncodeError):
                native.write_test_log(path, result('PRIVATE_SDK_PAYLOAD\u2713'), name)
            summary = summarize_text(captured.getvalue(), PublicSources(native.ROOT))
            self.assertEqual(summary['native_context'], [{'phase': 'log-write', 'test': name, 'kind': 'encoding'}])
            self.assertEqual(summary['failed_tests'], [])
            self.assertNotIn('PRIVATE', captured.getvalue())
            self.assertNotIn('\u2713', json.dumps(summary, ensure_ascii=False))

    def test_policy_covers_every_ignored_test_in_the_selected_sources(self):
        self.assertEqual(len(native.REGRESSION_TESTS), 5)
        self.assertEqual(sum(map(len, native.REGRESSION_TESTS.values())), 10)
        for suite, names in native.REGRESSION_TESTS.items():
            source = (native.ROOT / 'tests' / (suite + '.rs')).read_text()
            observed = re.findall(r'#\[ignore[^\]]*\]\s*fn\s+(\w+)\s*\(', source)
            with self.subTest(suite=suite):
                self.assertEqual(set(names), set(observed))
                self.assertEqual(len(names), len(observed))
                self.assertEqual(re.findall(r'^#!\[cfg\((.*)\)\]', source, re.M),
                                 ['target_os = "linux"'] if suite == 'video_padding_sdk' else [])

    def test_platform_policy_requires_every_applicable_suite(self):
        self.assertEqual(native.regression_tests('linux'), native.REGRESSION_TESTS)
        portable = {suite: names for suite, names in native.REGRESSION_TESTS.items()
                    if suite != 'video_padding_sdk'}
        for platform in ('darwin', 'win32'):
            self.assertEqual(native.regression_tests(platform), portable)
            self.assertEqual(sum(map(len, native.regression_tests(platform).values())), 9)
        with self.assertRaises(ValueError):
            native.regression_tests('unreviewed-host')

    def test_zero_skipped_duplicate_or_failed_execution_cannot_pass(self):
        native.require_one_pass(result('test result: ok. 1 passed; 0 failed; 0 ignored;'), 'fixture')
        for text, code in [
            ('test result: ok. 0 passed; 0 failed; 0 ignored;', 0),
            ('test result: ok. 1 passed; 0 failed; 1 ignored;', 0),
            ('test result: ok. 2 passed; 0 failed; 0 ignored;', 0),
            ('test result: ok. 1 passed; 0 failed; 0 ignored;\n' * 2, 0),
            ('test result: ok. 1 passed; 0 failed; 0 ignored;', 1),
            ('test result: ok. 1 passed; 0 failed; 0 ignored;', -9),
        ]:
            with self.subTest(text=text, code=code), contextlib.redirect_stdout(io.StringIO()), \
                    self.assertRaises(ValueError):
                native.require_one_pass(result(text, code), 'fixture')

    def test_test_executable_must_be_unique_present_and_target_scoped(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / 'target/triple'
            record = artifact(root, 'suite')
            self.assertEqual(native.test_executables([record], {'suite'}, root),
                             {'suite': Path(record['executable'])})
            for records in ([], [record, record], [artifact(root, 'unexpected')],
                            [artifact(root.parent, 'suite')]):
                with self.subTest(records=records), self.assertRaises(ValueError):
                    native.test_executables(records, {'suite'}, root)
            Path(record['executable']).unlink()
            with self.assertRaises(ValueError):
                native.test_executables([record], {'suite'}, root)

    def test_compiled_ignored_inventory_cannot_omit_or_add_a_test(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for text in ('one: test\n', 'one: test\ntwo: test\nextra: test\n',
                         'one: test\ntwo: test\ntwo: test\n'):
                with patch.object(native, 'isolate', return_value=result(text)) as confined:
                    with contextlib.redirect_stdout(io.StringIO()), self.assertRaises(ValueError):
                        native.verify_ignored_tests(root / 'binary', ('one', 'two'), root, {}, root / 'list.log')
                    self.assertEqual(confined.call_args.args[1], ['--list', '--ignored', '--format', 'terse'])

    def test_real_failed_child_exposes_only_bounded_public_diagnostics(self):
        with tempfile.TemporaryDirectory(prefix='ndi-native-failure-') as temporary:
            root = Path(temporary)
            stdout_secret, stderr_secret = 'PRIVATE_NATIVE_STDOUT', 'PRIVATE_NATIVE_STDERR'
            failed = root / 'failed.py'
            failed.write_text('import sys\nprint(' + repr(stdout_secret) + ')\n'
                'print(' + repr("thread 'PRIVATE_THREAD' panicked at tests/async_send_lifecycle.rs:18:5:")
                + ', file=sys.stderr)\nprint(' + repr(stderr_secret) + ', file=sys.stderr)\nsys.exit(17)\n')
            child = root / 'native-child.py'
            child.write_text('import subprocess, sys\n'
                'sys.path.insert(0, ' + repr(str(Path(native.__file__).parent)) + ')\n'
                'from native import require_one_pass\n'
                'result = subprocess.run([sys.executable, ' + repr(str(failed))
                + '], text=True, capture_output=True)\n'
                'require_one_pass(result, "owned_pipeline_returns_original_buffers_and_metadata")\n')
            cell = CellRun.__new__(CellRun)
            cell.output = root / 'cell'; cell.output.mkdir()
            cell.env = os.environ.copy()
            cell.commands = []
            published = io.StringIO()
            with contextlib.redirect_stdout(published), self.assertRaises(ValueError):
                cell.command('sdk-regressions', [sys.executable, child])
            diagnostic = cell.commands[-1]['diagnostics']
            self.assertIn('rust-panic', diagnostic['reasons'])
            self.assertIn({'file': 'tests/async_send_lifecycle.rs', 'line': 18, 'column': 5},
                          diagnostic['project_locations'])
            self.assertIn('owned_pipeline_returns_original_buffers_and_metadata', diagnostic['failed_tests'])
            private_log = (cell.output / 'sdk-regressions.log').read_text()
            for secret in (stdout_secret, stderr_secret, 'PRIVATE_THREAD', str(failed)):
                self.assertNotIn(secret, published.getvalue())
                self.assertNotIn(secret, json.dumps(cell.commands))
            self.assertIn(stdout_secret, private_log)
            self.assertIn(stderr_secret, private_log)

    def test_every_regression_executes_through_shared_confinement_and_is_receipted(self):
        for platform, count, suite_count, confinement in (
            ('linux', 10, 5, 'linux-network-namespace'),
            ('darwin', 9, 4, 'macos-pf-offline'),
            ('win32', 9, 4, 'windows-firewall'),
        ):
            with self.subTest(platform=platform):
                self.check_regression_execution(platform, count, suite_count, confinement)

    def test_mac_diagnostic_uses_same_policy_and_cannot_waive_original_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            report = {'schema': 1, 'diagnostic_only': True, 'platform': 'darwin',
                      'timeout_seconds': TIMEOUT_SECONDS, 'operations': [
                          {'operation': 'unix.socketpair.create', 'status': 'failed', 'errno': 1}]}
            with patch.object(sys, 'platform', 'darwin'), \
                    patch.object(macos_native, 'execute', return_value=result(json.dumps(report))) as command, \
                    contextlib.redirect_stdout(io.StringIO()) as captured:
                native.diagnose_sandbox(root, {'SAFE_ENV': 'value'}, root)
            self.assertEqual(command.call_args.args[:2],
                (Path(sys.executable), [str(native.ROOT / 'ci/sandbox_probe.py')]))
            self.assertEqual(command.call_args.kwargs['operation'], 'sandbox-diagnostic')
            self.assertEqual(command.call_args.kwargs['timeout'], 30)
            self.assertEqual(json.loads(captured.getvalue().removeprefix('SANDBOX_PROBE=')), report)
            for failure in (subprocess.TimeoutExpired('PRIVATE_COMMAND', 30), ValueError('PRIVATE_ERROR')):
                with patch.object(sys, 'platform', 'darwin'), patch.object(native, 'isolate', side_effect=failure), \
                        contextlib.redirect_stdout(io.StringIO()) as captured:
                    native.diagnose_sandbox(root, {}, root)
                self.assertNotIn('PRIVATE', captured.getvalue())
                self.assertTrue(captured.getvalue().startswith('SANDBOX_PROBE_FAILURE='))

    def test_failed_mac_test_remains_failed_after_diagnostic(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); output = root / 'evidence'
            record = artifact(root / 'target/triple', 'advanced_runtime')
            args = ['native.py', '--mode', 'advanced', '--target', 'triple', '--output', str(output)]
            with patch.dict(os.environ, {'CARGO_TARGET_DIR': str(root / 'target'), 'NDI_EXPECTED_RUNTIME': 'fixture'}), \
                    patch.object(sys, 'argv', args), patch.object(sys, 'platform', 'darwin'), \
                    patch.object(native.subprocess, 'run', return_value=result(json.dumps(record))), \
                    patch.object(native, 'isolate', return_value=result('test result: FAILED', 1)), \
                    patch.object(native, 'diagnose_sandbox') as diagnostic, \
                    contextlib.redirect_stdout(io.StringIO()), self.assertRaisesRegex(ValueError, 'required native test failed'):
                native.main()
            diagnostic.assert_called_once()
            self.assertFalse((output / 'native.json').exists())

    def check_regression_execution(self, platform, count, suite_count, confinement):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); target = root / 'target/triple'; output = root / 'evidence'
            suites = native.regression_tests(platform)
            records = [artifact(target, suite) for suite in suites]
            calls = []
            def confined(binary, args, scratch, environment):
                calls.append((binary.name, args))
                self.assertEqual(environment['GRAFTON_NDI_ISOLATED_NETWORK'], '1')
                self.assertNotIn('NDI_SDK_ARCHIVE_TOKEN', environment)
                self.assertEqual(environment['NDI_CONFIG_DIR'], 'inherited-config')
                if args[0] == '--list':
                    completed = result(''.join(name + ': test\n' for name in suites[binary.name]))
                else:
                    self.assertEqual(args[1:], ['--exact', '--ignored', '--test-threads=1', '--nocapture'])
                    completed = result('test result: ok. 1 passed; 0 failed; 0 ignored;')
                if platform == 'darwin':
                    completed.confinement_evidence = receipt(binary, 'list' if args[0] == '--list' else args[0])
                return completed
            environment = {'CARGO_TARGET_DIR': str(root / 'target'), 'NDI_EXPECTED_RUNTIME': 'fixture-runtime',
                           'NDI_SDK_ARCHIVE_TOKEN': 'private-source-token', 'NDI_SDK_SOURCES_JSON': '',
                           'NDI_CONFIG_DIR': 'inherited-config'}
            arguments = ['native.py', '--mode', 'regressions', '--target', 'triple', '--output', str(output)]
            with patch.dict(os.environ, environment), patch.object(sys, 'argv', arguments), \
                    patch.object(sys, 'platform', platform), \
                    patch.object(native.subprocess, 'run', return_value=result('\n'.join(map(json.dumps, records)))) as build, \
                    patch.object(native, 'isolate', side_effect=confined), \
                    patch.object(native, 'diagnose_sandbox') as diagnostic:
                native.main()
            diagnostic.assert_not_called()
            build.assert_called_once()
            self.assertIn('--no-run', build.call_args.args[0])
            self.assertNotIn('NDI_SDK_ARCHIVE_TOKEN', build.call_args.kwargs['env'])
            self.assertEqual(build.call_args.kwargs['encoding'], 'utf-8')
            self.assertEqual(build.call_args.kwargs['errors'], 'backslashreplace')
            selected = build.call_args.args[0]
            self.assertEqual([selected[i + 1] for i, arg in enumerate(selected) if arg == '--test'], list(suites))
            self.assertEqual(len(calls), suite_count + count)
            report = json.loads((output / 'native.json').read_text())
            self.assertEqual((report['mode'], report['test_count'], report['suite_count']), ('regressions', count, suite_count))
            self.assertEqual(report['confinement'], confinement)
            self.assertEqual({(row['suite'], row['test']) for row in report['tests']},
                             {(suite, name) for suite, names in suites.items() for name in names})
            self.assertTrue(all(row['passed'] == 1 and row['ignored'] == 0 and row['status'] == 'passed'
                                for row in report['tests']))

    def test_linux_confinement_applies_to_the_actual_binary(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); binary = root / 'test-binary'
            with patch.object(sys, 'platform', 'linux'), \
                    patch.object(native.subprocess, 'run', return_value=result('')) as command:
                native.isolate(binary, ['selected_test', '--exact'], root, {})
            args = command.call_args.args[0]
            self.assertEqual(args[0], 'bwrap')
            self.assertEqual(command.call_args.kwargs['encoding'], 'utf-8')
            self.assertEqual(command.call_args.kwargs['errors'], 'backslashreplace')
            self.assertIn('--unshare-net', args)
            self.assertIn('--proc', args)
            self.assertEqual(args[-3:], [str(binary), 'selected_test', '--exact'])

    def test_windows_confinement_uses_utf8_capture_without_weakening_firewall_rules(self):
        with patch.object(sys, 'platform', 'win32'), \
                patch.object(native.subprocess, 'run', return_value=result('')) as command:
            native.isolate(Path('runtime-probe.exe'), [], Path('scratch'), {}, timeout=30)
        args = command.call_args.args[0]
        self.assertEqual(args[:3], ['pwsh', '-NoProfile', '-Command'])
        self.assertIn('-Direction Outbound -Action Block', args[3])
        self.assertIn('-Direction Inbound -Action Block', args[3])
        self.assertEqual(command.call_args.kwargs['encoding'], 'utf-8')
        self.assertEqual(command.call_args.kwargs['errors'], 'backslashreplace')
        self.assertEqual(command.call_args.kwargs['timeout'], 30)


class NativeCoreTests(unittest.TestCase):
    def test_all_core_features_execute_each_exact_test_through_confinement(self):
        for platform, target in (('linux', 'x86_64-unknown-linux-gnu'),
                                 ('darwin', 'aarch64-apple-darwin'),
                                 ('win32', 'x86_64-pc-windows-msvc')):
            for selection in ['all', *map(str, range(len(FEATURE_SETS)))]:
                with self.subTest(platform=platform, feature_set=selection):
                    self.check_execution(platform, target, selection)

    def check_execution(self, platform, target_name, selection):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); target = root / 'target' / target_name; output = root / 'output'
            suites = native.suite_tests('core', platform)
            inventory = native.core_ignored_tests(platform, target_name, selection)
            records = [artifact(target, suite) for suite in suites]
            calls = []
            def confined(binary, args, scratch, environment):
                calls.append((binary.name, args))
                self.assertNotIn('NDI_SDK_ARCHIVE_TOKEN', environment)
                self.assertEqual(environment['GRAFTON_NDI_ISOLATED_NETWORK'], '1')
                if args[0] == '--list':
                    completed = result(''.join(name + ': test\n' for name in inventory[binary.name]))
                else:
                    self.assertIn(args[0], suites[binary.name])
                    self.assertEqual(args[1:], ['--exact', '--ignored', '--test-threads=1', '--nocapture'])
                    completed = result('test result: ok. 1 passed; 0 failed; 0 ignored;')
                if platform == 'darwin':
                    completed.confinement_evidence = receipt(binary, 'list' if args[0] == '--list' else args[0])
                return completed
            args = ['native.py', '--mode', 'core', '--feature-set', selection,
                    '--target', target_name, '--output', str(output)]
            with patch.dict(os.environ, {'CARGO_TARGET_DIR': str(root / 'target'),
                    'NDI_EXPECTED_RUNTIME': 'fixture-runtime', 'NDI_SDK_ARCHIVE_TOKEN': 'private-token'}), \
                    patch.object(sys, 'argv', args), patch.object(sys, 'platform', platform), \
                    patch.object(native.subprocess, 'run', return_value=result('\n'.join(map(json.dumps, records)))) as build, \
                    patch.object(native, 'isolate', side_effect=confined):
                native.main()
            command = build.call_args.args[0]
            self.assertEqual(command[3:3 + len(feature_flags(selection))], feature_flags(selection))
            self.assertIn('--no-run', command)
            self.assertEqual(command.count('--lib'), 1)
            self.assertEqual([command[index + 1] for index, arg in enumerate(command) if arg == '--test'],
                             [suite for suite in suites if suite != 'grafton_ndi'])
            self.assertEqual(len(calls), 45)
            report = json.loads((output / 'native.json').read_text())
            self.assertEqual((report['mode'], report['feature_set'], report['test_count'], report['suite_count']),
                             ('core', selection, 39, 6))
            self.assertEqual({(row['suite'], row['test']) for row in report['tests']},
                             {(suite, name) for suite, names in suites.items() for name in names})
            if platform == 'darwin':
                self.assertEqual(len(report['listings']), 6)
                self.assertTrue(all('confinement_evidence' in row for row in report['tests']))

    def test_ignored_union_is_feature_and_platform_scoped(self):
        core = set(native.CORE_TESTS['grafton_ndi'])
        for platform, target in (('linux', 'x86_64-unknown-linux-gnu'),
                                 ('darwin', 'aarch64-apple-darwin'),
                                 ('win32', 'x86_64-pc-windows-msvc'),
                                 ('win32', 'i686-pc-windows-msvc')):
            for selection in ['all', *map(str, range(len(FEATURE_SETS)))]:
                features = set(FEATURE_SETS[int(selection)].split(',')) if selection != 'all' else {'image-encoding', 'file-io'}
                observed = native.core_ignored_tests(platform, target, selection)
                expected = core | {'frames::tests::video_padding_throughput'}
                if 'image-encoding' in features:
                    expected.add('frames::tests::image_encoding_resources')
                if 'file-io' in features:
                    expected |= {'file::sdk_tests::' + name for name in native.FILE_TESTS}
                    if platform != 'win32':
                        expected.add('file::sdk_tests::native_file_rejects_standard_posix_before_load')
                    elif target.startswith('i686-'):
                        expected.add('file::sdk_tests::native_file_rejects_windows_x86_before_load')
                self.assertEqual(set(observed['grafton_ndi']), expected)
                self.assertEqual(len(observed['grafton_ndi']), len(expected))
                self.assertEqual(set(observed['diagnose_windows_ci']),
                                 {'test_ndi_environment', 'test_ndi_init_with_timeout'})

    def test_feature_override_cannot_silently_weaken_noncore_suites(self):
        with patch.object(sys, 'argv', ['native.py', '--mode', 'file', '--feature-set', '0',
                '--target', 'triple', '--output', '/unused']), self.assertRaisesRegex(ValueError, 'only core'):
            native.main()


if __name__ == '__main__':
    unittest.main()
