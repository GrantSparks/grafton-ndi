"""Offline PF protocol tests; no privileged mutation or external network."""
import copy
import json
import copy
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, MagicMock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import macos_native as probe
import native  # Load the shared suite policy before tests emulate a macOS host.

class ParsingTests(unittest.TestCase):
    def test_initial_policy_is_narrow(self):
        self.assertTrue(probe.known_initial_rules(''))
        self.assertTrue(probe.known_initial_rules('anchor "com.apple/*" all\n'))
        observed = 'scrub-anchor "com.apple/*" all fragment reassemble\nanchor "com.apple/*" all\n'
        self.assertTrue(probe.known_initial_rules(observed))
        self.assertFalse(probe.known_initial_rules(observed.replace('fragment reassemble', 'fragment no reassemble')))
        self.assertFalse(probe.known_initial_rules(observed.replace('com.apple/*', 'custom/*', 1)))
        self.assertFalse(probe.known_initial_rules('pass all\n'))
        self.assertFalse(probe.known_initial_rules('anchor "com.apple/*" all\npass quick all'))

    def test_observed_scrub_prefix_is_preserved_in_offline_and_restore_readback(self):
        # Exact -sr output from BOTH macOS15 architectures in hosted macOS rehearsal.
        observed = ('scrub-anchor "com.apple/*" all fragment reassemble\n'
                    'anchor "com.apple/*" all\n')
        expected = [probe.DEFAULT_SCRUB] + probe.normalized_rules(probe.RULES)
        self.assertEqual(probe.offline_policy(observed), probe.DEFAULT_SCRUB + '\n' + probe.RULES)
        self.assertEqual(probe.original_policy(observed), observed)
        self.assertEqual(probe.expected_active_rules(observed), expected)
        probe.validate_rule_readback(probe.offline_policy(observed), expected)
        probe.validate_rule_readback(observed, probe.normalized_rules(observed))
        # Hosted readback showed that filter-only -R inputs erase scrub. Both
        # installed and restored actual outputs from that run must still fail.
        with self.assertRaises(ValueError):
            probe.validate_rule_readback(probe.RULES, expected)
        with self.assertRaises(ValueError):
            probe.validate_rule_readback(probe.DEFAULT_FILTER, probe.normalized_rules(observed))
        self.assertEqual(probe.preserved_scrub_rules(observed), [probe.DEFAULT_SCRUB])

    def test_scrub_normalization_never_hides_extra_or_changed_rules(self):
        observed = probe.DEFAULT_SCRUB + '\n' + probe.DEFAULT_FILTER + '\n'
        rewritten_scrub = 'scrub-anchor "/*" all fragment reassemble\n'
        for actual in (rewritten_scrub + probe.RULES,
                       probe.offline_policy(observed) + 'pass quick all\n',
                       probe.offline_policy(observed).replace('fragment reassemble', 'fragment no reassemble')):
            with self.subTest(actual=actual), self.assertRaises(ValueError):
                probe.validate_rule_readback(actual, probe.expected_active_rules(observed))
        with self.assertRaises(ValueError):
            probe.validate_rule_readback(rewritten_scrub + probe.DEFAULT_FILTER,
                                         probe.normalized_rules(observed))
        self.assertEqual(probe.expected_active_rules(''), probe.normalized_rules(probe.RULES))

    def test_scrub_filter_inputs_validate_all_supported_initial_baselines(self):
        for original in ('', probe.DEFAULT_FILTER + '\n',
                         probe.DEFAULT_SCRUB + '\n' + probe.DEFAULT_FILTER + '\n'):
            with self.subTest(original=original):
                self.assertEqual(probe.original_policy(original), original)
                probe.validate_rule_readback(probe.offline_policy(original),
                                             probe.expected_active_rules(original))
                probe.validate_rule_readback(probe.original_policy(original),
                                             probe.normalized_rules(original))
        for invalid in ('pass quick all\n', probe.DEFAULT_SCRUB + '\n',
                        'scrub-anchor "/*" all fragment reassemble\n' + probe.DEFAULT_FILTER):
            for build in (probe.offline_policy, probe.original_policy, probe.expected_active_rules):
                with self.subTest(invalid=invalid, build=build.__name__), self.assertRaises(ValueError):
                    build(invalid)

    def test_translation_root_and_parent_must_be_known_empty(self):
        default = 'nat-anchor "com.apple/*" all\nrdr-anchor "com.apple/*" all\n'
        probe.validate_initial_translations('', '')
        probe.validate_initial_translations(default, '')
        bypasses = (
            ('nat pass on en0 from any to any -> (en0)', ''),
            ('rdr pass on en0 proto udp from any to any -> 127.0.0.1', ''),
            (default, 'nat pass on en0 from any to any -> (en0)'),
            ('nat-anchor "unknown/*" all', ''),
            (default + 'binat pass from any to any -> 192.0.2.1', ''),
        )
        for root, child in bypasses:
            with self.subTest(root=root, child=child), self.assertRaises(ValueError):
                probe.validate_initial_translations(root, child)

    def test_observed_parent_topology_requires_direct_empty_leaf_evidence(self):
        # Exact parent links/names from both hosts in hosted macOS inventory.
        parent = 'anchor "200.AirDrop/*" all\nanchor "250.ApplicationFirewall/*" all\n'
        names = '  com.apple/200.AirDrop\n  com.apple/250.ApplicationFirewall\n'
        empty = {name: {'rules': '', 'translations': '', 'descendants': ''}
                 for name in probe.APPLE_LEAVES}
        probe.validate_apple_topology('', '', {})
        probe.validate_apple_topology(parent, names, empty)
        self.assertEqual(probe.expected_leaf_names(names), probe.APPLE_LEAVES)
        for value in ('scrub all fragment reassemble', 'pass quick all',
                      'nat pass on en0 from any to any -> (en0)', 'com.apple/200.AirDrop/unknown'):
            for field in ('rules', 'translations', 'descendants'):
                changed = copy.deepcopy(empty)
                changed[probe.APPLE_LEAVES[0]][field] = value
                with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                    probe.validate_apple_topology(parent, names, changed)
        for changed_parent, changed_names, changed_leaves in (
            (parent, names, {}),
            (parent + 'pass quick all\n', names, empty),
            ('', names, empty),
            (parent, names + '  com.apple/999.Unknown\n', empty),
            (parent, 'com.apple/200.AirDrop\n', empty),
            (parent, names, {**empty, 'com.apple/extra': {}}),
        ):
            with self.subTest(parent=changed_parent, names=changed_names), self.assertRaises(ValueError):
                probe.validate_apple_topology(changed_parent, changed_names, changed_leaves)

    def test_proof_query_rejects_actual_zero_exit_diagnostic(self):
        notices = 'No ALTQ support in kernel\nALTQ related functions disabled\n'
        good = {'exit_code': 0, 'timed_out': False, 'stderr': notices}
        probe.validate_query_output(good)
        probe.validate_query_output({**good, 'stderr': ''})
        for changed in (
            {**good, 'stderr': notices + 'pfctl: DIOCGETRULES: Invalid argument\n'},
            {**good, 'stderr': 'unexpected notice\n'},
            {**good, 'exit_code': 1},
            {**good, 'timed_out': True},
            {**good, 'output_truncated': True},
        ):
            with self.subTest(result=changed), self.assertRaises(ValueError):
                probe.validate_query_output(changed)

    def test_direct_leaf_query_uses_strict_diagnostic_validation(self):
        with tempfile.TemporaryDirectory() as temporary:
            commands = probe.Commands(Path(temporary), None, None)
            result = {'exit_code': 0, 'timed_out': False, 'stdout': '',
                      'stderr': 'pfctl: DIOCGETRULES: Invalid argument\n'}
            with patch.object(commands, 'run', return_value=result) as run:
                with self.assertRaises(ValueError):
                    commands.pf('-a', probe.APPLE_LEAVES[0], '-sn')
                self.assertEqual(run.call_args.args[0],
                                 ['/sbin/pfctl', '-a', probe.APPLE_LEAVES[0], '-sn'])

    def test_status_unknown_or_duplicate_is_rejected(self):
        self.assertFalse(probe.pf_enabled('Status: Disabled for 0 days\n'))
        self.assertTrue(probe.pf_enabled('Status: Enabled for 1 day\n'))
        for text in ('', 'Disabled', 'Status: Enabled\nStatus: Disabled'):
            with self.subTest(text=text), self.assertRaises(ValueError):
                probe.pf_enabled(text)

    def test_interface_inventory_cannot_omit_or_skip_nonloopback(self):
        fixture = 'all\n  References: [ States: 0 Rules: 0 ]\nlo0 (skip)\n  Cleared: today\nen0\n'
        self.assertEqual(probe.parse_interfaces(fixture, ['lo0', 'en0']),
                         {'all': False, 'lo0': True, 'en0': False})
        for text, actual in ((fixture, ['lo0', 'en0', 'utun7']),
                             (fixture.replace('en0\n', 'en0 (skip)\n'), ['lo0', 'en0']),
                             ('lo0\nen0 (unknown)\n', ['lo0', 'en0']),
                             ('lo0\nen0\nen0\n', ['lo0', 'en0'])):
            with self.subTest(text=text), self.assertRaises(ValueError):
                probe.parse_interfaces(text, actual)

    def test_owned_token_and_rule_counter_are_exact(self):
        self.assertEqual(probe.enable_token('pf enabled\nToken : 123\n'), '123')
        for text in ('', 'Token: 0', 'Token: 12\nToken: 13'):
            with self.subTest(text=text), self.assertRaises(ValueError):
                probe.enable_token(text)
        text = f'@1 block drop quick all label "{probe.LABEL}"\n  [ Evaluations: 4 Packets: 3 Bytes: 180 ]\n'
        self.assertEqual(probe.block_packets(text), 3)
        for changed in ('', text + text, text.replace('Packets: 3', 'Unknown: 3')):
            with self.subTest(text=changed), self.assertRaises(ValueError):
                probe.block_packets(changed)


