"""Failure summaries expose checked public identities, never arbitrary log text."""
import io
import json
from pathlib import Path
import subprocess
import shutil
import sys
import tempfile
import unittest
from contextlib import redirect_stdout

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from diagnostics import (MAX_INPUT_BYTES, MAX_OUTPUT_BYTES, MACOS_CONFINEMENT_PHASES,
    PublicSources, summarize_text, summarize_log, summarize_exception)
from run import CellRun
from common import profiles
from plan import make_plan
from sandbox_probe import OPERATIONS, TIMEOUT_SECONDS


class DiagnosticTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name) / 'project'
        (self.root / 'src').mkdir(parents=True)
        (self.root / 'src/lib.rs').write_text('mod tests { #[test] fn public_test() {} }\n')
        subprocess.run(['git', 'init', '--quiet', str(self.root)], check=True, capture_output=True)
        subprocess.run(['git', '-C', str(self.root), 'add', '.'], check=True, capture_output=True)
        self.sources = PublicSources(self.root)

    def compiler(self, filename='src/lib.rs', message='mismatched types PRIVATE_SECRET_TOKEN'):
        return json.dumps({'reason': 'compiler-message', 'message': {
            'level': 'error', 'code': {'code': 'E0308', 'explanation': 'PRIVATE_SECRET_TOKEN'},
            'message': message, 'rendered': 'PRIVATE_SECRET_TOKEN',
            'spans': [{'file_name': filename, 'line_start': 1, 'column_start': 2,
                'is_primary': True, 'text': [{'text': 'SDK header contents PRIVATE_SECRET_TOKEN'}]}]}})

    def test_real_rust_panic_exposes_only_public_source_location(self):
        compiler = shutil.which('rustc')
        if compiler is None:
            self.skipTest('Rust compiler unavailable')
        (self.root / 'src/lib.rs').write_text('fn main() { panic!("PRIVATE_PANIC_PAYLOAD"); }\n')
        binary = Path(self.directory.name) / ('panic.exe' if sys.platform == 'win32' else 'panic')
        subprocess.run([compiler, '--edition=2021', 'src/lib.rs', '-o', str(binary)],
                       cwd=self.root, capture_output=True, check=True, timeout=60)
        child = subprocess.run([str(binary)], capture_output=True, text=True, timeout=10)
        self.assertNotEqual(child.returncode, 0)
        self.assertIn('PRIVATE_PANIC_PAYLOAD', child.stderr)
        summary = summarize_text(child.stderr, self.sources)
        self.assertIn('rust-panic', summary['reasons'])
        self.assertEqual(summary['project_locations'], [{'file': 'src/lib.rs', 'line': 1, 'column': 13}])
        self.assertNotIn('PRIVATE_PANIC_PAYLOAD', json.dumps(summary))

    def test_socket_probe_closed_vocabulary_and_output_bound(self):
        report = {'schema': 1, 'diagnostic_only': True, 'platform': 'darwin',
                  'timeout_seconds': TIMEOUT_SECONDS,
                  'operations': [{'operation': op, 'status': 'passed', 'errno': None}
                                 for op in sorted(OPERATIONS)]}
        report['operations'][0].update(status='failed', errno=1)
        summary = summarize_text('SANDBOX_PROBE=' + json.dumps(report), self.sources)
        self.assertEqual(summary['sandbox_operations'], report['operations'])
        self.assertEqual(len(summary['sandbox_operations']), 38)
        self.assertLessEqual(len(json.dumps(summary, sort_keys=True).encode()), MAX_OUTPUT_BYTES)
        for field, value in [('operation', 'PRIVATE_SOCKET_NAME'), ('status', 'PRIVATE_ERROR'),
                             ('errno', 'PRIVATE_ERRNO'), ('errno', True), ('errno', 4096)]:
            bad = json.loads(json.dumps(report))
            bad['operations'][0][field] = value
            invalid = summarize_text('SANDBOX_PROBE=' + json.dumps(bad), self.sources)
            self.assertNotIn('sandbox_operations', invalid)
            self.assertIn('sandbox-probe-invalid', invalid['reasons'])
            self.assertNotIn('PRIVATE_', json.dumps(invalid))
        duplicate = json.loads(json.dumps(report))
        duplicate['operations'][1] = duplicate['operations'][0]
        self.assertNotIn('sandbox_operations', summarize_text('SANDBOX_PROBE=' + json.dumps(duplicate), self.sources))
        for failure in ('execution-failed', 'timed-out', 'invalid-or-unavailable'):
            parsed = summarize_text('SANDBOX_PROBE_FAILURE=' + failure, self.sources)
            self.assertIn('sandbox-probe-' + failure, parsed['reasons'])
        private = summarize_text('SANDBOX_PROBE_FAILURE=PRIVATE_FAILURE', self.sources)
        self.assertNotIn('PRIVATE_FAILURE', json.dumps(private))

    def test_compiler_codes_and_public_locations_without_messages_or_snippets(self):
        summary = summarize_text(self.compiler() + '\n' + self.compiler('/private/sdk/generated.rs'), self.sources)
        self.assertEqual(summary['error_codes'], ['E0308'])
        self.assertEqual(summary['project_locations'], [{'file': 'src/lib.rs', 'line': 1, 'column': 2}])
        self.assertIn('type-mismatch', summary['reasons'])
        rendered = json.dumps(summary)
        for forbidden in ('PRIVATE_SECRET_TOKEN', 'SDK header contents', 'generated.rs', '/private'):
            self.assertNotIn(forbidden, rendered)

    def test_log_write_context_is_closed_and_distinct_from_failed_tests(self):
        for kind in ('io', 'encoding', 'other'):
            marker = 'NATIVE_LOG_WRITE_FAILURE=' + json.dumps({'test': 'tests::public_test', 'kind': kind})
            summary = summarize_text(marker + '\nPRIVATE_SDK_PAYLOAD', self.sources)
            self.assertEqual(summary['native_context'], [
                {'phase': 'log-write', 'test': 'tests::public_test', 'kind': kind}])
            self.assertEqual(summary['reasons'], ['native-log-write-failed'])
            self.assertEqual(summary['failed_tests'], [])
            self.assertNotIn('PRIVATE', json.dumps(summary))
        for record in ({'test': 'PRIVATE_TEST', 'kind': 'io'},
                       {'test': 'public_test', 'kind': 'PRIVATE_ERROR'},
                       {'test': 'public_test', 'kind': 'io', 'path': 'PRIVATE_PATH'},
                       {'test': ['public_test'], 'kind': 'io'},
                       {'test': 'public_test', 'kind': 'encoding', 'errno': 1},
                       ['PRIVATE_PAYLOAD']):
            summary = summarize_text('NATIVE_LOG_WRITE_FAILURE=' + json.dumps(record), self.sources)
            self.assertNotIn('native_context', summary)
            self.assertEqual(summary['failed_tests'], [])
            self.assertNotIn('PRIVATE', json.dumps(summary))

    def test_macos_confinement_failure_uses_only_closed_phase_names(self):
        for phase in MACOS_CONFINEMENT_PHASES:
            for prefix in ('', 'ConfinementError: ', 'macos_executor.ConfinementError: '):
                summary = summarize_text(prefix + 'MACOS_CONFINEMENT_FAILURE=' + phase, self.sources)
                self.assertEqual(summary['reasons'], ['macos-confinement-' + phase])
        for line in ('MACOS_CONFINEMENT_FAILURE=PRIVATE_PHASE',
                     'MACOS_CONFINEMENT_FAILURE=cleanup PRIVATE_PATH',
                     'ConfinementError: PRIVATE_PATH'):
            summary = summarize_text(line, self.sources)
            self.assertEqual(summary['reasons'], [])
            self.assertNotIn('PRIVATE_', json.dumps(summary))

    def test_only_recognized_public_failed_tests_and_source_paths_are_emitted(self):
        (self.root / 'src/untracked.rs').write_text('fn secret_test() {}')
        text = '\n'.join([
            'test tests::public_test ... FAILED',
            'test PRIVATE_SECRET_TOKEN::public_test ... FAILED',
            'test tests::secret_test ... FAILED',
            ' --> src/untracked.rs:3:2',
            f' --> {self.root}/src/lib.rs:1:3',
            ' --> https://secret.invalid/private:4:2',
            'ValueError: tests::public_test: required native test failed or did not execute'])
        summary = summarize_text(text, self.sources)
        self.assertEqual(summary['failed_tests'], ['tests::public_test'])
        self.assertEqual(summary['project_locations'], [{'file': 'src/lib.rs', 'line': 1, 'column': 3}])
        self.assertNotIn('SECRET', json.dumps(summary))
        self.assertNotIn('secret.invalid', json.dumps(summary))

    def test_unstructured_diagnostics_are_classified_without_copying_private_values(self):
        text = '\n'.join(['src/lib.rs:1:4: fatal error: Processing.NDI.Private.h file not found',
            'error: linking with https://secret.invalid/token failed',
            'error[E0463]: can\'t find crate for `std`',
            'error: HTTP Error 403 at https://secret.invalid/PRIVATE_SECRET_TOKEN',
            'error: No space left on device'])
        summary = summarize_text(text, self.sources)
        self.assertEqual(summary['error_codes'], ['E0463'])
        self.assertTrue({'header-not-found', 'linker-failed', 'download-failed', 'disk-full',
                         'target-standard-library-missing'}.issubset(summary['reasons']))
        for forbidden in ('Processing.NDI', 'https://', 'PRIVATE_SECRET_TOKEN'):
            self.assertNotIn(forbidden, json.dumps(summary))

    def test_input_output_limits_and_malformed_records_fail_closed(self):
        path = Path(self.directory.name) / 'large.log'
        path.write_bytes(b'x' * (MAX_INPUT_BYTES + 20) + b'\n' + self.compiler().encode())
        summary = summarize_log(path, self.root)
        self.assertTrue(summary['truncated'])
        self.assertLessEqual(len(json.dumps(summary, sort_keys=True).encode()), MAX_OUTPUT_BYTES)
        malformed = '{"reason":"compiler-message","message":{"level":"error","spans":null}}'
        summary = summarize_text(malformed + '\n' + '\n'.join(f'error[E{i:04d}]: private value' for i in range(100)), self.sources)
        self.assertEqual(len(summary['error_codes']), 8)
        self.assertTrue(summary['truncated'])

    def test_provisioner_failure_keeps_a_bounded_category(self):
        for message, category in (
                ('private GitHub SDK acquisition failed; source details are withheld', 'download-failed'),
                ('SDK acquisition retry budget exhausted', 'download-failed'),
                ('archive SHA256 mismatch: PRIVATE_DIGEST', 'identity-mismatch')):
            with self.subTest(message=message):
                summary = summarize_text('Provisioning failed: ' + message + '\nPRIVATE_LOCATOR_AND_TOKEN', self.sources)
                self.assertIn(category, summary['reasons'])
                self.assertNotIn('PRIVATE_', json.dumps(summary))

    def test_linker_and_panic_evidence_does_not_export_symbol_or_thread_names(self):
        summary = summarize_text('\n'.join([
            'private.obj : error LNK2019: unresolved external symbol PRIVATE_SECRET_TOKEN',
            '/usr/bin/ld: /private/sdk/object.o: undefined reference to PRIVATE_SECRET_TOKEN',
            "thread 'PRIVATE_SECRET_TOKEN' panicked at src/lib.rs:1:2:"]), self.sources)
        self.assertEqual(summary['error_codes'], ['LNK2019'])
        self.assertIn('undefined-link-symbol', summary['reasons'])
        self.assertIn('rust-panic', summary['reasons'])
        self.assertEqual(summary['project_locations'], [{'file': 'src/lib.rs', 'line': 1, 'column': 2}])
        self.assertNotIn('PRIVATE_SECRET_TOKEN', json.dumps(summary))

    def test_numeric_panic_thread_id_and_public_timeout_stage_preserve_privacy(self):
        for message, category in (
                ('initial receiver connections timed out', 'receiver-connection-timed-out'),
                ('initial receiver connection timed out', 'receiver-connection-timed-out'),
                ('PTZ capability enable timed out', 'receiver-ptz-capability-timed-out'),
                ('web-control capability update timed out', 'receiver-web-control-capability-timed-out'),
                ('control status notification timed out', 'receiver-status-notification-timed-out'),
                ('local sender endpoint not discovered: PRIVATE_SOURCE', 'local-source-discovery-timed-out')):
            with self.subTest(category=category):
                summary = summarize_text("thread 'PRIVATE_THREAD' (987654321) panicked at src/lib.rs:1:2:\n"
                                         + message + '\nPRIVATE_PAYLOAD', self.sources)
                self.assertTrue({'rust-panic', category}.issubset(summary['reasons']))
                self.assertEqual(summary['project_locations'], [{'file': 'src/lib.rs', 'line': 1, 'column': 2}])
                for private in ('PRIVATE_', '987654321'):
                    self.assertNotIn(private, json.dumps(summary))

    def test_macro_and_transitive_crate_failures_exclude_arbitrary_names(self):
        summary = summarize_text('\n'.join([
            "error[E0463]: can't find crate for `PRIVATE_CRATE_NAME`",
            "warning: 'PRIVATE_MACRO_NAME' macro redefined [-Wmacro-redefined]"]), self.sources)
        self.assertEqual(summary['error_codes'], ['E0463'])
        self.assertTrue({'crate-dependency-missing', 'compiler-macro-redefined'}.issubset(summary['reasons']))
        self.assertNotIn('PRIVATE_CRATE_NAME', json.dumps(summary))
        self.assertNotIn('PRIVATE_MACRO_NAME', json.dumps(summary))

    def test_unsupported_native_archive_excludes_library_and_path(self):
        summary = summarize_text('error: failed to add native library /private/PRIVATE_LIBRARY.a: '
                                 'Unsupported archive identifier', self.sources)
        self.assertIn('unsupported-native-archive', summary['reasons'])
        self.assertNotIn('PRIVATE_LIBRARY', json.dumps(summary))
        self.assertNotIn('/private', json.dumps(summary))

    def test_apple_linker_variants_export_categories_without_private_values(self):
        cases = [
            ('Undefined symbols: PRIVATE_SYMBOL', 'undefined-link-symbol'),
            ('ld: symbol(s) not found for architecture PRIVATE_ARCH', 'undefined-link-symbol'),
            ("ld: framework 'PRIVATE_FRAMEWORK' not found", 'link-framework-missing'),
            ("ld: library 'PRIVATE_LIBRARY' not found", 'link-library-missing'),
            ('ld: building for PRIVATE_PLATFORM but linking in dylib built for PRIVATE_PLATFORM2: /private/sdk.dylib', 'incompatible-native-platform'),
            ('ld: incompatible architecture PRIVATE_ARCH in /private/sdk.o', 'incompatible-native-architecture'),
            ('ld: file was built for PRIVATE_ARCH which is not the architecture being linked', 'incompatible-native-architecture'),
            ('ld: duplicate symbol PRIVATE_SYMBOL in /private/sdk.a', 'duplicate-link-symbol'),
            ('ld: malformed archive /private/PRIVATE_LIBRARY.a', 'unsupported-native-archive'),
            ('ld: malformed object PRIVATE_OBJECT at https://secret.invalid/private', 'invalid-native-object'),
            ('ld: unsupported file format PRIVATE_FORMAT in /private/sdk.o', 'invalid-native-object'),
            ('ld: unknown file type in /private/PRIVATE_OBJECT', 'invalid-native-object'),
        ]
        for message, category in cases:
            with self.subTest(category=category, message=message):
                summary = summarize_text(message, self.sources)
                self.assertIn(category, summary['reasons'])
                for forbidden in ('PRIVATE_', '/private', 'https://', 'secret.invalid'):
                    self.assertNotIn(forbidden, json.dumps(summary))

    def test_uppercase_tool_errors_are_classified_without_exporting_text(self):
        summary = summarize_text('ERROR: No such file or directory: '
                                 '/private/PRIVATE_PATH https://secret.invalid/token', self.sources)
        self.assertIn('file-or-command-missing', summary['reasons'])
        self.assertNotIn('PRIVATE_PATH', json.dumps(summary))
        self.assertNotIn('secret.invalid', json.dumps(summary))

    def test_windows_object_tool_failures_exclude_private_paths(self):
        cases = [
            ('ERROR: LLVM readobj/objcopy are required for target object inspection: PRIVATE_PATH', 'object-inspection-tool-missing'),
            (r'ERROR: C:\private\llvm-objcopy.exe failed (1): PRIVATE_VALUE', 'object-inspection-tool-failed'),
            ('ERROR: /private/llvm-objcopy-14 failed (1): option not supported by llvm-objcopy for COFF', 'object-inspection-tool-failed'),
            (r'ERROR: [WinError 3] The system cannot find the path specified: C:\PRIVATE_PATH', 'file-or-command-missing'),
        ]
        for message, category in cases:
            with self.subTest(category=category):
                summary = summarize_text(message, self.sources)
                self.assertIn(category, summary['reasons'])
                self.assertNotIn('PRIVATE_', json.dumps(summary))
                self.assertNotIn('private', json.dumps(summary))

    def test_probe_stages_use_a_closed_vocabulary(self):
        for stage in ('configuration', 'c-compile', 'rust-compile', 'dependency-inventory',
                      'object-identity', 'section-extraction', 'comparison', 'record-identity'):
            with self.subTest(stage=stage):
                summary = summarize_text('ABI_PROBE_STAGE=' + stage, self.sources)
                self.assertEqual(summary['reasons'], ['abi-stage-' + stage])
        summary = summarize_text('\n'.join(['ABI_PROBE_STAGE=private-secret-value',
            'ABI_PROBE_STAGE=configuration PRIVATE_SECRET',
            'ABI_PROBE_STAGE=https://secret.invalid/private']), self.sources)
        self.assertEqual(summary['reasons'], [])
        self.assertNotIn('private', json.dumps(summary))

    def test_mobile_runtime_states_use_a_closed_vocabulary(self):
        for state in ('result-missing', 'native-running', 'native-error', 'native-panic',
                      'result-invalid', 'result-write-failed', 'app-exited', 'launch-failed'):
            with self.subTest(state=state):
                summary = summarize_text('MOBILE_SMOKE_STATE=' + state, self.sources)
                self.assertEqual(summary['reasons'], ['mobile-smoke-' + state])
        summary = summarize_text('\n'.join(['MOBILE_SMOKE_STATE=private-secret-value',
            'MOBILE_SMOKE_STATE=native-error PRIVATE_PAYLOAD',
            'MOBILE_SMOKE_STATE=https://secret.invalid/private']), self.sources)
        self.assertEqual(summary['reasons'], [])
        self.assertNotIn('private', json.dumps(summary))

    def test_mobile_runtime_phases_use_a_closed_vocabulary(self):
        for phase in ('boot', 'install', 'launch', 'result', 'terminate'):
            with self.subTest(phase=phase):
                summary = summarize_text('MOBILE_SMOKE_PHASE=' + phase, self.sources)
                self.assertEqual(summary['reasons'], ['mobile-phase-' + phase])
        summary = summarize_text('MOBILE_SMOKE_PHASE=private-phase\n'
                                 'MOBILE_SMOKE_PHASE=result PRIVATE_PAYLOAD', self.sources)
        self.assertEqual(summary['reasons'], [])
        self.assertNotIn('private', json.dumps(summary))

    def test_missing_system_libraries_use_only_the_reviewed_allowlist(self):
        libraries = {'c++': 'cxx', 'objc': 'objc', 'z': 'z', 'dns_sd': 'dns-sd',
                     'iconv': 'iconv', 'System': 'system', 'c': 'c', 'm': 'm'}
        for library, category in libraries.items():
            for diagnostic in (f"ld: library '{library}' not found",
                               f'ld: library not found for -l{library}',
                               f'ld: cannot find -l{library}: No such file or directory'):
                with self.subTest(library=library, diagnostic=diagnostic):
                    summary = summarize_text(diagnostic, self.sources)
                    self.assertIn('link-system-library-missing-' + category, summary['reasons'])
        for name in ('PRIVATE_LIBRARY', 'iconv_PRIVATE_VALUE', 'dns_sd/private/token',
                     'https://secret.invalid/token'):
            summary = summarize_text(f"ld: library '{name}' not found\n"
                                     f'ld: library not found for -l{name}', self.sources)
            self.assertIn('link-library-missing', summary['reasons'])
            self.assertFalse(any(r.startswith('link-system-library-missing-') for r in summary['reasons']))
            self.assertNotIn(name, json.dumps(summary))

    def test_public_runtime_error_variants_exclude_private_payloads(self):
        for variant, category in (
                ('RuntimeLoadFailed', 'runtime-load-failed'),
                ('InitializationFailed', 'runtime-initialization-failed'),
                ('NativeOperationFailed', 'native-operation-failed'),
                ('NullPointer', 'native-null-pointer')):
            with self.subTest(variant=variant):
                summary = summarize_text(f'Error: {variant}("PRIVATE_VALUE /private/sdk '
                                         'https://secret.invalid/token")', self.sources)
                self.assertIn(category, summary['reasons'])
                for forbidden in ('PRIVATE_VALUE', '/private', 'secret.invalid'):
                    self.assertNotIn(forbidden, json.dumps(summary))

    def test_initialization_phases_use_only_exact_public_source_literals(self):
        for message, category in (
                ('NDIlib_initialize failed', 'runtime-initialize-rejected'),
                ('Failed to create NDI send instance', 'sender-create-rejected')):
            with self.subTest(category=category):
                summary = summarize_text(f'Error: InitializationFailed("{message}: '
                                         'PRIVATE_PAYLOAD /private/sdk")', self.sources)
                self.assertTrue({'runtime-initialization-failed', category}.issubset(summary['reasons']))
                self.assertNotIn('PRIVATE_PAYLOAD', json.dumps(summary))
                self.assertNotIn('/private', json.dumps(summary))
        summary = summarize_text('Error: InitializationFailed("Failed to create PRIVATE_OBJECT")', self.sources)
        self.assertEqual(summary['reasons'], ['runtime-initialization-failed'])

    def test_owned_mobile_error_prefix_preserves_phase_without_payload(self):
        summary = summarize_text('NDI smoke failed: Failed to initialize the NDI runtime: '
            'Failed to create NDI send instance; PRIVATE_PAYLOAD https://secret.invalid/token', self.sources)
        self.assertTrue({'runtime-initialization-failed', 'sender-create-rejected'}.issubset(summary['reasons']))
        for forbidden in ('PRIVATE_PAYLOAD', 'https://', 'secret.invalid'):
            self.assertNotIn(forbidden, json.dumps(summary))
        self.assertEqual(summarize_text('NDI smoke failed: PRIVATE_PAYLOAD', self.sources)['reasons'], [])

    def test_bootstrap_exceptions_retain_category_without_private_error_text(self):
        try:
            raise PermissionError(13, 'Permission denied: https://secret.invalid/PRIVATE_SECRET_TOKEN')
        except PermissionError as error:
            summary = summarize_exception(error, self.root)
        self.assertEqual(summary['exception'], 'PermissionError')
        self.assertEqual(summary['errno'], 13)
        self.assertIn('permission-denied', summary['reasons'])
        self.assertNotIn('secret.invalid', json.dumps(summary))
        summary = summarize_exception(subprocess.CalledProcessError(7,
            ['https://secret.invalid/PRIVATE_SECRET_TOKEN']), self.root)
        self.assertEqual(summary['exit_code'], 7)
        self.assertNotIn('PRIVATE_SECRET_TOKEN', json.dumps(summary))

    def test_failed_command_exports_only_bounded_summary_and_preserves_raw_log_privately(self):
        cell = next(c for c in make_plan(profiles()) if c['profile'] == 'x86_64-linux-gnu')
        run = CellRun(cell, Path(self.directory.name) / 'output')
        script = 'import sys\nprint(' + repr(self.compiler()) + ')\nsys.exit(101)\n'
        output = io.StringIO()
        with redirect_stdout(output):
            with self.assertRaisesRegex(ValueError, 'failed with exit code 101'):
                run.command('link-examples', [sys.executable, '-c', script])
        self.assertEqual(run.commands[-1]['diagnostics']['error_codes'], ['E0308'])
        self.assertNotIn('PRIVATE_SECRET_TOKEN', output.getvalue())
        self.assertNotIn('PRIVATE_SECRET_TOKEN', json.dumps(run.commands))
        self.assertIn('PRIVATE_SECRET_TOKEN', (run.output / 'link-examples.log').read_text())


if __name__ == '__main__':
    unittest.main()
