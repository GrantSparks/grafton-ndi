#!/usr/bin/env python3
"""Bounded native SDK execution on disposable macOS CI hosts.

An independent root guardian owns the temporary PF transaction. Known tests and
probes run as the caller under Seatbelt. SDK acquisition, compilation, and source
provenance remain in the existing CellRun/batch boundaries. Raw host details are
kept in ephemeral scratch; only a closed receipt is returned to aggregation.
"""
from __future__ import annotations

import argparse
import http.client
import hashlib
import math
import shutil
import tempfile
import json
import os
from pathlib import Path
import re
import select
import selectors
import signal
import socket
import ssl
import subprocess
import sys
import time

DRIVER_SECONDS = 120
COMMAND_SECONDS = 4
HOST = 'api.github.com'
LABEL = 'ndi-ci-offline-block'
DEFAULT_SCRUB = 'scrub-anchor "com.apple/*" all fragment reassemble'
DEFAULT_FILTER = 'anchor "com.apple/*" all'
APPLE_LEAVES = ('com.apple/200.AirDrop', 'com.apple/250.ApplicationFirewall')
APPLE_FILTER_LINKS = ('anchor "200.AirDrop/*" all', 'anchor "250.ApplicationFirewall/*" all')
ALTQ_NOTICES = frozenset(('No ALTQ support in kernel', 'ALTQ related functions disabled'))
RULES = f'pass quick on lo0 all no state\nblock drop quick all label "{LABEL}"\n'
SAFE_ENV = {'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'LANG': 'C', 'LC_ALL': 'C'}


def write_json(path, value, uid=None, gid=None):
    temporary = path.with_name(path.name + '.tmp')
    with temporary.open('w', encoding='utf-8') as stream:
        json.dump(value, stream, sort_keys=True, indent=2)
        stream.write('\n')
    os.chmod(temporary, 0o600)
    if uid is not None:
        os.chown(temporary, uid, gid)
    temporary.replace(path)


def normalized_rules(text):
    lines = []
    for line in text.splitlines():
        line = line.strip()
        if line:
            lines.append(' '.join(line.split()))
    return lines


def known_initial_rules(text):
    return normalized_rules(text) in ([], [DEFAULT_FILTER], [DEFAULT_SCRUB, DEFAULT_FILTER])


def preserved_scrub_rules(original):
    if not known_initial_rules(original):
        raise ValueError('initial root filter is not empty/default')
    return [DEFAULT_SCRUB] if normalized_rules(original)[:1] == [DEFAULT_SCRUB] else []


def offline_policy(original):
    # Hosted macOS readback confirms that -R replaces both scrub and filter rules.
    # Intentionally reload the exact reviewed scrub prefix in the transaction.
    return ''.join(line + '\n' for line in preserved_scrub_rules(original)) + RULES


def original_policy(original):
    preserved_scrub_rules(original)  # Refuse an unreviewed restoration input.
    return ''.join(line + '\n' for line in normalized_rules(original))


def expected_active_rules(original):
    # Verbose dry-run prints a short anchor name (observed as "/*"). It is not
    # installed-rule evidence. Only literal canonical actual readback qualifies.
    return preserved_scrub_rules(original) + normalized_rules(RULES)


def validate_rule_readback(actual, expected):
    if normalized_rules(actual) != expected:
        raise ValueError('actual scrub/filter rules differ from the exact expected policy')


def known_initial_translations(text):
    # The -R scrub/filter transaction does not select NAT/RDR. A translation rule
    # with "pass" can bypass the root filter, so allow only these empty/default
    # anchor calls and separately require the entire referenced subtree empty.
    return normalized_rules(text) in ([], [
        'nat-anchor "com.apple/*" all',
        'rdr-anchor "com.apple/*" all',
    ])


def validate_initial_translations(root, child):
    if not known_initial_translations(root):
        raise ValueError('initial root translation rules are not empty/default')
    if child.strip():
        raise ValueError('initial parent translation rules are not empty')


def expected_leaf_names(descendants):
    names = tuple(line.strip() for line in descendants.splitlines() if line.strip())
    if names not in ((), APPLE_LEAVES):
        raise ValueError('initial Apple anchor names are not the exact reviewed topology')
    return names


def validate_apple_topology(parent_rules, descendants, leaves):
    names = expected_leaf_names(descendants)
    expected_links = list(APPLE_FILTER_LINKS) if names else []
    if normalized_rules(parent_rules) != expected_links or set(leaves) != set(names):
        raise ValueError('initial Apple anchor links or leaf evidence differ from the reviewed topology')
    for name in names:
        leaf = leaves[name]
        if (set(leaf) != {'rules', 'translations', 'descendants'}
                or any(value.strip() for value in leaf.values())):
            raise ValueError('initial Apple leaf anchor is not empty')


def validate_query_output(result):
    # macOS can report DIOCGETRULES failures on stderr while returning exit0.
    # Only the exact ubiquitous ALTQ notices are benign for these proof queries.
    if (result['exit_code'] != 0 or result['timed_out']
            or result.get('output_truncated', False)):
        raise ValueError('PF proof query did not complete successfully')
    if any(line.strip() not in ALTQ_NOTICES for line in result['stderr'].splitlines() if line.strip()):
        raise ValueError('PF proof query reported an unexpected diagnostic')


def pf_enabled(text):
    found = re.findall(r'^Status:\s+(Enabled|Disabled)\b', text, re.MULTILINE)
    if len(found) != 1:
        raise ValueError('unrecognized PF status')
    return found[0] == 'Enabled'


def parse_interfaces(text, actual):
    """Conservative pfctl verbose interface reader: unknown syntax refuses."""
    observed = {}
    for line in text.splitlines():
        if not line.strip() or line[0].isspace():
            continue
        match = re.fullmatch(r'([A-Za-z][A-Za-z0-9_.:-]*)(?:\s+\(([^()]*)\))?', line.strip())
        if not match:
            raise ValueError('unrecognized PF interface record')
        name, flags = match.groups()
        if name in observed:
            raise ValueError('duplicate PF interface record')
        parts = set((flags or '').replace(',', ' ').split())
        if parts - {'skip'}:
            raise ValueError('unrecognized PF interface flags')
        observed[name] = 'skip' in parts
    if not actual or not set(actual).issubset(observed):
        raise ValueError('PF does not expose every current interface')
    if any(skip and name != 'lo0' for name, skip in observed.items()):
        raise ValueError('non-loopback PF skip flag')
    return observed


def block_packets(text):
    lines = text.splitlines()
    markers = [index for index, line in enumerate(lines) if f'label "{LABEL}"' in line]
    if len(markers) != 1:
        raise ValueError('missing unique blocking rule counter')
    for line in lines[markers[0] + 1:markers[0] + 5]:
        match = re.search(r'\bPackets:\s*(\d+)\b', line)
        if match:
            return int(match.group(1))
    raise ValueError('unrecognized blocking rule counter')


def enable_token(text):
    tokens = re.findall(r'\bToken\s*:\s*(\d+)\b', text)
    if len(tokens) != 1 or int(tokens[0]) <= 0:
        raise ValueError('PF enable did not return one owned token')
    return tokens[0]


def https_socket(address, timeout=3):
    connection = socket.create_connection((address, 443), timeout=timeout)
    try:
        return ssl.create_default_context().wrap_socket(connection, server_hostname=HOST)
    except BaseException:
        connection.close()
        raise


def valid_http_status(status):
    return type(status) is int and 200 <= status <= 599


def head(connection):
    connection.sendall((f'HEAD / HTTP/1.1\r\nHost: {HOST}\r\n'
                        'User-Agent: ndi-ci-network-check\r\n'
                        'Connection: keep-alive\r\n\r\n').encode('ascii'))
    response = http.client.HTTPResponse(connection, method='HEAD')
    response.begin()
    status = response.status
    response.read()
    # Closing this file wrapper does not close the underlying SSL socket.
    response.close()
    if not valid_http_status(status):
        raise ValueError(f'GitHub canary did not return a final HTTP response: {status}')
    return status


def offline_https_attempt(connection):
    try:
        return False, head(connection)
    except TimeoutError:
        return True, None


def offline_connect_attempt(address):
    try:
        with socket.create_connection((address, 443), timeout=1.5):
            pass
    except TimeoutError:
        return True
    return False


def loopback():
    rows = []
    for family, address, name in ((socket.AF_INET, '127.0.0.1', 'ipv4'),
                                  (socket.AF_INET6, '::1', 'ipv6')):
        with socket.socket(family, socket.SOCK_STREAM) as listener:
            listener.settimeout(1)
            listener.bind((address, 0))
            listener.listen(1)
            with socket.socket(family, socket.SOCK_STREAM) as client:
                client.settimeout(1)
                client.connect(listener.getsockname())
                peer, _ = listener.accept()
                with peer:
                    peer.settimeout(1)
                    client.sendall(b'P')
                    if peer.recv(1) != b'P':
                        raise ValueError('loopback TCP payload mismatch')
                    peer.sendall(b'Q')
                    if client.recv(1) != b'Q':
                        raise ValueError('loopback TCP reply mismatch')
            rows.append(name + '.tcp')
        with socket.socket(family, socket.SOCK_DGRAM) as receiver:
            receiver.settimeout(1)
            receiver.bind((address, 0))
            with socket.socket(family, socket.SOCK_DGRAM) as sender:
                sender.settimeout(1)
                sender.bind((address, 0))
                sender.sendto(b'P', receiver.getsockname())
                data, remote = receiver.recvfrom(1)
                if data != b'P':
                    raise ValueError('loopback UDP payload mismatch')
                receiver.sendto(b'Q', remote)
                if sender.recvfrom(1)[0] != b'Q':
                    raise ValueError('loopback UDP reply mismatch')
            rows.append(name + '.udp')
    return rows


def canary_child(address):
    """Trusted fixture only; guardian drops privileges before executing it."""
    if os.getuid() == 0 or os.geteuid() == 0:
        raise ValueError('canary must be unprivileged')
    with https_socket(address) as established:
        initial_status = head(established)
        print(json.dumps({'phase': 'ready', 'uid': os.geteuid(), 'peer': address,
                          'http_status': initial_status}), flush=True)
        if sys.stdin.readline().strip() != 'offline':
            raise ValueError('missing offline barrier')
        established.settimeout(1.5)
        established_blocked, received_status = offline_https_attempt(established)
        fresh_blocked = offline_connect_attempt(address)
        rows = loopback()
        print(json.dumps({'phase': 'offline', 'established_blocked': established_blocked,
                          'established_http_status': received_status,
                          'fresh_blocked': fresh_blocked, 'loopback': rows}), flush=True)
        # Stay alive until the guardian deliberately terminates and reaps us.
        sys.stdin.readline()


class Commands:
    def __init__(self, output, uid, gid):
        self.output, self.uid, self.gid = output, uid, gid
        self.index = 0
        self.active = None

    def _kill_and_reap(self):
        process = self.active
        if process is None:
            return '', ''
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        # Keep the active marker if this second, bounded wait also fails.
        # No subsequent command (especially restoration) may race it.
        output = process.communicate(timeout=2)
        self.active = None
        return output

    def reap_active(self):
        self._kill_and_reap()

    def run(self, argv, timeout=COMMAND_SECONDS, check=True, output_limit=None):
        if self.active is not None:
            raise RuntimeError('previous local command has not been reaped')
        self.index += 1
        process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, text=True, env=SAFE_ENV,
                                   start_new_session=True, close_fds=True)
        self.active = process
        timed_out = False
        try:
            stdout, stderr = process.communicate(timeout=timeout)
        except BaseException as error:
            # An alarm/cancellation must not leave a delayed pfctl mutation
            # racing the guardian's restoration commands.
            try:
                stdout, stderr = self._kill_and_reap()
            except BaseException as reap_error:
                write_json(self.output / f'command-{self.index:03d}.log',
                           {'argv': argv, 'command_reaped': False,
                            'interrupted': type(error).__name__,
                            'reap_error': type(reap_error).__name__}, self.uid, self.gid)
                raise RuntimeError('local command could not be reaped') from reap_error
            timed_out = isinstance(error, subprocess.TimeoutExpired)
            if not timed_out:
                write_json(self.output / f'command-{self.index:03d}.log',
                           {'argv': argv, 'exit_code': process.returncode,
                            'interrupted': type(error).__name__, 'stdout': stdout,
                            'stderr': stderr}, self.uid, self.gid)
                raise
        else:
            self.active = None
        record = {'argv': argv, 'exit_code': process.returncode, 'timed_out': timed_out,
                  'stdout': stdout, 'stderr': stderr}
        if output_limit is not None:
            record['output_truncated'] = len(stdout) > output_limit or len(stderr) > output_limit
            record['stdout'] = stdout[:output_limit]
            record['stderr'] = stderr[:output_limit]
        write_json(self.output / f'command-{self.index:03d}.log', record, self.uid, self.gid)
        if check and (timed_out or process.returncode != 0):
            raise RuntimeError('bounded local command failed')
        return record

    def pf(self, *args, **kwargs):
        result = self.run(['/sbin/pfctl', *args], **kwargs)
        if '-s' in args or any(flag in args for flag in ('-sr', '-sn', '-ss', '-vvsr')):
            validate_query_output(result)
        return result

    def interface_snapshot(self):
        actual = self.run(['/sbin/ifconfig', '-l'])['stdout'].split()
        return parse_interfaces(self.pf('-s', 'Interfaces', '-v')['stdout'], actual)