class HttpCanaryTests(unittest.TestCase):
    def test_complete_final_http_response_proves_online_connectivity(self):
        for status in (200, 301, 403, 429, 500, 599):
            response = Mock(status=status)
            connection = Mock()
            with self.subTest(status=status), patch.object(probe.http.client, 'HTTPResponse', return_value=response):
                self.assertEqual(probe.head(connection), status)
                response.begin.assert_called_once()
                response.read.assert_called_once()
                response.close.assert_called_once()
        for status in (100, 199, 600, True, '200'):
            response = Mock(status=status)
            with self.subTest(status=status), patch.object(probe.http.client, 'HTTPResponse', return_value=response):
                with self.assertRaises(ValueError):
                    probe.head(Mock())

    def test_online_requires_complete_response_and_offline_http_errors_are_not_blocks(self):
        response = Mock(status=403)
        response.read.side_effect = probe.http.client.IncompleteRead(b'')
        with patch.object(probe.http.client, 'HTTPResponse', return_value=response):
            with self.assertRaises(probe.http.client.IncompleteRead):
                probe.head(Mock())
        for status in (200, 403, 429, 500):
            with self.subTest(status=status), patch.object(probe, 'head', return_value=status):
                self.assertEqual(probe.offline_https_attempt(Mock()), (False, status))

    def test_only_transport_timeout_proves_blocked_attempt(self):
        for error in (TimeoutError(),):
            with self.subTest(error=type(error).__name__), patch.object(probe, 'head', side_effect=error):
                self.assertEqual(probe.offline_https_attempt(Mock()), (True, None))
            with patch.object(probe.socket, 'create_connection', side_effect=error):
                self.assertTrue(probe.offline_connect_attempt('192.0.2.10'))
        for error in (ConnectionRefusedError(), ConnectionResetError(), ValueError('bad HTTP'),
                      probe.http.client.RemoteDisconnected('peer closed')):
            with self.subTest(error=type(error).__name__), patch.object(probe, 'head', side_effect=error):
                with self.assertRaises(type(error)):
                    probe.offline_https_attempt(Mock())
            with patch.object(probe.socket, 'create_connection', side_effect=error):
                with self.assertRaises(type(error)):
                    probe.offline_connect_attempt('192.0.2.10')
        connection = Mock()
        connection.__enter__ = Mock(return_value=connection)
        connection.__exit__ = Mock(return_value=False)
        with patch.object(probe.socket, 'create_connection', return_value=connection):
            self.assertFalse(probe.offline_connect_attempt('192.0.2.10'))


