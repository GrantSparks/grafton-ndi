import errno
from pathlib import Path
import sys
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'offline'))
import check  # noqa: E402


def raising(error):
    def attempt(*_):
        raise error
    return attempt


def passing(*_):
    return None


class Outcome(unittest.TestCase):
    def test_only_an_immediate_local_refusal_counts_as_refused(self):
        self.assertEqual(check.outcome(passing), 'reached')
        for code in (errno.ENETUNREACH, errno.EHOSTUNREACH, errno.EPERM, errno.EACCES):
            with self.subTest(code=errno.errorcode[code]):
                self.assertEqual(check.outcome(raising(OSError(code, 'refused'))), 'refused')
        self.assertEqual(check.outcome(raising(TimeoutError('timed out'))), 'timed out')
        self.assertEqual(check.outcome(raising(OSError(errno.ETIMEDOUT, 'timed out'))), 'timed out')
        # A reset comes from the remote host, so the packet left the machine.
        self.assertEqual(check.outcome(raising(ConnectionRefusedError(errno.ECONNREFUSED, 'refused'))),
                         'failed with ConnectionRefusedError')


class Problems(unittest.TestCase):
    REFUSED = raising(OSError(errno.ENETUNREACH, 'unreachable'))

    def problems(self, online, *, connect=None, multicast=None, tcp=passing, udp=passing):
        with mock.patch.multiple(check, connect=connect or self.REFUSED, send_multicast=multicast or self.REFUSED,
                                 loopback_tcp=tcp, loopback_udp=udp):
            return check.problems(online)

    def test_offline_needs_working_loopback_and_every_exit_refused(self):
        self.assertEqual(self.problems(False), [])
        self.assertEqual(self.problems(False, udp=raising(OSError(errno.EPERM, 'denied'))),
                         ['IPv4 loopback UDP: refused'])
        self.assertEqual(self.problems(False, multicast=passing),
                         ['UDP to the mDNS group 224.0.0.251:5353 was not refused: reached'])

    def test_offline_rejects_a_timeout_and_a_single_reachable_address(self):
        self.assertEqual(self.problems(False, connect=raising(TimeoutError())),
                         [f'TCP to {host}:{port} was not refused: timed out' for host, port in check.EXTERNAL])

        def only_ipv6(address):
            if ':' not in address[0]:
                raise OSError(errno.ENETUNREACH, 'unreachable')
        self.assertEqual(self.problems(False, connect=only_ipv6),
                         ['TCP to 2606:4700:4700::1111:443 was not refused: reached'])

    def test_online_needs_a_public_address_and_the_multicast_group(self):
        self.assertEqual(self.problems(True, connect=passing, multicast=passing), [])
        self.assertEqual(self.problems(True, multicast=passing), ['no public IPv4 address was reached: refused, refused'])
        self.assertEqual(self.problems(True, connect=passing), ['the mDNS group was not reached: refused'])

        def only_second(address):
            if address != check.EXTERNAL[1]:
                raise TimeoutError()
        self.assertEqual(self.problems(True, connect=only_second, multicast=passing), [])


if __name__ == '__main__':
    unittest.main()
