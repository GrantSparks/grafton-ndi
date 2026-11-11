#!/usr/bin/env python3
"""Check what network this process can reach.

    python ci/offline/check.py --online          # control: the host can reach the internet
    ci/offline/linux.sh python3 ci/offline/check.py   # inside a runner: loopback only

Without --online the check passes only if IPv4 loopback TCP and UDP work and
no external address accepts a connection. With --online it passes only if an
external IPv4 address does, which shows the offline result is not vacuous.
"""
import argparse
import socket
import sys

EXTERNAL = (('1.1.1.1', 443), ('8.8.8.8', 53), ('2606:4700:4700::1111', 443))


def loopback_tcp():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(('127.0.0.1', 0))
        listener.listen(1)
        with socket.create_connection(listener.getsockname(), timeout=5) as client:
            server, _ = listener.accept()
            with server:
                client.sendall(b'tcp')
                return server.recv(3) == b'tcp'


def loopback_udp():
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as receiver, \
            socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sender:
        receiver.bind(('127.0.0.1', 0))
        receiver.settimeout(5)
        sender.sendto(b'udp', receiver.getsockname())
        return receiver.recv(3) == b'udp'


def reachable(address):
    try:
        socket.create_connection(address, timeout=3).close()
    except OSError:
        return False
    return True


def problems(online):
    if online:
        return [] if any(reachable(address) for address in EXTERNAL[:2]) else ['no external IPv4 address is reachable']
    found = []
    for name, probe in (('TCP', loopback_tcp), ('UDP', loopback_udp)):
        try:
            working = probe()
        except OSError as error:
            working = False
            name = f'{name} ({error})'
        if not working:
            found.append(f'IPv4 loopback {name} does not work')
    found += [f'{host}:{port} is reachable' for host, port in EXTERNAL if reachable((host, port))]
    return found


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--online', action='store_true', help='expect external connectivity instead')
    online = parser.parse_args().online
    found = problems(online)
    for problem in found:
        print(f'network check failed: {problem}', file=sys.stderr)
    if not found:
        print('online: an external address is reachable' if online
              else 'offline: loopback works and no external address is reachable')
    return 1 if found else 0


if __name__ == '__main__':
    raise SystemExit(main())
