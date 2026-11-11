"""Socket diagnostics are bounded observations, never native SDK qualification."""
import contextlib
import io
import json
from pathlib import Path
import socket
import sys
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import sandbox_probe as probe


class SandboxProbeTests(unittest.TestCase):
    def test_denied_creation_keeps_all_independent_scenarios_and_no_error_payload(self):
        with patch.object(probe.socket, 'socket', side_effect=PermissionError(1, 'PRIVATE_ERROR')), \
                patch.object(probe.socket, 'socketpair', side_effect=PermissionError(1, 'PRIVATE_ERROR')), \
                contextlib.redirect_stdout(io.StringIO()) as captured:
            probe.main()
        report = probe.validate_report(json.loads(captured.getvalue()), platform=sys.platform)
        self.assertEqual(len(report['operations']), 10)
        self.assertEqual({row['operation'] for row in report['operations']}, {
            family + '.' + operation for family in ('ipv4', 'ipv6')
            for operation in ('stream.create', 'datagram.create', 'tcp.listener-create', 'udp.receiver-create')
        } | {'unix.stream.create', 'unix.socketpair.create'})
        self.assertTrue(all(row['status'] == 'failed' and row['errno'] == 1 for row in report['operations']))
        self.assertNotIn('PRIVATE_ERROR', captured.getvalue())

    def test_local_address_failure_stays_inside_its_dependent_scenario(self):
        connection = Mock()
        connection.getsockname.side_effect = OSError(22, 'PRIVATE_ADDRESS')
        for function, prefix, stage in ((probe.tcp, 'ipv4.tcp', 'bind'),
                                       (probe.udp, 'ipv4.udp', 'receiver-bind')):
            probe.RESULTS.clear()
            with patch.object(probe, 'owned', return_value=connection), self.assertRaises(probe.StopScenario):
                function(socket.AF_INET, '127.0.0.1', prefix)
            self.assertEqual(probe.RESULTS[-1], {'operation': prefix + '.' + stage, 'status': 'failed', 'errno': 22})
            connection.connect.assert_not_called()

    def test_timeout_has_no_dynamic_error_or_invented_errno(self):
        probe.RESULTS.clear()
        def timeout():
            raise TimeoutError('PRIVATE_TIMEOUT')
        with self.assertRaises(probe.StopScenario):
            probe.step('ipv4.tcp.receive', timeout)
        self.assertEqual(probe.RESULTS, [{'operation': 'ipv4.tcp.receive', 'status': 'failed', 'errno': None}])


if __name__ == '__main__':
    unittest.main()