def child_row(child, seconds):
    ready, _, _ = select.select([child.stdout], [], [], seconds)
    if not ready:
        raise TimeoutError('canary response deadline exceeded')
    line = child.stdout.readline(8193)
    if not line or len(line) > 8192:
        raise ValueError('invalid canary response')
    return json.loads(line)


def stop_child(child):
    if child is None:
        return
    if child.poll() is None:
        os.killpg(child.pid, signal.SIGTERM)
        try:
            child.wait(timeout=1)
        except subprocess.TimeoutExpired:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait(timeout=2)
    # Trusted fixture has no descendants. Do not restore while its group exists.
    try:
        os.killpg(child.pid, 0)
    except ProcessLookupError:
        return
    raise RuntimeError('canary process group remains alive')


def guardian(output, uid, gid, address, request_path):
    if os.geteuid() != 0 or uid <= 0 or gid < 0 or os.getppid() != 1:
        raise ValueError('guardian must be root, detached and reparented before mutation')
    commands = Commands(output, uid, gid)
    report = {'schema': 1, 'status': 'preparing', 'stage': 'setup', 'cleanup': {},
              'guardian_pid': os.getpid(), 'guardian_parent_pid': os.getppid(),
              'external_canary_families': ['ipv4'],
              'ipv6_external_canary': 'not-executed-in-this-first-stage'}
    child = None
    worker_child = None
    worker_log = None
    token = None
    restore_needed = False
    original = None
    sealed_at = None
    policy_path = output / 'offline-policy.conf'
    original_path = output / 'original-filter.conf'
    child_error = (output / 'canary-stderr.log').open('w')
    os.chown(output / 'canary-stderr.log', uid, gid)
    def interrupted(signum, _frame):
        raise InterruptedError(f'guardian interrupted by signal {signum}')
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    # Hard local ceiling covers every normal setup/probe operation, independent
    # of the workflow parent. Cleanup uses per-command timeouts after disarming.
    signal.signal(signal.SIGALRM, interrupted)
    signal.alarm(65)
    try:
        request = validate_request(request_path)
        report['operation'] = request['operation']
        report['binary_sha256'] = request['binary_sha256']
        initial_info = commands.pf('-s', 'info')['stdout']
        if pf_enabled(initial_info):
            raise ValueError('initial PF is enabled; baseline is not reviewed')
        original = commands.pf('-sr')['stdout']
        if not known_initial_rules(original):
            raise ValueError('initial root filter is not empty/default')
        validate_initial_translations(
            commands.pf('-sn')['stdout'], commands.pf('-a', 'com.apple', '-sn')['stdout'])
        parent_rules = commands.pf('-a', 'com.apple', '-sr')['stdout']
        descendants = commands.pf('-a', 'com.apple', '-s', 'Anchors')['stdout']
        leaves = {}
        for name in expected_leaf_names(descendants):
            leaves[name] = {
                'rules': commands.pf('-a', name, '-sr')['stdout'],
                'translations': commands.pf('-a', name, '-sn')['stdout'],
                'descendants': commands.pf('-a', name, '-s', 'Anchors')['stdout'],
            }
        validate_apple_topology(parent_rules, descendants, leaves)
        for args in (('-ss',), ('-s', 'Tables')):
            if commands.pf(*args)['stdout'].strip():
                raise ValueError('initial PF has unreviewed state, tables or child rules')
        commands.pf('-s', 'References')  # Private audit; Disabled is the gate.
        report['initial_interfaces'] = commands.interface_snapshot()
        route = commands.run(['/sbin/route', '-n', 'get', '-inet', address])['stdout']
        match = re.search(r'^\s*interface:\s*(\S+)\s*$', route, re.MULTILINE)
        if not match or match[1] == 'lo0' or match[1] not in report['initial_interfaces']:
            raise ValueError('GitHub canary route is not verified non-loopback')
        report['canary_interface'] = match[1]
        original_path.write_text(original_policy(original), encoding='utf-8')
        policy_path.write_text(offline_policy(original), encoding='utf-8')
        commands.pf('-n', '-R', '-f', str(original_path))
        commands.pf('-n', '-R', '-f', str(policy_path))
        expected = expected_active_rules(original)
        def demote():
            os.setgroups([])
            os.setgid(gid)
            os.setuid(uid)
        child = subprocess.Popen([sys.executable, '-I', str(Path(__file__).resolve()),
                                  '--canary-child', address], stdin=subprocess.PIPE,
                                 stdout=subprocess.PIPE, stderr=child_error, text=True,
                                 env=SAFE_ENV, start_new_session=True, close_fds=True,
                                 preexec_fn=demote)
        ready = child_row(child, 12)
        if (not isinstance(ready, dict)
                or set(ready) != {'phase', 'uid', 'peer', 'http_status'}
                or ready['phase'] != 'ready' or ready['uid'] != uid or ready['peer'] != address
                or not valid_http_status(ready['http_status'])):
            raise ValueError('unprivileged external canary was not established')
        report['canary_initial_http_status'] = ready['http_status']
        report['stage'] = 'barrier'
        enabled = commands.pf('-E', check=False)
        # Capture ownership even when pfctl reports an error; cleanup never uses -d.
        token = enable_token(enabled['stdout'] + enabled['stderr'])
        if enabled['exit_code'] or enabled['timed_out']:
            raise RuntimeError('PF enable command failed')
        restore_needed = True  # Intent recorded before a possibly partial command.
        sealed_at = time.monotonic()
        signal.alarm(OFFLINE_SECONDS)
        commands.pf('-R', '-f', str(policy_path))
        validate_rule_readback(commands.pf('-sr')['stdout'], expected)
        # Deliberate whole-disposable-host state purge, AFTER blocker commit.
        commands.pf('-F', 'states')
        if commands.pf('-ss')['stdout'].strip():
            raise ValueError('PF states remain after the global purge')
        commands.interface_snapshot()
        if not pf_enabled(commands.pf('-s', 'info')['stdout']):
            raise ValueError('PF stopped before offline barrier')
        before = block_packets(commands.pf('-vvsr')['stdout'])
        report['phase'] = 'offline'
        write_json(output / 'checkpoint.log', report, uid, gid)
        child.stdin.write('offline\n')
        child.stdin.flush()
        observed = child_row(child, 6)
        report['canary'] = observed
        required = {'phase': 'offline', 'established_blocked': True, 'fresh_blocked': True,
                    'established_http_status': None,
                    'loopback': ['ipv4.tcp', 'ipv4.udp', 'ipv6.tcp', 'ipv6.udp']}
        if observed != required:
            raise ValueError('offline canary or loopback operation failed')
        after = block_packets(commands.pf('-vvsr')['stdout'])
        if after <= before:
            raise ValueError('blocked connections produced no PF block-counter evidence')
        report['block_packets_delta'] = after - before
        report['stage'] = 'worker'
        worker_child, worker_log = launch_worker(request_path, output, uid, gid)
        worker_deadline = time.monotonic() + WORKER_SECONDS
        # Keep monitoring the exact policy, skip flags and absence of states.
        while worker_child.poll() is None:
            if worker_child is not None and worker_child.poll() is None and time.monotonic() >= worker_deadline:
                raise TimeoutError('native worker exceeded its bounded lifetime')
            validate_rule_readback(commands.pf('-sr')['stdout'], expected)
            if commands.pf('-ss')['stdout'].strip():
                raise ValueError('unexpected PF state during no-state window')
            commands.interface_snapshot()
            time.sleep(0.2)
        validate_rule_readback(commands.pf('-sr')['stdout'], expected)
        if commands.pf('-ss')['stdout'].strip():
            raise ValueError('unexpected PF state at native completion')
        commands.interface_snapshot()
        report['worker'] = worker_result(output, worker_child, request['operation'])
        report['offline_elapsed_seconds'] = round(time.monotonic() - sealed_at, 3)
        report['status'] = 'offline-boundary-observed'
    except BaseException as error:
        report['status'] = 'failed'
        report['error_type'] = type(error).__name__
        report['error'] = str(error)
    finally:
        signal.alarm(0)
        # Do not permit a second cancellation signal to skip restoration.
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        try:
            stop_worker(worker_child)
            report['cleanup']['worker_reaped'] = True
        except BaseException as error:
            report['cleanup']['worker_reaped'] = False
            report['cleanup']['worker_reap_error'] = type(error).__name__
        if worker_log is not None:
            try:
                worker_log.close()
            except BaseException as error:
                # Diagnostics cannot bypass child cleanup or PF restoration.
                report['worker_log_close_error'] = type(error).__name__
                report['status'] = 'failed'
        try:
            stop_child(child)
            report['cleanup']['child_reaped'] = True
        except BaseException as error:
            report['cleanup']['child_reaped'] = False
            report['cleanup']['error'] = type(error).__name__
        try:
            commands.reap_active()
            report['cleanup']['commands_reaped'] = True
        except BaseException as error:
            report['cleanup']['commands_reaped'] = False
            report['cleanup']['command_reap_error'] = type(error).__name__
        if (report['cleanup'].get('worker_reaped') and report['cleanup'].get('child_reaped')
                and report['cleanup'].get('commands_reaped')):
            try:
                if restore_needed:
                    commands.pf('-R', '-f', str(original_path))
                    validate_rule_readback(commands.pf('-sr')['stdout'], normalized_rules(original))
                report['cleanup']['filter_restored'] = True
            except BaseException as error:
                report['cleanup']['filter_restored'] = False
                report['cleanup']['error'] = type(error).__name__
            # If root filter restoration fails, retaining PF is fail-closed.
            if report['cleanup'].get('filter_restored'):
                try:
                    if token is not None:
                        commands.pf('-X', token)
                    report['cleanup']['token_released'] = True
                    report['cleanup']['initial_disabled_state_restored'] = not pf_enabled(
                        commands.pf('-s', 'info')['stdout'])
                except BaseException as error:
                    report['cleanup']['token_released'] = False
                    report['cleanup']['error'] = type(error).__name__
        try:
            child_error.close()
        except BaseException as error:
            report['canary_log_close_error'] = type(error).__name__
            report['status'] = 'failed'
        # A restoration command can itself time out while being killed. Never
        # report complete cleanup unless that command is also confirmed reaped.
        report['cleanup']['commands_reaped'] = commands.active is None
        report['cleanup']['complete'] = all(report['cleanup'].get(key) is True for key in (
            'worker_reaped', 'child_reaped', 'commands_reaped', 'filter_restored', 'token_released',
            'initial_disabled_state_restored'))
        if not report['cleanup']['complete']:
            report['status'] = 'cleanup-incomplete'
        if sealed_at is not None:
            # Include process cleanup and policy restoration in the reported
            # offline duration. A slow recovery cannot masquerade as a short
            # window merely because the native test finished quickly.
            report['offline_elapsed_seconds'] = round(time.monotonic() - sealed_at, 3)
        write_json(output / 'guardian-result.log', report, uid, gid)