class CleanupTests(unittest.TestCase):
    def test_interrupted_command_is_killed_and_reaped_before_propagation(self):
        process = Mock(pid=900, returncode=-9)
        process.communicate.side_effect = [InterruptedError('alarm'), ('out', 'err')]
        with tempfile.TemporaryDirectory() as temporary:
            commands = probe.Commands(Path(temporary), None, None)
            with patch.object(probe.subprocess, 'Popen', return_value=process), \
                 patch.object(probe.os, 'killpg') as kill:
                with self.assertRaises(InterruptedError):
                    commands.run(['/sbin/pfctl', '-R', '-f', '/private/policy'])
                kill.assert_called_once_with(900, signal.SIGKILL)
                self.assertEqual(process.communicate.call_count, 2)
            record = json.loads((Path(temporary) / 'command-001.log').read_text())
            self.assertEqual(record['interrupted'], 'InterruptedError')

    def test_timed_out_command_returns_evidence_only_when_check_disabled(self):
        process = Mock(pid=900, returncode=-9)
        process.communicate.side_effect = [subprocess.TimeoutExpired('pfctl', 4), ('', 'Token : 42')]
        with tempfile.TemporaryDirectory() as temporary:
            with patch.object(probe.subprocess, 'Popen', return_value=process), \
                 patch.object(probe.os, 'killpg') as kill:
                result = probe.Commands(Path(temporary), None, None).run(['pfctl'], check=False)
                self.assertTrue(result['timed_out'])
                self.assertEqual(probe.enable_token(result['stderr']), '42')
                kill.assert_called_once_with(900, signal.SIGKILL)

    def test_unreaped_mutator_blocks_restoration_until_confirmed_dead(self):
        process = Mock(pid=900, returncode=None)
        process.communicate.side_effect = [
            subprocess.TimeoutExpired('pfctl', 4),
            subprocess.TimeoutExpired('pfctl-after-kill', 2),
            subprocess.TimeoutExpired('pfctl-retry-reap', 2),
            ('', ''),
        ]
        with tempfile.TemporaryDirectory() as temporary:
            commands = probe.Commands(Path(temporary), None, None)
            with patch.object(probe.subprocess, 'Popen', return_value=process) as spawn, \
                 patch.object(probe.os, 'killpg'):
                with self.assertRaisesRegex(RuntimeError, 'could not be reaped'):
                    commands.pf('-R', '-f', '/private/offline-policy')
                self.assertIs(commands.active, process)
                with self.assertRaisesRegex(RuntimeError, 'previous local command'):
                    commands.pf('-R', '-f', '/private/original-policy')
                with self.assertRaisesRegex(RuntimeError, 'previous local command'):
                    commands.pf('-X', '123')
                self.assertEqual(spawn.call_count, 1)
                with self.assertRaises(subprocess.TimeoutExpired):
                    commands.reap_active()
                self.assertIs(commands.active, process)
                commands.reap_active()
                self.assertIsNone(commands.active)
            record = json.loads((Path(temporary) / 'command-001.log').read_text())
            self.assertIs(record['command_reaped'], False)

    def test_successful_and_reaped_interrupted_commands_clear_active_marker(self):
        process = Mock(pid=900, returncode=0)
        process.communicate.return_value = ('output', '')
        with tempfile.TemporaryDirectory() as temporary:
            commands = probe.Commands(Path(temporary), None, None)
            with patch.object(probe.subprocess, 'Popen', return_value=process):
                commands.pf('-sr')
                self.assertIsNone(commands.active)
                commands.pf('-sn')
                self.assertIsNone(commands.active)

    def test_child_must_be_reaped_and_group_absent(self):
        child = Mock(pid=987)
        child.poll.return_value = None
        def signal_group(pid, sig):
            if sig == 0:
                raise ProcessLookupError()
        with patch.object(probe.os, 'killpg', side_effect=signal_group) as kill:
            probe.stop_child(child)
            child.wait.assert_called_once_with(timeout=1)
            self.assertEqual(kill.call_args_list[0].args, (987, signal.SIGTERM))
        child.poll.return_value = 0
        with patch.object(probe.os, 'killpg'):
            with self.assertRaises(RuntimeError):
                probe.stop_child(child)

    def test_child_escalation_is_bounded(self):
        child = Mock(pid=987)
        child.poll.return_value = None
        child.wait.side_effect = [subprocess.TimeoutExpired('fixture', 1), -9]
        def signal_group(pid, sig):
            if sig == 0:
                raise ProcessLookupError()
        with patch.object(probe.os, 'killpg', side_effect=signal_group) as kill:
            probe.stop_child(child)
            self.assertEqual([c.args[1] for c in kill.call_args_list],
                             [signal.SIGTERM, signal.SIGKILL, 0])
            self.assertEqual([c.kwargs['timeout'] for c in child.wait.call_args_list], [1, 2])



