#!/usr/bin/env python3
"""Diagnostic only: fixed loopback/anonymous operations, never a release gate."""
import contextlib
import json
import socket
import sys

TIMEOUT_SECONDS = 0.5
RESULTS = []
OPERATIONS = frozenset(
    [family + '.' + operation for family in ('ipv4', 'ipv6') for operation in (
        'stream.create', 'datagram.create', 'tcp.listener-create', 'tcp.bind',
        'tcp.listen', 'tcp.client-create', 'tcp.client-bind', 'tcp.connect',
        'tcp.accept', 'tcp.send', 'tcp.receive', 'udp.receiver-create',
        'udp.receiver-bind', 'udp.sender-create', 'udp.sender-bind', 'udp.send',
        'udp.receive')]
    + ['unix.stream.create', 'unix.socketpair.create', 'unix.socketpair.send',
       'unix.socketpair.receive'])


def validate_report(report, *, platform='darwin'):
    """Accept only the closed diagnostic vocabulary; no child strings escape."""
    if (not isinstance(report, dict) or set(report) != {
            'schema', 'diagnostic_only', 'platform', 'timeout_seconds', 'operations'}
            or type(report['schema']) is not int or report['schema'] != 1
            or report['diagnostic_only'] is not True or report['platform'] != platform
            or type(report['timeout_seconds']) is not float
            or report['timeout_seconds'] != TIMEOUT_SECONDS
            or not isinstance(report['operations'], list)
            or not 0 < len(report['operations']) <= len(OPERATIONS)):
        raise ValueError('invalid sandbox diagnostic report')
    seen = set()
    for row in report['operations']:
        if (not isinstance(row, dict) or set(row) != {'operation', 'status', 'errno'}
                or not isinstance(row['operation'], str) or row['operation'] not in OPERATIONS
                or row['operation'] in seen or row['status'] not in ('passed', 'failed', 'internal-error')
                or not (row['errno'] is None or type(row['errno']) is int and 0 < row['errno'] < 4096)
                or row['status'] != 'failed' and row['errno'] is not None):
            raise ValueError('invalid sandbox diagnostic operation')
        seen.add(row['operation'])
    return report


class StopScenario(Exception):
    pass


def step(operation, action):
    try:
        value = action()
    except OSError as error:
        RESULTS.append({"operation": operation, "status": "failed", "errno": error.errno})
        raise StopScenario from None
    except Exception:
        RESULTS.append({"operation": operation, "status": "internal-error", "errno": None})
        raise StopScenario from None
    RESULTS.append({"operation": operation, "status": "passed", "errno": None})
    return value


def check(condition):
    if not condition:
        raise RuntimeError("unexpected local result")


def owned(stack, family, kind):
    connection = stack.enter_context(socket.socket(family, kind))
    connection.settimeout(TIMEOUT_SECONDS)
    return connection


def create_socket(family, kind, prefix):
    with contextlib.ExitStack() as stack:
        step(prefix + ".create", lambda: owned(stack, family, kind))


def bind_address(connection, host):
    connection.bind((host, 0))
    return connection.getsockname()


def tcp(family, host, prefix):
    with contextlib.ExitStack() as stack:
        listener = step(prefix + ".listener-create", lambda: owned(stack, family, socket.SOCK_STREAM))
        address = step(prefix + ".bind", lambda: bind_address(listener, host))
        step(prefix + ".listen", lambda: listener.listen(1))
        client = step(prefix + ".client-create", lambda: owned(stack, family, socket.SOCK_STREAM))
        step(prefix + ".client-bind", lambda: client.bind((host, 0)))
        step(prefix + ".connect", lambda: client.connect(address))

        def accept():
            peer, address = listener.accept()
            stack.enter_context(peer)
            peer.settimeout(TIMEOUT_SECONDS)
            check(address[0] == host)
            return peer

        peer = step(prefix + ".accept", accept)
        step(prefix + ".send", lambda: client.sendall(b"p"))
        step(prefix + ".receive", lambda: check(peer.recv(1) == b"p"))


def udp(family, host, prefix):
    with contextlib.ExitStack() as stack:
        receiver = step(prefix + ".receiver-create", lambda: owned(stack, family, socket.SOCK_DGRAM))
        address = step(prefix + ".receiver-bind", lambda: bind_address(receiver, host))
        sender = step(prefix + ".sender-create", lambda: owned(stack, family, socket.SOCK_DGRAM))
        step(prefix + ".sender-bind", lambda: sender.bind((host, 0)))
        step(prefix + ".send", lambda: check(sender.sendto(b"p", address) == 1))

        def receive():
            data, peer = receiver.recvfrom(1)
            check(data == b"p" and peer[0] == host)

        step(prefix + ".receive", receive)


def anonymous_pair():
    with contextlib.ExitStack() as stack:
        def create():
            left, right = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
            stack.enter_context(left)
            stack.enter_context(right)
            left.settimeout(TIMEOUT_SECONDS)
            right.settimeout(TIMEOUT_SECONDS)
            return left, right

        left, right = step("unix.socketpair.create", create)
        step("unix.socketpair.send", lambda: left.sendall(b"p"))
        step("unix.socketpair.receive", lambda: check(right.recv(1) == b"p"))


def main():
    RESULTS.clear()
    scenarios = []
    for label, family, host in (("ipv4", socket.AF_INET, "127.0.0.1"), ("ipv6", socket.AF_INET6, "::1")):
        scenarios += [
            (label + ".stream", lambda f=family, p=label: create_socket(f, socket.SOCK_STREAM, p + ".stream")),
            (label + ".datagram", lambda f=family, p=label: create_socket(f, socket.SOCK_DGRAM, p + ".datagram")),
            (label + ".tcp", lambda f=family, h=host, p=label: tcp(f, h, p + ".tcp")),
            (label + ".udp", lambda f=family, h=host, p=label: udp(f, h, p + ".udp")),
        ]
    scenarios += [
        ("unix.stream", lambda: create_socket(socket.AF_UNIX, socket.SOCK_STREAM, "unix.stream")),
        ("unix.socketpair", anonymous_pair),
    ]
    for name, action in scenarios:
        try:
            action()
        except StopScenario:
            # Only dependent later steps in this scenario are omitted. Every
            # independent address family and scenario is still attempted.
            pass
    print(json.dumps({"schema": 1, "diagnostic_only": True, "platform": sys.platform,
                      "timeout_seconds": TIMEOUT_SECONDS, "operations": RESULTS}, sort_keys=True))


if __name__ == "__main__":
    main()