def detach_guardian(output, uid, gid, address, request_path):
    """Double-fork before PF access; no runner tracking or inherited pipes."""
    if os.geteuid() != 0 or sys.platform != 'darwin':
        raise ValueError('guardian launch requires root on macOS')
    pid = os.fork()
    if pid:
        _, status = os.waitpid(pid, 0)
        os._exit(os.waitstatus_to_exitcode(status))
    os.setsid()
    pid = os.fork()
    if pid:
        os._exit(0)
    os.environ.clear()
    os.environ.update(SAFE_ENV)
    os.chdir('/')
    with open('/dev/null', 'rb') as source, (output / 'guardian-console.log').open('ab', buffering=0) as sink:
        os.chown(output / 'guardian-console.log', uid, gid)
        os.dup2(source.fileno(), 0)
        os.dup2(sink.fileno(), 1)
        os.dup2(sink.fileno(), 2)
    deadline = time.monotonic() + 3
    while os.getppid() != 1 and time.monotonic() < deadline:
        time.sleep(0.01)
    guardian(output, uid, gid, address, request_path)
    os._exit(0)


WORKER_SECONDS = 30
OFFLINE_SECONDS = 45
ONLINE_RECOVERY_SECONDS = 2
LOG_BYTES = 1024 * 1024
POLICY = ('(version 1)(allow default)(deny network*)(allow network-bind)'
          '(allow network-outbound (remote ip "localhost:*"))'
          '(allow network-inbound (local ip "localhost:*"))')