TEST = 'owned_pipeline_returns_original_buffers_and_metadata'


def request_fixture(root):
    binary = root / 'async_send_lifecycle-deadbeef'
    binary.write_bytes(b'executable fixture'); binary.chmod(0o700)
    runtime = root / 'libndi.dylib'; runtime.write_bytes(b'runtime fixture')
    request = {'schema': 1, 'operation': TEST, 'binary': str(binary), 'binary_sha256': probe.digest(binary),
        'args': [TEST, '--exact', '--ignored', '--test-threads=1', '--nocapture'],
        'env': probe.execution_environment({'NDI_EXPECTED_RUNTIME': str(runtime)}), 'timeout': 30}
    path = root / 'request.json'; probe.write_json(path, request)
    return path, request


def receipt_fixture(operation=TEST, binary_hash='b' * 64):
    return {'schema': 1, 'status': 'passed', 'operation': operation, 'binary_sha256': binary_hash,
        'worker_limit_seconds': 30, 'offline_limit_seconds': 45, 'offline_elapsed_seconds': 3.5,
        'block_packets_delta': 4, 'external_ipv6': 'not-probed',
        'checks': {key: True for key in probe.RECEIPT_CHECKS}, 'details_sha256': 'd' * 64}


def completed_report(request):
    return {'schema': 1, 'status': 'offline-boundary-observed', 'stage': 'worker',
        'operation': request['operation'], 'binary_sha256': request['binary_sha256'],
        'offline_elapsed_seconds': 3.5, 'block_packets_delta': 4,
        'canary': {'established_blocked': True, 'fresh_blocked': True},
        'worker': {'status': 'completed', 'returncode': 0, 'anonymous_socketpair': True,
            'loopback': ['ipv4.tcp', 'ipv4.udp', 'ipv6.tcp', 'ipv6.udp']},
        'cleanup': {key: True for key in ('complete', 'worker_reaped', 'child_reaped', 'commands_reaped',
            'filter_restored', 'token_released', 'initial_disabled_state_restored')}}


