#!/usr/bin/env python3
"""Check what network this process can reach.

    python ci/offline/check.py --online               # control: outside a test runner
    ci/offline/linux.sh python3 ci/offline/check.py   # inside one

Inside a runner the check passes only if IPv4 loopback TCP and UDP work and the
operating system refuses, at once, every attempt to leave the machine: a TCP
connection to each of three public addresses, and a UDP datagram to the mDNS
multicast group that NDI discovery uses. An attempt that times out, or that a
remote host answers in any way, does not count as refused.

With --online the same attempts must succeed, which shows that a refusal inside
the runner is the runner's doing and not the state of the machine.

Only the calling process is exercised. What each runner confines beyond that,
and what it does not, is described in ci/README.md.
"""
import argparse
import errno
import socket
import sys

EXTERNAL = (('1.1.1.1', 443), ('8.8.8.8', 53), ('2606:4700:4700::1111', 443))
MDNS = ('224.0.0.251', 5353)
# How an operating system refuses traffic it will not carry: there is no route
# (a network namespace) or no permission (a sandbox profile, a firewall rule).
REFUSED = {errno.ENETUNREACH, errno.EHOSTUNREACH, errno.EPERM, errno.EACCES}


def outcome(attempt):
    """Run `attempt`; return 'reached', 'refused', or what else happened."""
    try:
        attempt()
    except OSError as error:
        if error.errno in REFUSED:
            return 'refused'
        return 'timed out' if isinstance(error, TimeoutError) else f'failed with {type(error).__name__}'
    return 'reached'


def connect(address):
    socket.create_connection(address, timeout=5).close()


def send_multicast():
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sender:
        sender.sendto(b'\0', MDNS)


def loopback_tcp():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(('127.0.0.1', 0))
        listener.listen(1)
        with socket.create_connection(listener.getsockname(), timeout=5) as client:
            server, _ = listener.accept()
            with server:
                client.sendall(b'tcp')
                if server.recv(3) != b'tcp':
                    raise OSError('loopback TCP delivered other bytes')


def loopback_udp():
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as receiver, \
            socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sender:
        receiver.bind(('127.0.0.1', 0))
        receiver.settimeout(5)
        sender.sendto(b'udp', receiver.getsockname())
        if receiver.recv(3) != b'udp':
            raise OSError('loopback UDP delivered other bytes')


def problems(online):
    """What differs from the expected state: everything reachable online, loopback only otherwise."""
    leaving = {f'TCP to {host}:{port}': outcome(lambda address=(host, port): connect(address))
               for host, port in EXTERNAL}
    leaving[f'UDP to the mDNS group {MDNS[0]}:{MDNS[1]}'] = outcome(send_multicast)
    if online:
        # One public IPv4 address is enough, and a build machine need not have IPv6.
        *tcp, udp = leaving.values()
        found = [] if 'reached' in tcp[:2] else ['no public IPv4 address was reached: ' + ', '.join(tcp[:2])]
        return found if udp == 'reached' else found + [f'the mDNS group was not reached: {udp}']
    found = [f'IPv4 loopback {name}: {result}' for name, result in
             (('TCP', outcome(loopback_tcp)), ('UDP', outcome(loopback_udp))) if result != 'reached']
    return found + [f'{name} was not refused: {result}' for name, result in leaving.items() if result != 'refused']


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--online', action='store_true', help='expect the network to be reachable instead')
    online = parser.parse_args(argv).online
    found = problems(online)
    for problem in found:
        print(f'network check failed: {problem}', file=sys.stderr)
    if not found:
        print('online: a public address and the mDNS group can be reached' if online
              else 'offline: loopback works and every attempt to leave the machine is refused')
    return 1 if found else 0


if __name__ == '__main__':
    raise SystemExit(main())
