"""Exercise actual Clang Android target selection without vendor headers or NDK."""
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from common import profiles
from run import CellRun


class AndroidTargetTests(unittest.TestCase):
    def test_driver_and_bindgen_agree_on_api24_without_redefining_builtins(self):
        clang = shutil.which('clang')
        if clang is None:
            self.skipTest('real Clang is required for this target preprocessing regression')
        with tempfile.TemporaryDirectory(prefix='ndi-android-target-') as temporary:
            ndk = Path(temporary)
            tools = ndk/'toolchains/llvm/prebuilt/linux-x86_64'
            (tools/'bin').mkdir(parents=True)
            (tools/'sysroot').mkdir()
            for profile_id, stem, pointer_size in (
                    ('android-arm64', 'aarch64-linux-android', 8),
                    ('android-armv7', 'armv7a-linux-androideabi', 4),
                    ('android-x86', 'i686-linux-android', 4),
                    ('android-x86_64', 'x86_64-linux-android', 8)):
                with self.subTest(profile=profile_id):
                    linker = tools/'bin'/(stem+'24-clang')
                    linker.touch()
                    profile = profiles()[profile_id]
                    cell = CellRun.__new__(CellRun)
                    cell.env = {'ANDROID_NDK_HOME': str(ndk)}
                    cell.clang_args = []
                    with mock.patch('run.platform.system', return_value='Linux'):
                        cell.configure_target(profile)
                    self.assertEqual(cell.abi_cc, str(linker))
                    self.assertEqual(cell.env['CARGO_TARGET_'+profile['target'].upper().replace('-', '_')+'_LINKER'], str(linker))
                    bindgen = shlex.split(cell.env['BINDGEN_EXTRA_CLANG_ARGS_'+profile['target'].replace('-', '_')])
                    source = (f'#if __ANDROID_API__ != 24 || __ANDROID_MIN_SDK_VERSION__ != 24\n'
                              '#error wrong Android API\n#endif\n'
                              f'#if __SIZEOF_POINTER__ != {pointer_size}\n#error wrong target ABI\n#endif\n'
                              'int independently_checked_android_target;\n')
                    # First emulate the NDK wrapper's target selection; then
                    # independently test the arguments passed to libclang alone.
                    for arguments in (['--target='+stem+'24', *cell.clang_args], bindgen):
                        result = subprocess.run([clang, *arguments, '-Werror', '-fsyntax-only', '-x', 'c', '-'],
                                                input=source, capture_output=True, text=True)
                        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == '__main__':
    unittest.main()