CONFIG = {'ndi': {'networks': {'discovery': '127.0.0.1'}}}
RECEIPT_CHECKS = frozenset(('ipv4_established_blocked', 'ipv4_fresh_blocked',
    'ipv4_tcp_loopback', 'ipv4_udp_loopback', 'ipv6_tcp_loopback', 'ipv6_udp_loopback',
    'anonymous_socketpair', 'exact_policy', 'empty_states', 'no_external_skip',
    'worker_reaped', 'canary_reaped', 'commands_reaped', 'original_policy_restored',
    'original_disabled_restored', 'fresh_https_recovered'))
RECEIPT_FIELDS = frozenset(('schema', 'status', 'operation', 'binary_sha256',
    'worker_limit_seconds', 'offline_limit_seconds', 'offline_elapsed_seconds',
    'block_packets_delta', 'external_ipv6', 'checks', 'details_sha256'))
FILE_ENV = frozenset(('GRAFTON_NDI_FILE_FIXTURE', 'GRAFTON_NDI_FILE_SCRATCH',
                     'GRAFTON_NDI_TEST_STANDARD_RUNTIME'))
RUNTIME_ENV = frozenset(('NDI_RUNTIME_DIR_V6', 'NDI_EXPECTED_RUNTIME',
                        'GRAFTON_NDI_RUNTIME', 'GRAFTON_NDI_ISOLATED_NETWORK'))
