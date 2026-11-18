"""The runtime-swap policy skips only exact, audited, still-owned build tests."""
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import sdk_test_scope as scope  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
LINUX = 'x86_64-unknown-linux-gnu'
WINDOWS = 'x86_64-pc-windows-msvc'
MACOS = 'aarch64-apple-darwin'


def listed(*names):
    return ''.join(name + ': test\n' for name in names)


class InventorySelectionTests(unittest.TestCase):
    def test_miri_applies_literal_filters_in_group_order_with_exact_skips(self):
        filters = {group: entries for group, entries in scope.FILTER_SELECTIONS.items()
                   if group in scope.MIRI_GROUPS}
        fixtures = {group: [entry + '_fixture' for entry in entries] for group, entries in filters.items()}
        names = [name for group in fixtures.values() for name in group]
        text = listed(*reversed(names), *scope.MIRI_SKIPS,
                      scope.MIRI_SKIPS[1] + '_extra_coverage')
        fixtures['miri-reads'].insert(1, scope.MIRI_SKIPS[1] + '_extra_coverage')
        for group, expected in fixtures.items():
            with self.subTest(group=group):
                self.assertEqual(scope.selection_arguments(text, group), expected)

    def test_missing_empty_and_overlapping_filters_fail(self):
        cases = [
            ({'miri': ()}, 'is empty'),
            ({'miri': ('',)}, 'selects no tests'),
            ({'miri': ('absent',)}, 'selects no tests'),
            ({'miri': ('frames', 'padding')}, 'matches both'),
            ({'miri': ('frames',), 'memcheck': ('padding',)}, 'matches both'),
        ]
        for groups, expected in cases:
            with self.subTest(groups=groups), self.assertRaisesRegex(ValueError, expected):
                scope.filter_groups(['frames::padding'], groups)

    def test_missing_or_collateral_skip_fails(self):
        with self.assertRaisesRegex(ValueError, 'skip .* does not name an inventory test'):
            scope.filter_groups(['test_to_skip_renamed', 'test_to_keep'], {'run': ('test_to_keep',)},
                                skips=('test_to_skip',))

    def test_invalid_and_duplicate_inventory_lines_fail(self):
        for text, expected in (
            ('invalid: entry\n', 'unrecognized'),
            ('-invalid: test\n', 'unrecognized'),
            (listed('test', 'test'), 'duplicate'),
        ):
            with self.subTest(text=text), self.assertRaisesRegex(ValueError, expected):
                scope.test_names(text)
        self.assertEqual(scope.test_names('a: test\nb: benchmark\n\n1 test, 1 benchmark\n'), ['a'])

    def test_exact_group_rejects_renamed_fixture_or_new_unclassified_fixture(self):
        for group, fixtures in scope.EXACT_SELECTIONS.items():
            with self.subTest(group=group):
                self.assertEqual(scope.selection_arguments(listed(*fixtures), group), list(fixtures))
                with self.assertRaisesRegex(ValueError, 'inventory differs'):
                    scope.selection_arguments(listed(*fixtures[1:], fixtures[0] + '_renamed'), group)
                with self.assertRaisesRegex(ValueError, 'unexpected'):
                    scope.selection_arguments(listed(*fixtures, 'unclassified'), group)

    def test_cli_writes_one_exact_name_per_line(self):
        names = ('frames::tests::video_padding_encoders_one', 'frames::tests::video_padding_encoders_two')
        with tempfile.TemporaryDirectory() as directory:
            inventory = Path(directory) / 'inventory'
            selection = Path(directory) / 'selected'
            inventory.write_text(listed(*names, 'other_fixture'), encoding='utf-8')
            command = [sys.executable, str(ROOT / 'ci/sdk_test_scope.py'), 'select', '--group',
                       'encoder-padding', '--inventory', str(inventory), '--output', str(selection)]
            result = subprocess.run(command, text=True, capture_output=True, check=True)
            self.assertEqual(selection.read_bytes(), ('\n'.join(names) + '\n').encode())
            self.assertIn('selected 2 exact tests', result.stderr)
            inventory.write_text(listed('other_fixture'), encoding='utf-8')
            result = subprocess.run(command, text=True, capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('selects no tests', result.stderr)
# Native File tests share a module family with runtime-free worker mocks.
# Those names, all new tests and the doctest must survive a compatibility run.
NATIVE = (
    scope.NATIVE_WITNESS,
    'file::asynchronous::native_tests::a_native_finish_reports_confirmed_closure_when_another_clone_closes_it',
    'alpha_sources_keep_their_tagged_matrix_with_best_and_fastest',
    'split_clocked_video_keeps_audio_flowing_on_another_thread',
    'receiver::latest_report_tests::a_new_unclassified_case',
    'new_compiler_fixture_in_a_previously_audited_file',
    'src/runtime.rs - runtime::Ndi::new (line 20)',
)


def inventory(target=LINUX, headers='standard', extra=NATIVE):
    names = scope.expected_tests(target, headers) + list(extra)
    return '\n'.join(name + ': test' for name in names) + '\n'


class RuntimeTestScopeTests(unittest.TestCase):
    def test_compiler_audit_includes_internal_probes_beyond_the_original_four(self):
        names = set(scope.expected_tests(LINUX, 'advanced'))
        self.assertTrue({
            'compile_contracts',
            'advanced_ownership_contracts',
            scope.HEADER_TEST,
            'a_foreign_mutex_cannot_supply_the_finder_snapshot_guard',
            'attached_modes_fail_the_internal_capture_entry_points',
            'frame_parts_require_the_matching_descriptor_validator',
            'native_loans_and_slot_keys_require_their_owner',
            'root_glob_import_keeps_external_crates_unambiguous',
            'unsupported_apple_abis_fail_before_the_build_script_opens_an_sdk',
        }.issubset(names))

    def test_each_audit_entry_names_a_real_function_in_its_cited_source(self):
        for source, names in scope.AUDITED.items():
            functions = set(re.findall(r'\bfn\s+(\w+)\s*\(', (ROOT / source).read_text(encoding='utf-8')))
            for name in names:
                with self.subTest(source=source, name=name):
                    self.assertIn(name.rsplit('::', 1)[-1], functions)

    def test_native_unknown_and_doc_tests_remain_in_every_build_configuration(self):
        for target in scope.TARGET_OSES:
            for headers in ('standard', 'advanced'):
                with self.subTest(target=target, headers=headers):
                    args = scope.skip_arguments(inventory(target, headers), target, headers)
                    self.assertEqual(args[::2], ['--skip'] * (len(args) // 2))
                    skips = args[1::2]
                    remaining = [name for name in NATIVE if not any(skip in name for skip in skips)]
                    self.assertEqual(remaining, list(NATIVE))
                    self.assertGreater(len(skips), 0)

    def test_an_empty_or_compiler_only_inventory_is_refused(self):
        for text in ('', '0 tests, 0 benchmarks\n', inventory(extra=())):
            with self.subTest(text=text[:30]):
                with self.assertRaisesRegex(ValueError, 'native lifecycle test'):
                    scope.skip_arguments(text, LINUX, 'standard')

    def test_a_missing_or_renamed_expected_compiler_case_is_refused(self):
        for removed in ('compile_contracts', 'native_loans_and_slot_keys_require_their_owner'):
            text = inventory().replace(removed + ': test\n', removed + '_renamed: test\n')
            with self.subTest(removed=removed):
                with self.assertRaisesRegex(ValueError, 'must match exactly once'):
                    scope.skip_arguments(text, LINUX, 'standard')

    def test_a_duplicate_function_name_in_another_binary_is_not_silently_skipped(self):
        with self.assertRaisesRegex(ValueError, 'collateral tests'):
            scope.skip_arguments(inventory() + 'compile_contracts: test\n', LINUX, 'standard')

    def test_substrings_cannot_remove_a_new_native_test_or_doc_test(self):
        for collateral in ('native_compile_contracts_round_trip',
                           'native::compile_contracts',
                           'src/native.rs - native::compile_contracts (line 10)'):
            with self.subTest(collateral=collateral):
                with self.assertRaisesRegex(ValueError, 'collateral tests'):
                    scope.skip_arguments(inventory() + collateral + ': test\n', LINUX, 'standard')

    def test_fixed_graph_scope_accepts_lower_features_and_skips_only_three_probes(self):
        names = [
            'attached_modes_fail_the_internal_capture_entry_points',
            'frame_parts_require_the_matching_descriptor_validator',
            'native_loans_and_slot_keys_require_their_owner',
        ]
        text = ''.join(name + ': test\n' for name in names + list(NATIVE))
        text += 'absent_feature_error_variants_are_unavailable: test\n'
        args = scope.skip_arguments(text, scope='fixed-graph')
        self.assertEqual(set(args[1::2]), set(names))
        self.assertNotIn('absent_feature_error_variants_are_unavailable', args)

    def test_synthetic_header_scope_skips_only_its_fixed_fixture_matrix(self):
        args = scope.skip_arguments(inventory(headers='advanced'), scope='synthetic-headers')
        self.assertEqual(args, ['--skip', scope.HEADER_TEST])
        with self.assertRaisesRegex(ValueError, 'must match exactly once'):
            scope.skip_arguments(inventory(), scope='synthetic-headers')
        for mode in ('synthetic-headers', 'fixed-graph'):
            known = scope.expected_tests(scope=mode)[0]
            with self.subTest(mode=mode):
                with self.assertRaisesRegex(ValueError, 'collateral tests'):
                    scope.skip_arguments(inventory(headers='advanced') + known + ': test\n', scope=mode)

    def test_native_witness_is_also_built_on_windows_and_macos(self):
        self.assertEqual(scope.NATIVE_WITNESS,
                         'handles_keep_runtime_alive_until_last_drop_and_allow_reinitialization')
        for target in (WINDOWS, MACOS):
            text = inventory(target)
            self.assertNotIn('provisioned_identity_is_checked_in_the_process_that_loads_the_runtime', text)
            self.assertTrue(scope.skip_arguments(text, target, 'standard'))
            with self.assertRaisesRegex(ValueError, 'native lifecycle test'):
                scope.skip_arguments(text.replace(scope.NATIVE_WITNESS + ': test\n', ''), target, 'standard')

    def test_a_default_or_partial_feature_inventory_is_not_accepted(self):
        for missing in ('advanced_ownership_contracts', 'tokio_types_keep_their_auto_traits',
                        'smol_types_keep_their_auto_traits', 'file_types_keep_their_auto_traits',
                        'image_encoding_types_keep_their_auto_traits'):
            with self.subTest(missing=missing):
                with self.assertRaises(ValueError):
                    scope.skip_arguments(inventory().replace(missing + ': test\n', ''), LINUX, 'standard')
        with self.assertRaisesRegex(ValueError, 'all-features'):
            scope.skip_arguments(inventory() + 'absent_feature_error_variants_are_unavailable: test\n',
                                 LINUX, 'standard')

    def test_header_absence_is_explicit_rather_than_a_missing_test_exception(self):
        self.assertNotIn(scope.HEADER_TEST, scope.expected_tests(LINUX, 'standard'))
        self.assertIn(scope.HEADER_TEST, scope.expected_tests(LINUX, 'advanced'))
        with self.assertRaisesRegex(ValueError, 'must match exactly once'):
            scope.skip_arguments(inventory(), LINUX, 'advanced')
        with self.assertRaisesRegex(ValueError, 'unexpected test'):
            scope.skip_arguments(inventory(headers='advanced'), LINUX, 'standard')

    def test_os_specific_build_probes_are_required_exactly_where_they_compile(self):
        symlinks = 'directory_accepts_same_target_aliases_with_declared_preference'
        bundle = 'rustc_can_bundle_the_materialized_archive_without_an_apple_sdk'
        self.assertIn(bundle, scope.expected_tests(LINUX, 'standard'))
        self.assertIn(symlinks, scope.expected_tests(MACOS, 'standard'))
        self.assertNotIn(bundle, scope.expected_tests(MACOS, 'standard'))
        self.assertNotIn(symlinks, scope.expected_tests(WINDOWS, 'standard'))
        for text, target in ((inventory().replace(bundle + ': test\n', ''), LINUX),
                             (inventory(), WINDOWS),
                             (inventory(MACOS).replace(symlinks + ': test\n', ''), MACOS)):
            with self.subTest(target=target):
                with self.assertRaises(ValueError):
                    scope.skip_arguments(text, target, 'standard')

    def test_unknown_targets_and_header_editions_fail_conservatively(self):
        for target, headers in (('new-platform', 'standard'), (LINUX, 'new-edition')):
            with self.subTest(target=target, headers=headers):
                with self.assertRaises(ValueError):
                    scope.skip_arguments(inventory(), target, headers)

    def test_cli_writes_lf_delimited_arguments_even_for_crlf_inventory(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / 'inventory'
            output = Path(directory) / 'args'
            source.write_bytes(inventory().replace('\n', '\r\n').encode())
            result = subprocess.run([
                sys.executable, str(ROOT / 'ci/sdk_test_scope.py'),
                '--inventory', str(source), '--target', LINUX,
                '--headers', 'standard', '--output', str(output),
            ], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, '')
            data = output.read_bytes()
            self.assertNotIn(b'\r', data)
            self.assertEqual(data.decode().splitlines(),
                             scope.skip_arguments(inventory(), LINUX, 'standard'))
            self.assertIn('unclassified tests and doctests remain included', result.stderr)

    def test_cli_validation_failure_does_not_create_a_skip_file(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / 'inventory'
            output = Path(directory) / 'args'
            source.write_text('compile_contracts: test\n')
            result = subprocess.run([
                sys.executable, str(ROOT / 'ci/sdk_test_scope.py'),
                '--inventory', str(source), '--target', LINUX,
                '--headers', 'standard', '--output', str(output),
            ], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(output.exists())


if __name__ == '__main__':
    unittest.main()
