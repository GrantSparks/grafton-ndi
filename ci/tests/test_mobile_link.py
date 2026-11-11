"""Validate the compiler's native link contract without vendor SDK inputs."""
from pathlib import Path
import contextlib
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from mobile import native_static_libraries
from run import CellRun


class NativeStaticLibrariesTests(unittest.TestCase):
    def test_preserves_framework_order_and_repeated_libraries(self):
        flags = ['-framework', 'CoreFoundation', '-lc++', '-liconv', '-lc++', '-lSystem']
        self.assertEqual(native_static_libraries('other diagnostic\nnote: native-static-libs: '
                                                + ' '.join(flags) + '\n'), flags)

    def test_rejects_missing_ambiguous_or_unbounded_contract(self):
        for stderr in ('', 'note: native-static-libs: ',
                       'note: native-static-libs: -lc\nnote: native-static-libs: -lc',
                       'note: native-static-libs: ' + 'x' * 16385,
                       'note: native-static-libs: ' + '-lc ' * 257):
            with self.subTest(stderr=stderr[:100]), self.assertRaises(ValueError):
                native_static_libraries(stderr)

    def test_rejects_options_paths_response_files_and_malformed_frameworks(self):
        for flag in ('-Wl,-undefined,dynamic_lookup', '@response', '/private/library.a',
                     '-L/private', '-l:library.a', '-l../library', '-lc;echo', '-lc$(id)',
                     '-framework', '-framework -lc', '-framework ../UIKit',
                     '-framework "UIKit"', '-framework UIKit/Other',
                     '-framework ' + 'A' * 129, '-l' + 'a' * 129):
            with self.subTest(flag=flag), self.assertRaises(ValueError):
                native_static_libraries('note: native-static-libs: ' + flag)

    def test_failed_cargo_build_preserves_only_bounded_public_diagnostics(self):
        cargo = shutil.which('cargo')
        if not cargo:
            self.skipTest('Cargo is required for the failing compilation regression')
        with tempfile.TemporaryDirectory(prefix='ndi-static-build-failure-') as temporary:
            root = Path(temporary)
            project = root / 'project'; project.mkdir()
            (project / 'Cargo.toml').write_text('[package]\nname="static-failure"\n'
                'version="0.0.0"\nedition="2021"\n[lib]\npath="lib.rs"\n'
                'crate-type=["staticlib"]\n')
            private_name = 'private_vendor_identifier_never_exported'
            (project / 'lib.rs').write_text('pub fn fails() { ' + private_name + '(); }\n')
            private_output = root / 'compiler'; private_output.mkdir()
            args = [cargo, 'rustc', '--offline', '--color', 'never',
                    '--message-format=json-render-diagnostics', '--manifest-path',
                    str(project / 'Cargo.toml'), '--lib', '--', '--print=native-static-libs']
            child = root / 'child.py'
            child.write_text('import os, sys\nfrom pathlib import Path\n'
                'sys.path.insert(0, ' + repr(str(Path(__file__).resolve().parents[1])) + ')\n'
                'from mobile import build_static_library\n'
                'build_static_library(' + repr(args) + ', Path(' + repr(str(private_output))
                + '), os.environ.copy())\n')
            cell = CellRun.__new__(CellRun)
            cell.output = root / 'cell'; cell.output.mkdir()
            cell.env = dict(os.environ, CARGO_TARGET_DIR=str(root / 'target'),
                            RUSTC_WRAPPER='', RUSTC_WORKSPACE_WRAPPER='',
                            GMS_CACHE_MODE='off', SCCACHE_DISABLE='1')
            cell.commands = []
            published = io.StringIO()
            with contextlib.redirect_stdout(published), self.assertRaises(ValueError):
                cell.command('mobile-package', [sys.executable, child])
            diagnostic = cell.commands[-1]['diagnostics']
            self.assertIn('E0425', diagnostic['error_codes'])
            self.assertIn('missing-name', diagnostic['reasons'])
            self.assertNotIn(private_name, published.getvalue())
            self.assertNotIn(private_name, json.dumps(cell.commands))
            self.assertNotIn(str(project), published.getvalue())
            self.assertIn(private_name, (cell.output / 'mobile-package.log').read_text())
            self.assertIn(private_name, (private_output / 'cargo-artifacts.jsonl').read_text()
                          + (private_output / 'cargo-stderr.log').read_text())
            self.assertFalse((private_output / 'native-static-libs.json').exists())

    @unittest.skipUnless(sys.platform in ('linux', 'darwin'), 'Unix native link fixture')
    def test_real_rust_staticlib_contract_links_a_c_consumer(self):
        rustc, cc, ar = (shutil.which(name) for name in ('rustc', 'cc', 'ar'))
        if not all((rustc, cc, ar)):
            self.skipTest('Rust and native C archive tools are required')
        with tempfile.TemporaryDirectory(prefix='ndi-static-link-contract-') as temporary:
            root = Path(temporary)
            fixture = root / 'native.c'
            fixture.write_text('int ndi_contract_value(void) { return 37; }\n')
            subprocess.run([cc, '-c', fixture, '-o', root / 'native.o'], check=True,
                           capture_output=True, text=True)
            subprocess.run([ar, 'rcs', root / 'libndi_contract_fixture.a', root / 'native.o'],
                           check=True, capture_output=True, text=True)
            source = root / 'probe.rs'
            source.write_text('#[link(name="ndi_contract_fixture")]\n'
                              'extern "C" { fn ndi_contract_value() -> i32; }\n'
                              '#[no_mangle] pub extern "C" fn rust_contract_value() -> i32 {\n'
                              'unsafe { ndi_contract_value() } }\n')
            library = root / 'libprobe.a'
            compiled = subprocess.run([rustc, '--crate-type=staticlib', '--crate-name=probe',
                                       '--print=native-static-libs', source, '-o', library],
                                      check=True, capture_output=True, text=True, timeout=60)
            flags = native_static_libraries(compiled.stderr)
            self.assertIn('-lndi_contract_fixture', flags)
            main = root / 'main.c'
            main.write_text('extern int rust_contract_value(void);\n'
                            'int main(void) { return rust_contract_value() == 37 ? 0 : 1; }\n')
            executable = root / 'consumer'
            command = [cc, main, library, '-L' + str(root)]
            # Omitting the discovered dependency must actually break linking.
            missing = subprocess.run([*command, *[f for f in flags if f != '-lndi_contract_fixture'],
                                      '-o', executable], capture_output=True, text=True)
            self.assertNotEqual(missing.returncode, 0)
            subprocess.run([*command, *flags, '-o', executable], check=True,
                           capture_output=True, text=True)
            subprocess.run([executable], check=True, capture_output=True, timeout=10)


if __name__ == '__main__':
    unittest.main()