class ReceiptTests(unittest.TestCase):
    def test_only_closed_complete_bound_receipts_are_accepted(self):
        good = receipt_fixture()
        probe.validate_receipt(good, operation=TEST, binary_sha256='b' * 64)
        mutations = [lambda value: value.update(extra='private data'),
            lambda value: value.update(operation='another_test'), lambda value: value.update(binary_sha256='a' * 64),
            lambda value: value.update(schema=True), lambda value: value.update(worker_limit_seconds=180),
            lambda value: value.update(offline_elapsed_seconds=46), lambda value: value.update(offline_elapsed_seconds=float('nan')),
            lambda value: value.update(offline_elapsed_seconds=True), lambda value: value.update(block_packets_delta=0),
            lambda value: value.update(block_packets_delta=True), lambda value: value.update(details_sha256='private/path'),
            lambda value: value['checks'].update(worker_reaped=False), lambda value: value['checks'].update(extra=True)]
        for mutate in mutations:
            changed = copy.deepcopy(good); mutate(changed)
            with self.subTest(changed=changed), self.assertRaises(ValueError):
                probe.validate_receipt(changed, operation=TEST, binary_sha256='b' * 64)
        for key in good:
            changed = copy.deepcopy(good); changed.pop(key)
            with self.subTest(missing=key), self.assertRaises(ValueError):
                probe.validate_receipt(changed, operation=TEST, binary_sha256='b' * 64)

    def test_incomplete_cleanup_or_recovery_cannot_produce_evidence(self):
        request = {'operation': TEST, 'binary_sha256': 'b' * 64}
        good = completed_report(request); good['fresh_https_recovered'] = True
        receipt = probe.receipt_from_result(good, operation=TEST, binary_sha256='b' * 64, details_sha256='d' * 64)
        self.assertEqual(set(receipt), probe.RECEIPT_FIELDS)
        for change in ('complete', 'recovery', 'status'):
            failed = copy.deepcopy(good)
            if change == 'complete': failed['cleanup']['complete'] = False
            elif change == 'recovery': failed['fresh_https_recovered'] = False
            else: failed['status'] = 'failed'
            with self.subTest(change=change), self.assertRaises(ValueError):
                probe.receipt_from_result(failed, operation=TEST, binary_sha256='b' * 64, details_sha256='d' * 64)


class RequestTests(unittest.TestCase):
    def test_only_reviewed_exact_operation_and_current_binary_are_allowed(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); path, request = request_fixture(root)
            self.assertEqual(probe.validate_request(path), request)
            mutations = [lambda value: value.update(command=['shell']), lambda value: value.update(args=['--help']),
                lambda value: value.update(operation='arbitrary'), lambda value: value.update(timeout=180),
                lambda value: value['env'].update(NDI_SDK_ARCHIVE_TOKEN='secret')]
            for mutate in mutations:
                changed = copy.deepcopy(request); mutate(changed); probe.write_json(path, changed)
                with self.subTest(changed=changed), self.assertRaises(ValueError): probe.validate_request(path)
            probe.write_json(path, request); Path(request['binary']).write_bytes(b'changed executable')
            with self.assertRaises(ValueError): probe.validate_request(path)

    def test_all_existing_mac_native_modes_are_accepted_without_extra_commands(self):
        import native
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for mode in ('regressions', 'core', 'advanced', 'file', 'file-negative'):
                for suite, tests in native.suite_tests(mode, 'darwin').items():
                    binary = root / (suite + '-aabbcc')
                    for name in tests:
                        probe.allowed_operation(binary, [name, '--exact', '--ignored', '--test-threads=1', '--nocapture'], name)
            probe.allowed_operation(root / 'runtime-probe', [], 'runtime-identity')
            probe.allowed_operation(Path(sys.executable).resolve(), [str(Path(probe.__file__).with_name('sandbox_probe.py'))], 'sandbox-diagnostic')
            with self.assertRaises(ValueError):
                probe.allowed_operation(Path(sys.executable).resolve(), ['-c', 'unreviewed code'], 'sandbox-diagnostic')

    def test_core_listings_keep_exact_arguments_and_suite_binding(self):
        import native
        listing = ['--list', '--ignored', '--format', 'terse']
        for suite, tests in native.suite_tests('core', 'darwin').items():
            binary = Path('/unused') / (suite + '-aabbcc')
            probe.allowed_operation(binary, listing, 'list')
            with self.subTest(suite=suite), self.assertRaises(ValueError):
                probe.allowed_operation(binary, ['--list'], 'list')
            for name in tests:
                args = [name, '--exact', '--ignored', '--test-threads=1', '--nocapture']
                for wrong_suite in ('unregistered', 'async_send_lifecycle'):
                    wrong_binary = Path('/unused') / (wrong_suite + '-aabbcc')
                    with self.subTest(suite=suite, test=name, wrong_suite=wrong_suite), self.assertRaises(ValueError):
                        probe.allowed_operation(wrong_binary, args, name)
        with self.assertRaises(ValueError):
            probe.allowed_operation(Path('/unused/unregistered-aabbcc'), listing, 'list')
        with self.assertRaises(ValueError):
            probe.allowed_operation(Path('/unused/advanced_runtime-aabbcc'), listing, 'list')

    def test_runtime_and_file_environment_is_allowlisted_without_home_override(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); _, request = request_fixture(root)
            fixture = root / 'fixture.mkv'; fixture.write_bytes(b'fixture')
            env = probe.execution_environment(dict(request['env'], TOKEN='secret', HOME='unchanged',
                DYLD_INSERT_LIBRARIES='unsafe', GRAFTON_NDI_FILE_FIXTURE=str(fixture), GRAFTON_NDI_FILE_SCRATCH=str(root)))
            self.assertNotIn('HOME', env); self.assertNotIn('TOKEN', env); self.assertNotIn('DYLD_INSERT_LIBRARIES', env)
            self.assertEqual(env['GRAFTON_NDI_FILE_FIXTURE'], str(fixture))
            self.assertEqual(env['GRAFTON_NDI_FILE_SCRATCH'], str(root))