REQUEST_FIELDS = frozenset(('schema', 'operation', 'binary', 'binary_sha256', 'args', 'env', 'timeout'))
FAILURE_STAGES = frozenset(('setup', 'barrier', 'worker', 'cleanup', 'recovery'))


class ConfinementError(ValueError):
    def __init__(self, stage):
        if stage not in FAILURE_STAGES:
            stage = 'setup'
        super().__init__('MACOS_CONFINEMENT_FAILURE=' + stage)


def digest(path):
    value = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            value.update(chunk)
    return value.hexdigest()


def read_json(path, limit=64 * 1024):
    with Path(path).open('rb') as stream:
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise ValueError('macOS execution record exceeds its bound')
    return json.loads(data)


def validate_receipt(receipt, *, operation, binary_sha256):
    if not isinstance(receipt, dict) or set(receipt) != RECEIPT_FIELDS:
        raise ValueError('macOS confinement receipt schema is incomplete or unknown')
    expected = {'schema': 1, 'status': 'passed', 'operation': operation, 'binary_sha256': binary_sha256,
                'worker_limit_seconds': WORKER_SECONDS, 'offline_limit_seconds': OFFLINE_SECONDS,
                'external_ipv6': 'not-probed'}
    if any(receipt[key] != value for key, value in expected.items()):
        raise ValueError('macOS confinement receipt identity or limits differ')
    if any(type(receipt[key]) is not int for key in ('schema', 'worker_limit_seconds', 'offline_limit_seconds')):
        raise ValueError('macOS confinement receipt integer fields are invalid')
    if (not isinstance(receipt['checks'], dict) or set(receipt['checks']) != RECEIPT_CHECKS
            or any(value is not True for value in receipt['checks'].values())):
        raise ValueError('macOS confinement proof is incomplete')
    elapsed = receipt['offline_elapsed_seconds']
    if type(elapsed) not in (float, int) or not math.isfinite(elapsed) or not 0 < elapsed <= OFFLINE_SECONDS:
        raise ValueError('macOS confinement exceeded its offline bound')
    if type(receipt['block_packets_delta']) is not int or receipt['block_packets_delta'] <= 0:
        raise ValueError('macOS confinement has no observed drop counter increase')
    for field in ('binary_sha256', 'details_sha256'):
        if not isinstance(receipt[field], str) or not re.fullmatch(r'[0-9a-f]{64}', receipt[field]):
            raise ValueError('macOS confinement digest is invalid')
    return receipt


