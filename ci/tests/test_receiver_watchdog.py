import contextlib
import io
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import receiver_watchdog  # noqa: E402


class ReceiverWatchdog(unittest.TestCase):
    def run_child(self, code, **timeouts):
        stream = io.BytesIO()
        output = io.TextIOWrapper(stream, write_through=True)
        with contextlib.redirect_stdout(output), patch.object(receiver_watchdog, "dump_threads") as dump:
            status = receiver_watchdog.supervise([sys.executable, "-c", code], system="Darwin", **timeouts)
        return status, stream.getvalue(), dump.call_count

    def test_keeps_the_exit_code_and_output(self):
        result = self.run_child("print('ordinary failure'); raise SystemExit(13)", suite_timeout=3)
        self.assertEqual(result, (13, b"ordinary failure\n", 0))

    def test_reports_a_stalled_overlap_suite(self):
        status, output, dumps = self.run_child("import time; time.sleep(20)", suite_timeout=0.2)
        self.assertEqual((status, dumps), (124, 1))
        self.assertIn(b"receiver overlap suite exceeded", output)

    def test_bounds_reconnect_even_if_the_rest_of_the_binary_has_no_deadline(self):
        code = f"import time; print({receiver_watchdog.BEGIN.decode()!r}, flush=True); time.sleep(20)"
        status, output, dumps = self.run_child(code, reconnect_timeout=0.1)
        self.assertEqual((status, dumps), (124, 1))
        self.assertIn(b"reconnect case exceeded", output)

    def test_a_finished_reconnect_disarms_its_deadline(self):
        code = (
            f"import time; print({receiver_watchdog.BEGIN.decode()!r}, flush=True); "
            f"print({receiver_watchdog.END.decode()!r}, flush=True); time.sleep(0.3)"
        )
        self.assertEqual(self.run_child(code, reconnect_timeout=0.1)[::2], (0, 0))