class DriverTests(unittest.TestCase):
    def execute_case(self, root, *, native_code=0, cleanup=True, status='offline-boundary-observed', recovery=True):
        _, request = request_fixture(root)
        scratch = root / 'scratch'; scratch.mkdir()
        def launch(argv, **kwargs):
            output = Path(argv[argv.index('--output') + 1])
            report = completed_report(request); report['worker']['returncode'] = native_code
            report['cleanup']['complete'] = cleanup; report['status'] = status
            probe.write_json(output / 'guardian-result.log', report)
            (output / 'native-stdout.log').write_text('native stdout')
            (output / 'native-stderr.log').write_text('native stderr')
            return subprocess.CompletedProcess(argv, 0, '', '')
        calls = []
        def response(_connection):
            calls.append('https')
            if len(calls) > 1 and not recovery: raise OSError('private endpoint detail')
            return 403
        with patch.object(sys, 'platform', 'darwin'), patch.object(probe.os, 'getuid', return_value=501), \
                patch.dict(os.environ, {'GITHUB_ACTIONS': 'true', 'RUNNER_ENVIRONMENT': 'github-hosted'}), \
                patch.object(probe.socket, 'getaddrinfo', return_value=[(0, 0, 0, '', ('192.0.2.10', 443))]), \
                patch.object(probe, 'https_socket', return_value=MagicMock()), patch.object(probe, 'head', side_effect=response), \
                patch.object(probe.subprocess, 'run', side_effect=launch), patch.object(probe.time, 'sleep') as sleep:
            result = probe.execute(request['binary'], request['args'], scratch, request['env'], operation=TEST)
        return result, scratch, sleep

    def test_native_failure_output_is_preserved_with_valid_confinement_receipt(self):
        with tempfile.TemporaryDirectory() as temporary:
            result, scratch, sleep = self.execute_case(Path(temporary), native_code=17)
            self.assertEqual((result.returncode, result.stdout, result.stderr), (17, 'native stdout', 'native stderr'))
            probe.validate_receipt(result.confinement_evidence, operation=TEST, binary_sha256=result.confinement_evidence['binary_sha256'])
            self.assertEqual(list(scratch.iterdir()), [])
            sleep.assert_called_once_with(probe.ONLINE_RECOVERY_SECONDS)

    def test_confinement_failure_is_distinct_from_native_assertion_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            result, scratch, _ = self.execute_case(Path(temporary), status='failed')
            self.assertEqual(result.returncode, 125)
            self.assertIn('MACOS_CONFINEMENT_FAILURE=worker', result.stderr)
            self.assertIsNone(result.confinement_evidence)
            self.assertEqual(list(scratch.iterdir()), [])

    def test_incomplete_cleanup_keeps_guardian_restoration_inputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with self.assertRaisesRegex(probe.ConfinementError, 'MACOS_CONFINEMENT_FAILURE=cleanup'):
                self.execute_case(root, cleanup=False)
            self.assertEqual(len(list((root / 'scratch').iterdir())), 1)

    def test_recovery_failure_is_closed_and_cannot_export_passed_receipt(self):
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaisesRegex(probe.ConfinementError, '^MACOS_CONFINEMENT_FAILURE=recovery$'):
                self.execute_case(Path(temporary), recovery=False)