def allowed_operation(binary, args, operation):
    # These registries are the existing public suite policy, never a second SDK
    # source/version manifest. A root helper cannot select an arbitrary command.
    source = str(Path(__file__).resolve().parent)
    if source not in sys.path:
        sys.path.insert(0, source)
    from native import suite_tests
    listing_suites = {suite for mode in ('regressions', 'core')
                      for suite in suite_tests(mode, 'darwin')}
    mapping = {test: suite for mode in ('regressions', 'core', 'advanced', 'file', 'file-negative')
               for suite, tests in suite_tests(mode, 'darwin').items() for test in tests}
    if operation == 'runtime-identity':
        if args or binary.name != 'runtime-probe':
            raise ValueError('unreviewed runtime-identity invocation')
        return
    if operation == 'sandbox-diagnostic':
        expected = Path(__file__).resolve().with_name('sandbox_probe.py')
        if binary != Path(sys.executable).resolve() or args != [str(expected)]:
            raise ValueError('unreviewed sandbox diagnostic invocation')
        return
    if operation == 'list':
        if args != ['--list', '--ignored', '--format', 'terse']:
            raise ValueError('unreviewed native test listing')
        selected = listing_suites
    else:
        if operation not in mapping or args != [operation, '--exact', '--ignored', '--test-threads=1', '--nocapture']:
            raise ValueError('unreviewed native test invocation')
        selected = {mapping[operation]}
    if not any(re.fullmatch(re.escape(suite) + r'(?:-[0-9a-f]+)?', binary.name) for suite in selected):
        raise ValueError('native executable name differs from its fixed suite')


def execution_environment(env):
    runtime = Path(env['NDI_EXPECTED_RUNTIME']).resolve(strict=True)
    if not runtime.is_file() or runtime.name not in ('libndi.dylib', 'libndi_advanced.dylib'):
        raise ValueError('selected macOS runtime is unavailable')
    result = dict(SAFE_ENV, NDI_RUNTIME_DIR_V6=str(runtime.parent), NDI_EXPECTED_RUNTIME=str(runtime),
                  GRAFTON_NDI_RUNTIME=str(runtime), GRAFTON_NDI_ISOLATED_NETWORK='1')
    for key in FILE_ENV:
        if key in env:
            path = Path(env[key]).resolve(strict=True)
            if key == 'GRAFTON_NDI_FILE_SCRATCH':
                if not path.is_dir():
                    raise ValueError('File test scratch is not a directory')
            elif not path.is_file():
                raise ValueError('File test input is not a file')
            result[key] = str(path)
    return result


def validate_request(path):
    request = read_json(path)
    if not isinstance(request, dict) or set(request) != REQUEST_FIELDS or type(request['schema']) is not int or request['schema'] != 1:
        raise ValueError('unreviewed macOS execution request')
    binary = Path(request['binary']).resolve(strict=True)
    if not binary.is_file() or not os.access(binary, os.X_OK) or str(binary) != request['binary']:
        raise ValueError('native executable is absent or changed')
    if not isinstance(request['binary_sha256'], str) or digest(binary) != request['binary_sha256']:
        raise ValueError('native executable differs from its selected artifact')
    if not isinstance(request['args'], list) or any(not isinstance(arg, str) for arg in request['args']):
        raise ValueError('native arguments must match the fixed policy')
    allowed_operation(binary, request['args'], request['operation'])
    timeout = request['timeout']
    if type(timeout) not in (float, int) or not math.isfinite(timeout) or not 0 < timeout <= WORKER_SECONDS:
        raise ValueError('native execution timeout is outside its fixed bound')
    env = request['env']
    if (not isinstance(env, dict) or set(env) - (set(SAFE_ENV) | RUNTIME_ENV | FILE_ENV)
            or env != execution_environment(env)):
        raise ValueError('native environment differs from the fixed allowlist')
    return request


