"""Exercise simulator observation with real local child processes, without SDKs."""
import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from mobile import await_smoke_result
from run import CellRun


class MobileRuntimeTests(unittest.TestCase):
    def observe(self, value, *, exit_early=False, console_text='', success_after_running=False):
        with tempfile.TemporaryDirectory(prefix='ndi-observer-') as temporary:
            root = Path(temporary)
            result, console = root / 'result', root / 'console'
            console.write_text(console_text)
            source = ('from pathlib import Path\nimport time\n'
                      'result=Path(' + repr(str(result)) + ')\n'
                      'def publish(value):\n'
                      ' pending=result.with_suffix(".pending")\n'
                      ' pending.write_bytes(value)\n'
                      ' pending.replace(result)\n')
            if value is not None:
                source += 'publish(' + repr(value) + ')\n'
            if success_after_running:
                source += 'time.sleep(0.04)\npublish(b"0")\n'
            source += 'time.sleep(0 if ' + repr(exit_early) + ' else 20)\n'
            child = subprocess.Popen([sys.executable, '-c', source],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            diagnostics = io.StringIO()
            try:
                if exit_early:
                    child.wait(timeout=5)
                with contextlib.redirect_stderr(diagnostics):
                    try:
                        await_smoke_result(result, child, console, timeout=0.5, interval=0.01)
                        state = 'success'
                    except ValueError:
                        state = diagnostics.getvalue().strip()
                return state
            finally:
                if child.poll() is None:
                    child.terminate()
                child.wait(timeout=5)

    def test_waits_for_real_running_child_to_report_success(self):
        self.assertEqual(self.observe(b'running', success_after_running=True), 'success')

    def test_native_error_panic_and_incomplete_states_remain_failures(self):
        for value, expected in ((b'1', 'native-error'), (b'2', 'native-panic'),
                                (b'running', 'native-running'), (None, 'result-missing')):
            with self.subTest(value=value):
                self.assertEqual(self.observe(value), 'MOBILE_SMOKE_STATE=' + expected)

    def test_early_exit_cannot_pass_even_with_success_file(self):
        for value in (None, b'running', b'0'):
            with self.subTest(value=value):
                self.assertEqual(self.observe(value, exit_early=True), 'MOBILE_SMOKE_STATE=app-exited')

    def test_private_invalid_oversized_and_non_ascii_results_are_rejected(self):
        for value in (b'private_sdk_payload', b'0' + b' ' * 32, b'\xff', b''):
            with self.subTest(value=value):
                self.assertEqual(self.observe(value), 'MOBILE_SMOKE_STATE=result-invalid')

    def test_explicit_result_write_failure_is_detected(self):
        self.assertEqual(self.observe(None, console_text='private payload\nMOBILE_SMOKE_STATE=result-write-failed\n'),
                         'MOBILE_SMOKE_STATE=result-write-failed')

    @unittest.skipUnless(os.name == 'posix', 'executable fake xcrun requires POSIX')
    def test_real_child_failure_reaches_only_private_cell_log_and_removes_stale_success(self):
        with tempfile.TemporaryDirectory(prefix='ndi-sim-boundary-') as temporary:
            root = Path(temporary)
            container = root / 'container'; (container / 'tmp').mkdir(parents=True)
            result = container / 'tmp/ndi-result'
            result.write_text('0')  # A preserved old data container must not pass.
            output = root / 'mobile'; output.mkdir()
            tools = root / 'bin'; tools.mkdir()
            executable = tools / 'xcrun'
            executable.write_text('#!' + sys.executable + '\n'
                'import os,signal,sys,time\nfrom pathlib import Path\n'
                'root=Path(' + repr(str(root)) + ')\n'
                'command=sys.argv[2]\n'
                'if command=="get_app_container": print(root/"container")\n'
                'elif command=="launch":\n'
                ' (root/"launch.pid").write_text(str(os.getpid()))\n'
                ' if (root/"container/tmp/ndi-result").exists(): sys.exit(42)\n'
                ' print("NDI smoke failed: private_vendor_payload", flush=True)\n'
                ' pending=root/"container/tmp/ndi-result.pending"\n'
                ' pending.write_text("1")\n'
                ' pending.replace(root/"container/tmp/ndi-result")\n'
                ' time.sleep(20)\n'
                'elif command=="terminate":\n'
                ' os.kill(int((root/"launch.pid").read_text()), signal.SIGTERM)\n')
            executable.chmod(0o700)
            child = root / 'child.py'
            child.write_text('from pathlib import Path\nimport sys\n'
                'sys.path.insert(0,' + repr(str(Path(__file__).resolve().parents[1])) + ')\n'
                'from mobile import execute_apple_simulator\n'
                'execute_apple_simulator({"udid":"test-device","state":"Booted"},'
                'Path("unused.app"),Path(' + repr(str(output)) + '))\n')
            cell = CellRun.__new__(CellRun)
            cell.output = root / 'cell'; cell.output.mkdir()
            cell.env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ['PATH'])
            cell.commands = []
            public = io.StringIO()
            with contextlib.redirect_stdout(public), self.assertRaises(ValueError):
                cell.command('mobile-package', [sys.executable, child])
            reasons = cell.commands[-1]['diagnostics']['reasons']
            self.assertIn('mobile-smoke-native-error', reasons)
            self.assertIn('mobile-phase-result', reasons)
            self.assertNotIn('private_vendor_payload', public.getvalue())
            self.assertNotIn('private_vendor_payload', json.dumps(cell.commands))
            self.assertNotIn(str(container), public.getvalue())
            self.assertIn('private_vendor_payload', (cell.output / 'mobile-package.log').read_text())
            self.assertIn('private_vendor_payload', (output / 'simulator-console.log').read_text())
            self.assertEqual(result.read_text(), '1')


if __name__ == '__main__':
    unittest.main()