class NativeOutputTests(unittest.TestCase):
    def test_file_operations_larger_than_diagnostic_limit_remain_supported(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); recording = root / 'recording.bin'
            script = "from pathlib import Path; Path(" + repr(str(recording)) + ").write_bytes(b'x' * (2 * 1024 * 1024)); print('completed')"
            code = probe.run_native([sys.executable, '-c', script], probe.SAFE_ENV, root, 5)
            self.assertEqual(code, 0)
            self.assertEqual(recording.stat().st_size, 2 * 1024 * 1024)
            self.assertEqual((root / 'native-stdout.log').read_text(), 'completed\n')

    def test_oversized_diagnostic_stream_is_bounded_and_fails(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with self.assertRaisesRegex(ValueError, 'diagnostic stream exceeded'):
                probe.run_native([sys.executable, '-c', "import sys; sys.stdout.write('x' * (2 * 1024 * 1024))"], probe.SAFE_ENV, root, 5)
            self.assertLessEqual((root / 'native-stdout.log').stat().st_size, probe.LOG_BYTES)

    def test_native_timeout_is_bounded_and_child_is_reaped(self):
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaises(subprocess.TimeoutExpired):
                probe.run_native([sys.executable, '-c', 'import time; time.sleep(60)'], probe.SAFE_ENV, Path(temporary), 0.1)


class GuardianLifecycleTests(unittest.TestCase):
    def guardian_case(self, root, *, reap_error=False, worker_error=False, log_close_error=False):
        window = probe
        events = []
        original = window.DEFAULT_SCRUB + '\n' + window.DEFAULT_FILTER + '\n'
        class FakeCommands:
            active = None
            def __init__(self, *args):
                self.mode = 'initial'; self.counter = 0
            def collect_inventory(self, _address):
                pass
            def interface_snapshot(self):
                return {'lo0': False, 'en0': False}
            def run(self, _argv):
                return {'stdout': 'interface: en0\n'}
            def pf(self, *args, **kwargs):
                events.append(args)
                output = ''
                if args == ('-s', 'info'):
                    output = 'Status: Disabled' if self.mode in ('initial', 'disabled') else 'Status: Enabled'
                elif args == ('-sr',):
                    output = window.offline_policy(original) if self.mode == 'offline' else original
                elif args == ('-E',):
                    self.mode = 'enabled'; output = 'Token : 42'
                elif args[:2] == ('-R', '-f'):
                    self.mode = 'offline' if args[2].endswith('offline-policy.conf') else 'restored'
                elif args == ('-vvsr',):
                    output = f'block drop quick all label "{window.LABEL}"\n [ Packets: {self.counter} ]'
                    self.counter += 1
                elif args[:1] == ('-X',):
                    self.mode = 'disabled'
                return {'stdout': output, 'stderr': '', 'exit_code': 0, 'timed_out': False}
            def reap_active(self):
                events.append('commands-reaped')
        module = Mock(WORKER_SECONDS=30)
        module.validate_request.return_value = {'operation': TEST, 'binary_sha256': 'b' * 64}
        process = Mock(returncode=0); process.poll.return_value = 0
        sdk_log = Mock()
        if log_close_error:
            sdk_log.close.side_effect = OSError('diagnostic close failure')
        module.launch_worker.return_value = (process, sdk_log)
        module.worker_result.return_value = {'status': 'completed', 'returncode': 0}
        if worker_error:
            module.worker_result.side_effect = ValueError('fixed worker failed')
        def stopped(_process):
            events.append('worker-stop')
            if reap_error:
                raise RuntimeError('SDK group remains alive')
        module.stop_worker.side_effect = stopped
        ready = {'phase': 'ready', 'uid': 501, 'peer': '192.0.2.10', 'http_status': 200}
        offline = {'phase': 'offline', 'established_blocked': True, 'fresh_blocked': True,
                   'established_http_status': None, 'loopback': ['ipv4.tcp', 'ipv4.udp', 'ipv6.tcp', 'ipv6.udp']}
        with patch.object(window, 'Commands', FakeCommands), patch.object(window, 'validate_request', side_effect=module.validate_request), \
                patch.object(window, 'launch_worker', side_effect=module.launch_worker), \
                patch.object(window, 'worker_result', side_effect=module.worker_result), \
                patch.object(window, 'stop_worker', side_effect=module.stop_worker), \
                patch.object(window.os, 'geteuid', return_value=0), patch.object(window.os, 'getppid', return_value=1), \
                patch.object(window.os, 'chown'), patch.object(window.signal, 'signal'), patch.object(window.signal, 'alarm'), \
                patch.object(window.subprocess, 'Popen', return_value=Mock()), \
                patch.object(window, 'child_row', side_effect=[ready, offline]), \
                patch.object(window, 'stop_child', side_effect=lambda _child: events.append('canary-stop')):
            window.guardian(root, 501, 20, '192.0.2.10', root / 'manifest.json')
        return probe.read_json(root / 'guardian-result.log'), events

    def test_worker_group_is_reaped_before_original_rules_or_connectivity_restore(self):
        with tempfile.TemporaryDirectory() as temporary:
            report, events = self.guardian_case(Path(temporary))
            restoration = next(i for i, event in enumerate(events) if isinstance(event, tuple)
                and event[:2] == ('-R', '-f') and event[2].endswith('original-filter.conf'))
            self.assertLess(events.index('worker-stop'), restoration)
            self.assertLess(events.index('canary-stop'), restoration)
            self.assertLess(restoration, events.index(('-X', '42')))
            self.assertTrue(report['cleanup']['complete'])

    def test_unreaped_worker_group_prevents_all_restoration_and_token_release(self):
        with tempfile.TemporaryDirectory() as temporary:
            report, events = self.guardian_case(Path(temporary), reap_error=True)
            self.assertFalse(report['cleanup']['worker_reaped'])
            self.assertFalse(report['cleanup']['complete'])
            self.assertEqual(report['status'], 'cleanup-incomplete')
            self.assertNotIn(('-X', '42'), events)
            self.assertFalse(any(isinstance(event, tuple) and event[:2] == ('-R', '-f')
                and event[2].endswith('original-filter.conf') for event in events))

    def test_diagnostic_close_error_does_not_bypass_mandatory_cleanup(self):
        with tempfile.TemporaryDirectory() as temporary:
            report, events = self.guardian_case(Path(temporary), log_close_error=True)
            self.assertTrue(report['cleanup']['complete'])
            self.assertIn('canary-stop', events)
            self.assertIn(('-X', '42'), events)
            self.assertEqual(report['worker_log_close_error'], 'OSError')
            self.assertEqual(report['status'], 'failed')

    def test_failed_worker_stays_failed_even_after_complete_restoration(self):
        with tempfile.TemporaryDirectory() as temporary:
            report, _ = self.guardian_case(Path(temporary), worker_error=True)
            self.assertEqual(report['status'], 'failed')
            self.assertTrue(report['cleanup']['complete'])
            self.assertNotIn('qualifying_sdk_result', report)


class WorkerBoundaryTests(unittest.TestCase):
    def worker_case(self, root, *, diagnostic=False, control_failure=False):
        path, request = request_fixture(root)
        if diagnostic: request['operation'] = 'sandbox-diagnostic'
        output = root / 'worker'; output.mkdir()
        with patch.object(sys, 'platform', 'darwin'), patch.object(probe.os, 'getuid', return_value=501), \
                patch.object(probe.os, 'geteuid', return_value=501), patch.object(probe, 'validate_request', return_value=request), \
                patch.object(probe, 'loopback', return_value=['ipv4.tcp', 'ipv4.udp', 'ipv6.tcp', 'ipv6.udp'],
                    side_effect=PermissionError('denied') if control_failure else None) as controls, \
                patch.object(probe, 'run_native', return_value=17 if not diagnostic else 0) as native_call:
            probe.worker(path, output)
        return probe.read_json(output / 'worker-result.log'), native_call, controls, output

    def test_sdk_execution_requires_successful_combined_policy_controls(self):
        with tempfile.TemporaryDirectory() as temporary:
            report, native_call, _, _ = self.worker_case(Path(temporary), control_failure=True)
            self.assertEqual(report['status'], 'failed')
            native_call.assert_not_called()
        with tempfile.TemporaryDirectory() as temporary:
            report, native_call, _, _ = self.worker_case(Path(temporary))
            self.assertEqual(report['status'], 'completed')
            self.assertEqual(report['returncode'], 17)
            self.assertTrue(report['anonymous_socketpair'])
            self.assertEqual(native_call.call_args.args[0][1:], [TEST, '--exact', '--ignored', '--test-threads=1', '--nocapture'])
            env = native_call.call_args.args[1]
            self.assertNotIn('HOME', env)
            self.assertEqual(probe.read_json(Path(env['NDI_CONFIG_DIR']) / 'ndi-config.v1.json'), probe.CONFIG)

    def test_sdk_free_diagnostic_may_report_failed_primitives_but_never_qualifies(self):
        with tempfile.TemporaryDirectory() as temporary:
            report, native_call, controls, output = self.worker_case(Path(temporary), diagnostic=True, control_failure=True)
            controls.assert_not_called(); native_call.assert_called_once()
            self.assertTrue(report['diagnostic_only'])
            self.assertNotIn('loopback', report)
            probe.worker_result(output, Mock(returncode=0), 'sandbox-diagnostic')
            for operation in (TEST, 'runtime-identity', 'list'):
                with self.subTest(operation=operation), self.assertRaises(ValueError):
                    probe.worker_result(output, Mock(returncode=0), operation)

    def test_worker_process_group_is_terminated_and_reaped(self):
        child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'], start_new_session=True)
        try:
            probe.stop_worker(child)
            self.assertIsNotNone(child.returncode)
            with self.assertRaises(ProcessLookupError): os.killpg(child.pid, 0)
        finally:
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGKILL); child.wait()


if __name__ == '__main__':
    unittest.main()