def worker(request_path, output):
    if sys.platform != 'darwin' or os.getuid() == 0 or os.geteuid() == 0:
        raise ValueError('native worker must be unprivileged on macOS')
    report = {'schema': 1, 'status': 'failed'}
    try:
        request = validate_request(request_path)
        env = dict(request['env'])
        config = output / 'ndi-config'; config.mkdir()
        write_json(config / 'ndi-config.v1.json', CONFIG)
        temporary = output / 'tmp'; temporary.mkdir()
        env.update(NDI_CONFIG_DIR=str(config), TMPDIR=str(temporary))
        if request['operation'] == 'sandbox-diagnostic':
            # This fixed SDK-free script must be able to explain a denied
            # primitive. Its output never receives a qualifying test receipt.
            report['diagnostic_only'] = True
        else:
            report['loopback'] = loopback()
            left, right = socket.socketpair()
            with left, right:
                left.settimeout(1); right.settimeout(1)
                left.sendall(b'P')
                if right.recv(1) != b'P':
                    raise ValueError('anonymous socketpair payload mismatch')
            report['anonymous_socketpair'] = True
        code = run_native([request['binary'], *request['args']], env, output, request['timeout'])
        report.update(status='completed', returncode=code)
    except subprocess.TimeoutExpired:
        report['status'] = 'timed-out'
    except BaseException as error:
        report['error_type'] = type(error).__name__
        report['error'] = str(error)
    finally:
        write_json(output / 'worker-result.log', report)


