"""Cargo artifact discovery must survive layout changes without host substitution."""
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import cargo_artifacts


class ArtifactTests(unittest.TestCase):
    def fixture(self, root, path, name='std'):
        output = root / path
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_bytes(b'fixture')
        return {'reason': 'compiler-artifact', 'target': {'name': name}, 'filenames': [str(output)]}

    def test_legacy_and_new_layouts_are_selected_from_messages(self):
        for relative in ('debug/deps/libstd-hash.rlib', 'debug/build/std/hash/out/libstd-hash.rlib'):
            with self.subTest(layout=relative), tempfile.TemporaryDirectory() as directory:
                root = Path(directory) / 'target' / 'selected-triple'
                record = self.fixture(root, relative)
                parsed = cargo_artifacts.messages('ordinary diagnostic\n' + json.dumps(record))
                self.assertEqual(cargo_artifacts.library(parsed, 'std', '.rlib', root), root / relative)
                self.assertEqual(cargo_artifacts.dependency_arguments(parsed, root), ['-Ldependency=' + str((root / relative).parent)])

    def test_host_dependency_cannot_replace_target_std(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            record = self.fixture(root, 'debug/deps/libstd-host.rlib')
            with self.assertRaisesRegex(ValueError, 'outside the selected target'):
                cargo_artifacts.library([record], 'std', '.rlib', root / 'target-triple')
            with self.assertRaisesRegex(ValueError, 'no target dependency'):
                cargo_artifacts.dependency_arguments([record], root / 'target-triple')

    def test_declared_host_proc_macro_is_available_to_target_crate_metadata(self):
        for suffix in ('.so', '.dylib', '.dll'):
            with self.subTest(suffix=suffix), tempfile.TemporaryDirectory() as directory:
                root = Path(directory) / 'target'
                target = root / 'selected-triple'
                library = self.fixture(target, 'debug/deps/libgrafton_ndi-hash.rlib', 'grafton_ndi')
                macro = self.fixture(root, 'debug/deps/libthiserror_impl-hash' + suffix, 'thiserror_impl')
                macro['target']['kind'] = ['proc-macro']
                ordinary_host = self.fixture(root, 'debug/other/libstd-host.rlib')
                args = cargo_artifacts.dependency_arguments([library, macro, ordinary_host], target)
                self.assertEqual(set(args), {'-Ldependency=' + str(target / 'debug/deps'),
                                             '-Ldependency=' + str(root / 'debug/deps')})
                self.assertEqual(cargo_artifacts.dependency_arguments(
                    [library, macro], target, include_host_proc_macros=False),
                    ['-Ldependency=' + str(target / 'debug/deps')])

    def test_proc_macro_must_exist_inside_the_same_cargo_output_root(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / 'target/selected-triple'
            library = self.fixture(target, 'debug/deps/libgrafton_ndi-hash.rlib', 'grafton_ndi')
            macro = self.fixture(root, 'elsewhere/libthiserror_impl-hash.so', 'thiserror_impl')
            macro['target']['kind'] = ['proc-macro']
            with self.assertRaisesRegex(ValueError, 'outside the selected target'):
                cargo_artifacts.dependency_arguments([library, macro], target)
            macro = self.fixture(root, 'target/debug/deps/libthiserror_impl-hash.so', 'thiserror_impl')
            macro['target']['kind'] = ['proc-macro']
            Path(macro['filenames'][0]).unlink()
            with self.assertRaisesRegex(ValueError, 'missing'):
                cargo_artifacts.dependency_arguments([library, macro], target)

    def test_ambiguous_or_missing_artifacts_fail(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            records = [self.fixture(root, f'debug/deps/libstd-{suffix}.rlib') for suffix in ('one', 'two')]
            for values in (records, []):
                with self.assertRaisesRegex(ValueError, 'exactly one'):
                    cargo_artifacts.library(values, 'std', '.rlib', root)
            (root / 'debug/deps/libstd-one.rlib').unlink()
            with self.assertRaisesRegex(ValueError, 'missing'):
                cargo_artifacts.library(records[:1], 'std', '.rlib', root)

    def native_fixture(self, root):
        target = root / 'target/selected-triple'
        package = root / 'registry/target-package'
        package.mkdir(parents=True)
        manifest = package / 'Cargo.toml'
        manifest.write_text('[package]\nname="target-package"\nversion="0.0.0"\n')
        native = package / 'lib'; native.mkdir()
        output = target / 'debug/build/target-package/out'; output.mkdir(parents=True)
        library = self.fixture(target, 'debug/deps/libtarget_package.rlib', 'target_package')
        library.update(package_id='registry#target-package@0.0.0', manifest_path=str(manifest))
        build = {'reason': 'build-script-executed', 'package_id': library['package_id'],
                 'out_dir': str(output), 'linked_paths': ['native=' + str(native)]}
        return target, package, output, library, build

    def test_native_search_paths_use_matching_target_package_and_output(self):
        with tempfile.TemporaryDirectory() as directory:
            target, package, output, library, build = self.native_fixture(Path(directory))
            build['linked_paths'] = ['native=' + str(output), 'native=' + str(package / 'lib'),
                                     'native=' + str(output)]
            host = dict(build, out_dir=str(target.parent / 'debug/build/host/out'),
                        linked_paths=['native=/host-only-unavailable'])
            args = cargo_artifacts.native_link_arguments([host, build, library], target)
            self.assertEqual(args, ['-Lnative=' + str(output), '-Lnative=' + str(package / 'lib')])
            self.assertEqual(cargo_artifacts.native_link_arguments([host, library], target), [])

    def test_native_search_rejects_missing_unscoped_relative_or_unknown_paths(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target, package, output, library, build = self.native_fixture(root)
            unrelated = root / 'unrelated'; unrelated.mkdir()
            for value in ('native=' + str(unrelated), 'native=' + str(package / 'missing'),
                          'native=relative', 'dependency=' + str(package / 'lib'),
                          'unknown=' + str(package / 'lib'), 'native=bad\npath'):
                with self.subTest(value=value), self.assertRaises(ValueError):
                    cargo_artifacts.native_link_arguments([library, dict(build, linked_paths=[value])], target)
            for records in ([build], [library, dict(build, package_id='other-package')],
                            [library, dict(build, out_dir=str(output / 'missing'))]):
                with self.subTest(records=records), self.assertRaises(ValueError):
                    cargo_artifacts.native_link_arguments(records, target)
            (package / 'Cargo.toml').unlink()
            with self.assertRaisesRegex(ValueError, 'manifest is missing'):
                cargo_artifacts.native_link_arguments([library, build], target)

    def test_native_search_preserves_declared_kinds_and_rejects_symlink_escape(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target, package, output, library, build = self.native_fixture(root)
            build['linked_paths'] = [str(package / 'lib'), 'framework=' + str(package / 'lib')]
            self.assertEqual(cargo_artifacts.native_link_arguments([library, build], target),
                             ['-Lall=' + str(package / 'lib'), '-Lframework=' + str(package / 'lib')])
            escape = package / 'escape'
            try:
                escape.symlink_to(root, target_is_directory=True)
            except OSError:
                return  # Some Windows accounts cannot create directory symlinks.
            with self.assertRaisesRegex(ValueError, 'outside its target dependency'):
                cargo_artifacts.native_link_arguments([library, dict(build,
                    linked_paths=['native=' + str(escape)])], target)

    def test_rebuilt_std_probe_uses_full_metadata_and_explicit_builtins(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            records = [self.fixture(root, f'debug/build/{name}/out/lib{name}.rmeta', name)
                       for name in ('std', 'compiler_builtins')]
            args = cargo_artifacts.rebuilt_std_arguments(records, root)
            self.assertIn('--extern=std=' + str(root / 'debug/build/std/out/libstd.rmeta'), args)
            self.assertIn('--extern=compiler_builtins=' + str(root / 'debug/build/compiler_builtins/out/libcompiler_builtins.rmeta'), args)
            with self.assertRaisesRegex(ValueError, 'exactly one'):
                cargo_artifacts.rebuilt_std_arguments(records[:1], root)

    def test_bindings_use_recorded_out_dir_and_require_unique_existing_file(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / 'debug/build/grafton-ndi/hash/out'
            output.mkdir(parents=True)
            (output / 'ndi_lib.rs').write_text('fixture')
            record = {'reason': 'build-script-executed', 'out_dir': str(output)}
            self.assertEqual(cargo_artifacts.generated_bindings([record], root), output / 'ndi_lib.rs')
            with self.assertRaisesRegex(ValueError, 'outside the selected target'):
                cargo_artifacts.generated_bindings([record], root / 'other-target')
            with self.assertRaisesRegex(ValueError, 'expected one'):
                cargo_artifacts.generated_bindings([], root)


if __name__ == '__main__':
    unittest.main()