def run_native(argv, env, output, timeout):
    # Bound diagnostic streams, not the files exercised by File SDK tests.
    # The native process remains in this worker's tracked process group.
    deadline = time.monotonic() + timeout
    with (output / 'native-stdout.log').open('wb') as stdout, (output / 'native-stderr.log').open('wb') as stderr:
        process = subprocess.Popen(argv, env=env, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            with selectors.DefaultSelector() as selector:
                for pipe, destination in ((process.stdout, stdout), (process.stderr, stderr)):
                    selector.register(pipe, selectors.EVENT_READ, [destination, 0])
                while selector.get_map():
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise subprocess.TimeoutExpired(argv, timeout)
                    for key, _ in selector.select(min(remaining, 0.1)):
                        chunk = os.read(key.fileobj.fileno(), 65536)
                        if not chunk:
                            selector.unregister(key.fileobj)
                            continue
                        destination, total = key.data
                        destination.write(chunk[:max(0, LOG_BYTES - total)])
                        key.data[1] += len(chunk)
                        if key.data[1] > LOG_BYTES:
                            raise ValueError('native diagnostic stream exceeded its bound')
            return process.wait(timeout=max(0.001, deadline - time.monotonic()))
        except BaseException:
            process.kill()
            process.wait(timeout=2)
            raise
        finally:
            process.stdout.close()
            process.stderr.close()


def launch_worker(request_path, output, uid, gid):
    def demote():
        os.setgroups([]); os.setgid(gid); os.setuid(uid)
    log = (output / 'worker-console.log').open('wb')
    os.chown(output / 'worker-console.log', uid, gid)
    try:
        process = subprocess.Popen(['/usr/bin/sandbox-exec', '-p', POLICY, sys.executable,
            '-I', str(Path(__file__).resolve()), '--worker', str(request_path), '--output', str(output)],
            env=SAFE_ENV, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
            start_new_session=True, close_fds=True, preexec_fn=demote)
    except BaseException:
        log.close()
        raise
    return process, log


def stop_worker(process):
    if process is None:
        return
    for sig, seconds in ((signal.SIGTERM, 1), (signal.SIGKILL, 2)):
        try:
            os.killpg(process.pid, sig)
        except ProcessLookupError:
            process.wait(timeout=seconds)
            return
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            process.poll()
            try:
                os.killpg(process.pid, 0)
            except ProcessLookupError:
                process.wait(timeout=max(0.1, deadline - time.monotonic()))
                return
            time.sleep(0.02)
    raise RuntimeError('native worker group could not be reaped')


def worker_result(output, process, operation):
    result = read_json(output / 'worker-result.log')
    diagnostic = operation == 'sandbox-diagnostic' and result.get('diagnostic_only') is True
    controls = (result.get('loopback') == ['ipv4.tcp', 'ipv4.udp', 'ipv6.tcp', 'ipv6.udp']
                and result.get('anonymous_socketpair') is True)
    if process.returncode != 0 or result.get('status') not in ('completed', 'timed-out') or not (diagnostic or controls):
        raise ValueError('native worker failed its confinement controls')
    return result


def receipt_from_result(report, *, operation, binary_sha256, details_sha256):
    worker_report = report['worker']
    cleanup = report['cleanup']
    if (report['status'] != 'offline-boundary-observed' or not cleanup['complete']
            or report['operation'] != operation or report['binary_sha256'] != binary_sha256
            or worker_report['status'] != 'completed' or report.get('fresh_https_recovered') is not True):
        raise ValueError('macOS execution or recovery did not complete')
    checks = {key: True for key in RECEIPT_CHECKS}
    checks.update(ipv4_established_blocked=report['canary']['established_blocked'],
        ipv4_fresh_blocked=report['canary']['fresh_blocked'],
        anonymous_socketpair=worker_report['anonymous_socketpair'], worker_reaped=cleanup['worker_reaped'],
        canary_reaped=cleanup['child_reaped'], commands_reaped=cleanup['commands_reaped'],
        original_policy_restored=cleanup['filter_restored'],
        original_disabled_restored=cleanup['initial_disabled_state_restored'])
    receipt = {'schema': 1, 'status': 'passed', 'operation': operation, 'binary_sha256': binary_sha256,
        'worker_limit_seconds': WORKER_SECONDS, 'offline_limit_seconds': OFFLINE_SECONDS,
        'offline_elapsed_seconds': report['offline_elapsed_seconds'],
        'block_packets_delta': report['block_packets_delta'], 'external_ipv6': 'not-probed',
        'checks': checks, 'details_sha256': details_sha256}
    return validate_receipt(receipt, operation=operation, binary_sha256=binary_sha256)


def execute(binary, args, scratch, env, *, operation, timeout=WORKER_SECONDS):
    try:
        return _execute(binary, args, scratch, env, operation=operation, timeout=timeout)
    except (ConfinementError, subprocess.TimeoutExpired):
        raise
    except Exception:
        raise ConfinementError('setup') from None


def _execute(binary, args, scratch, env, *, operation, timeout=WORKER_SECONDS):
    if (sys.platform != 'darwin' or os.getuid() == 0 or os.environ.get('GITHUB_ACTIONS') != 'true'
            or os.environ.get('RUNNER_ENVIRONMENT') != 'github-hosted'):
        raise ValueError('macOS PF execution requires an unprivileged disposable hosted runner')
    binary = Path(binary).resolve(strict=True)
    request = {'schema': 1, 'operation': operation, 'binary': str(binary), 'binary_sha256': digest(binary),
               'args': list(args), 'env': execution_environment(env), 'timeout': min(timeout, WORKER_SECONDS)}
    output = Path(tempfile.mkdtemp(prefix='macos-window-', dir=scratch))
    request_path = output / 'request.json'
    launched_guardian = False
    cleanup_complete = False
    try:
        write_json(request_path, request)
        validate_request(request_path)
        address = socket.getaddrinfo(HOST, 443, socket.AF_INET, socket.SOCK_STREAM)[0][4][0]
        with https_socket(address) as connection:
            head(connection)
        argv = ['/usr/bin/sudo', '-n', '/usr/bin/env', '-i', *[f'{key}={value}' for key, value in SAFE_ENV.items()],
                sys.executable, '-I', str(Path(__file__).resolve()), '--output', str(output),
                '--request', str(request_path), '--guardian', str(os.getuid()), str(os.getgid()), address]
        # Set intent before launching: a failed parent command must not delete
        # the restoration input of a guardian which may already be detached.
        launched_guardian = True
        try:
            launched = subprocess.run(argv, capture_output=True, text=True,
                                      encoding='utf-8', errors='backslashreplace', timeout=10, env=SAFE_ENV)
        except subprocess.TimeoutExpired:
            raise ConfinementError('cleanup') from None
        (output / 'guardian-launch.log').write_text(launched.stdout + launched.stderr, encoding='utf-8')
        if launched.returncode:
            raise RuntimeError('macOS guardian launch failed')
        deadline = time.monotonic() + DRIVER_SECONDS
        result_path = output / 'guardian-result.log'
        while not result_path.exists() and time.monotonic() < deadline:
            time.sleep(0.25)
        if not result_path.exists():
            raise ConfinementError('cleanup')
        report = read_json(result_path)
        cleanup_complete = report['cleanup']['complete'] is True
        if not cleanup_complete:
            raise ConfinementError('cleanup')
        try:
            with https_socket(address, timeout=5) as connection:
                head(connection)
        except Exception:
            raise ConfinementError('recovery') from None
        report['fresh_https_recovered'] = True
        # Give the runner control channel a deliberate online interval between
        # repeated short test windows, beyond our successful fresh HTTPS check.
        time.sleep(ONLINE_RECOVERY_SECONDS)
        stdout = (output / 'native-stdout.log').read_text(encoding='utf-8', errors='backslashreplace') if (output / 'native-stdout.log').exists() else ''
        stderr = (output / 'native-stderr.log').read_text(encoding='utf-8', errors='backslashreplace') if (output / 'native-stderr.log').exists() else ''
        if report.get('worker', {}).get('status') == 'timed-out':
            raise subprocess.TimeoutExpired([str(binary), *args], request['timeout'], output=stdout, stderr=stderr)
        if report['status'] != 'offline-boundary-observed':
            # Preserve any native failure output for CellRun's existing private
            # diagnostic parser, without exporting raw PF/host details.
            marker = str(ConfinementError(report.get('stage', 'setup')))
            result = subprocess.CompletedProcess([str(binary), *args], 125, stdout, stderr + '\n' + marker + '\n')
            result.confinement_evidence = None
            return result
        if operation == 'sandbox-diagnostic':
            result = subprocess.CompletedProcess([str(binary), *args], report['worker']['returncode'], stdout, stderr)
            result.confinement_evidence = None
            return result
        details = json.dumps(report, sort_keys=True, separators=(',', ':')).encode()
        evidence = receipt_from_result(report, operation=operation, binary_sha256=request['binary_sha256'],
                                       details_sha256=hashlib.sha256(details).hexdigest())
        result = subprocess.CompletedProcess([str(binary), *args], report['worker']['returncode'], stdout, stderr)
        result.confinement_evidence = evidence
        return result
    finally:
        # Unknown/incomplete cleanup keeps its private restoration inputs for
        # the independent guardian; only verified complete windows are removed.
        if cleanup_complete or not launched_guardian:
            shutil.rmtree(output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--request', type=Path)
    parser.add_argument('--guardian', nargs=3, metavar=('UID', 'GID', 'IP'))
    parser.add_argument('--canary-child')
    parser.add_argument('--worker', type=Path)
    args = parser.parse_args()
    if sys.platform != 'darwin':
        raise ValueError('macOS native executor requires macOS')
    if args.canary_child and not any((args.output, args.request, args.guardian, args.worker)):
        canary_child(args.canary_child)
    elif args.worker and args.output and not any((args.guardian, args.request, args.canary_child)):
        worker(args.worker.resolve(strict=True), args.output.resolve(strict=True))
    elif args.guardian and args.output and args.request and not any((args.worker, args.canary_child)):
        uid, gid, address = args.guardian
        detach_guardian(args.output.resolve(strict=True), int(uid), int(gid), address,
                        args.request.resolve(strict=True))
    else:
        raise ValueError('macOS executor accepts only its fixed internal worker/guardian modes')


if __name__ == '__main__':
    main()
