#!/usr/bin/env python3
"""Execute exact opt-in native tests with reviewed network confinement."""
from __future__ import annotations
import argparse
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import sys
import uuid
from common import ROOT, FEATURE_SETS, digest, feature_flags, write_json
from core_tests import CORE_TESTS
from cargo_artifacts import messages, target_file
from provision import child_environment
from sandbox_probe import validate_report

FILE_TESTS = ('native_file_probe_and_player_controls', 'native_file_record_finalize_and_probe',
              'native_file_async_player_owner_thread')
ADVANCED_TESTS = ('advanced_native_constructors_controls_and_queue',
                  'advanced_native_allocator_replacement_and_capture')
REGRESSION_TESTS = {
    'async_send_lifecycle': (
        'owned_pipeline_returns_original_buffers_and_metadata',
        'copied_send_survives_released_source',
        'mixed_owned_and_copy_send_completes_each_frame_once',
        'concurrent_sync_and_reentrant_flush_preserve_owned_frame',
        'panicking_completion_keeps_new_owned_frame_pending',
        'copied_send_retains_metadata_after_source_drop',
    ),
    'sender_lifecycle_stress': ('rapid_owned_sender_lifecycle',),
    'receiver_status_notifications': ('media_capture_retains_capability_notifications',),
    'framesync_audio_requests': ('queried_channels_remain_fixed_when_source_format_changes',),
    'video_padding_sdk': ('live_receiver_framesync_and_owned_send_skip_padding',),
}


def regression_tests(platform):
    if platform not in ('linux', 'darwin', 'win32'):
        raise ValueError('no reviewed native regression policy for this host')
    # The live padded-video regression is explicitly Linux-only in its Rust
    # source: it observes Linux SDK padding and writes bytes to /dev/null for
    # optional Memcheck verification. Do not report an empty binary as a pass.
    return {suite: names for suite, names in REGRESSION_TESTS.items()
            if platform == 'linux' or suite != 'video_padding_sdk'}


def suite_tests(mode, platform):
    if mode == 'core':
        if platform not in ('linux', 'darwin', 'win32'):
            raise ValueError('no reviewed native core policy for this host')
        return CORE_TESTS
    if mode == 'regressions':
        return regression_tests(platform)
    if mode == 'advanced':
        return {'advanced_runtime': ADVANCED_TESTS}
    if mode == 'file':
        return {'grafton_ndi': tuple('file::sdk_tests::' + name for name in FILE_TESTS)}
    if mode == 'file-negative':
        return {'grafton_ndi': ('file::sdk_tests::native_file_rejects_standard_posix_before_load',)}
    if mode == 'file-platform-negative':
        return {'grafton_ndi': ('file::sdk_tests::native_file_rejects_windows_x86_before_load',)}
    raise ValueError('unknown native suite mode')


def core_ignored_tests(platform, target, selection):
    """All compiled ignored names, including reviewed manual/non-core tests."""
    feature_flags(selection)  # Validate the closed feature-selection vocabulary.
    features = None if selection == 'all' else set(FEATURE_SETS[int(selection)].split(','))
    enabled = lambda name: features is None or name in features
    suites = {suite: list(tests) for suite, tests in suite_tests('core', platform).items()}
    suites['grafton_ndi'].append('frames::tests::video_padding_throughput')
    if enabled('image-encoding'):
        suites['grafton_ndi'].append('frames::tests::image_encoding_resources')
    if enabled('file-io'):
        suites['grafton_ndi'] += ['file::sdk_tests::' + name for name in FILE_TESTS]
        if platform in ('linux', 'darwin'):
            suites['grafton_ndi'].append('file::sdk_tests::native_file_rejects_standard_posix_before_load')
        elif target == 'i686-pc-windows-msvc':
            suites['grafton_ndi'].append('file::sdk_tests::native_file_rejects_windows_x86_before_load')
    suites['diagnose_windows_ci'].append('test_ndi_init_with_timeout')
    return suites


def confinement_kind():
    try:
        return {'linux': 'linux-network-namespace', 'darwin': 'macos-pf-offline',
                'win32': 'windows-firewall'}[sys.platform]
    except KeyError:
        raise ValueError('no reviewed native network confinement for this host') from None


def test_executables(records, suites, target_root):
    binaries = {}
    for record in records:
        if (record.get('reason') != 'compiler-artifact' or not record.get('executable')
                or not record.get('profile', {}).get('test')):
            continue
        suite = record.get('target', {}).get('name')
        if suite not in suites or suite in binaries:
            raise ValueError('unexpected or duplicate native test executable')
        binaries[suite] = target_file(record['executable'], target_root)
    if set(binaries) != set(suites):
        raise ValueError('a required native test executable was not built')
    return binaries


def capture(args, *, env=None, cwd=None, timeout=180):
    # Rust/Cargo emit UTF-8, regardless of the Windows host code page. Preserve
    # unrelated malformed diagnostic bytes as escapes without losing either
    # reader thread or changing the exact ASCII test-summary requirement.
    return subprocess.run(args, cwd=cwd, env=env, capture_output=True, text=True,
                          encoding='utf-8', errors='backslashreplace', timeout=timeout)


def private_output(stream, text):
    # CellRun captures these streams as private UTF-8 logs. Redirected Windows
    # Python streams would otherwise try to re-encode valid UTF-8 as CP1252.
    if hasattr(stream, 'reconfigure'):
        stream.reconfigure(encoding='utf-8', errors='backslashreplace')
    stream.write(text if text.endswith('\n') else text + '\n')


def forward_failed_child(result):
    # CellRun captures this helper's output into an ephemeral private log and
    # exports only its bounded diagnostic summary. Standalone native.py output
    # is also private: never upload it or stream it to a public CI console.
    if result.stdout:
        private_output(sys.stdout, result.stdout)
    if result.stderr:
        private_output(sys.stderr, result.stderr)


def require_one_pass(result, name):
    summaries = re.findall(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;', result.stdout)
    if result.returncode or summaries != [('1', '0', '0')]:
        forward_failed_child(result)
        raise ValueError(f'{name}: required native test failed, was skipped, or did not execute exactly once')


def write_test_log(path, result, name):
    try:
        path.write_text(result.stdout + result.stderr, encoding='utf-8')
    except Exception as error:
        # This is a harness failure, not evidence that the SDK assertion failed.
        # CellRun independently checks the public test name before exporting it.
        print('NATIVE_LOG_WRITE_FAILURE=' + json.dumps({
            'test': name, 'kind': 'encoding' if isinstance(error, UnicodeError) else
            'io' if isinstance(error, OSError) else 'other'},
            sort_keys=True))
        raise


def verify_ignored_tests(binary, names, scratch, env, output):
    result = isolate(binary, ['--list', '--ignored', '--format', 'terse'], scratch, env)
    output.write_text(result.stdout + result.stderr, encoding='utf-8')
    observed = re.findall(r'^([A-Za-z_]\w*(?:::[A-Za-z_]\w*)*): test$', result.stdout, re.M)
    if result.returncode or len(observed) != len(set(observed)) or set(observed) != set(names):
        forward_failed_child(result)
        raise ValueError('compiled ignored tests differ from the exact reviewed regression suite')
    return result


def confinement_evidence(result, binary, operation):
    from macos_native import validate_receipt
    receipt = getattr(result, 'confinement_evidence', None)
    return validate_receipt(receipt, operation=operation, binary_sha256=digest(binary))


def isolate(binary, args, scratch, env, *, timeout=180):
    if sys.platform == 'linux':
        hostname = socket.gethostname()
        hosts = scratch / 'hosts'
        aliases = sorted({hostname, hostname.lower(), hostname.upper(), hostname + '.', hostname.lower() + '.', hostname.upper() + '.'})
        hosts.write_text('127.0.0.1 localhost ' + ' '.join(aliases) + '\n::1 localhost\n', encoding='utf-8')
        return capture(['bwrap', '--unshare-user', '--unshare-net', '--ro-bind', '/', '/',
            '--tmpfs', '/run', '--tmpfs', '/tmp', '--dev-bind', '/dev', '/dev', '--proc', '/proc',
            '--bind', str(scratch), str(scratch), '--ro-bind', str(hosts), '/etc/hosts', str(binary), *args],
            env=env, timeout=timeout)
    if sys.platform == 'darwin':
        from macos_native import execute
        operation = ('list' if args == ['--list', '--ignored', '--format', 'terse'] else
                     'sandbox-diagnostic' if args == [str(ROOT / 'ci/sandbox_probe.py')] else args[0])
        return execute(binary, args, scratch, env, operation=operation, timeout=timeout)
    if sys.platform == 'win32':
        # Windows Defender Firewall excludes loopback; process rules reject all
        # external traffic from this exact test executable in both directions.
        env = dict(env, NDI_NATIVE_BINARY=str(binary), NDI_NATIVE_ARGUMENTS=json.dumps(args),
                   NDI_NATIVE_RULE='grafton-ndi-ci-' + uuid.uuid4().hex)
        script = '''$ErrorActionPreference = 'Stop'
$name = $env:NDI_NATIVE_RULE
try {
  Set-NetFirewallProfile -Profile Domain,Public,Private -Enabled True
  New-NetFirewallRule -DisplayName $name -Direction Outbound -Action Block -Program $env:NDI_NATIVE_BINARY -Profile Any | Out-Null
  New-NetFirewallRule -DisplayName $name -Direction Inbound -Action Block -Program $env:NDI_NATIVE_BINARY -Profile Any | Out-Null
  $arguments = ConvertFrom-Json $env:NDI_NATIVE_ARGUMENTS
  & $env:NDI_NATIVE_BINARY @arguments
  $result = $LASTEXITCODE
} finally {
  Get-NetFirewallRule -DisplayName $name -ErrorAction SilentlyContinue | Remove-NetFirewallRule
}
exit $result
'''
        return capture(['pwsh', '-NoProfile', '-Command', script], env=env, timeout=timeout)
    raise ValueError('no reviewed native network confinement for this host')


def diagnose_sandbox(scratch, env, output):
    """Explain a failed native test without changing its result or confinement."""
    if sys.platform != 'darwin':
        return
    try:
        result = isolate(Path(sys.executable), [str(ROOT / 'ci/sandbox_probe.py')],
                         scratch, env, timeout=30)
        (output / 'sandbox-probe.log').write_text(result.stdout + result.stderr, encoding='utf-8')
        if result.returncode:
            print('SANDBOX_PROBE_FAILURE=execution-failed')
            return
        if len(result.stdout.encode('utf-8')) > 16384:
            raise ValueError('oversized sandbox diagnostic report')
        report = validate_report(json.loads(result.stdout))
        # This output is captured privately by CellRun. Its diagnostic parser
        # independently validates the closed operation/status/errno vocabulary.
        print('SANDBOX_PROBE=' + json.dumps(report, sort_keys=True))
    except subprocess.TimeoutExpired:
        print('SANDBOX_PROBE_FAILURE=timed-out')
    except Exception:
        # Diagnostic failure must never replace or waive the required test's
        # original failure, nor publish an exception carrying private paths.
        print('SANDBOX_PROBE_FAILURE=invalid-or-unavailable')


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--mode', choices=['core', 'regressions', 'advanced', 'file', 'file-negative', 'file-platform-negative'], required=True)
    p.add_argument('--feature-set', choices=['all', *map(str, range(len(FEATURE_SETS)))], default='all')
    p.add_argument('--target', required=True)
    p.add_argument('--output', type=Path, required=True)
    args = p.parse_args()
    if args.mode != 'core' and args.feature_set != 'all':
        raise ValueError('only core mode accepts a specific Cargo feature selection')
    args.output.mkdir(parents=True)
    env = child_environment()
    suites = suite_tests(args.mode, sys.platform)
    build = ['cargo', 'test', '--locked', *feature_flags(args.feature_set),
             '--no-run', '--message-format=json', '--target', args.target]
    for suite in suites:
        build += ['--lib'] if suite == 'grafton_ndi' else ['--test', suite]
    result = capture(build, cwd=ROOT, env=env, timeout=1800)
    (args.output / 'build.log').write_text(result.stdout + result.stderr, encoding='utf-8')
    if result.returncode:
        forward_failed_child(result)
        raise ValueError('native test executable build failed')
    target_directory = Path(env.get('CARGO_TARGET_DIR', str(ROOT / 'target')))
    if not target_directory.is_absolute():
        target_directory = ROOT / target_directory
    binaries = test_executables(messages(result.stdout), suites, target_directory / args.target)
    env['GRAFTON_NDI_ISOLATED_NETWORK'] = '1'
    env['GRAFTON_NDI_RUNTIME'] = env['NDI_EXPECTED_RUNTIME']
    scratch = args.output / 'scratch'; scratch.mkdir()
    if args.mode == 'file':
        fixture = scratch / 'fixture.mkv'
        subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-f', 'lavfi', '-i',
            'testsrc2=size=320x180:rate=30', '-t', '1', '-c:v', 'libx264', '-g', '15',
            '-pix_fmt', 'yuv420p', '-y', str(fixture)], check=True, env=env, timeout=60)
        env.update(GRAFTON_NDI_FILE_FIXTURE=str(fixture), GRAFTON_NDI_FILE_SCRATCH=str(scratch))
    elif args.mode == 'file-negative':
        env['GRAFTON_NDI_TEST_STANDARD_RUNTIME'] = env['NDI_EXPECTED_RUNTIME']
    evidence = []
    listings = []
    inventory = (core_ignored_tests(sys.platform, args.target, args.feature_set)
                 if args.mode == 'core' else suites)
    for suite, tests in suites.items():
        binary = binaries[suite]
        binary_hash = digest(binary)
        if args.mode in ('core', 'regressions'):
            listed = verify_ignored_tests(binary, inventory[suite], scratch, env, args.output / (suite + '-list.log'))
            if sys.platform == 'darwin':
                listings.append({'suite': suite, 'binary_sha256': binary_hash,
                                 'confinement_evidence': confinement_evidence(listed, binary, 'list')})
        for name in tests:
            result = isolate(binary, [name, '--exact', '--ignored', '--test-threads=1', '--nocapture'], scratch, env)
            log = args.output / (suite + '-' + name.replace('::', '-') + '.log')
            write_test_log(log, result, name)
            try:
                require_one_pass(result, name)
            except ValueError:
                # Preserve the reviewed test identity for the parent's bounded
                # diagnostics even when the native process aborts before libtest
                # can name its failing test. Raw output stays in private capture.
                print(f'test {name} ... FAILED')
                diagnose_sandbox(scratch, env, args.output)
                raise
            item = {'suite': suite, 'test': name, 'status': 'passed', 'passed': 1, 'ignored': 0,
                    'binary_sha256': binary_hash, 'log_sha256': digest(log)}
            if sys.platform == 'darwin':
                item['confinement_evidence'] = confinement_evidence(result, binary, name)
            evidence.append(item)
    report = {'mode': args.mode, 'feature_set': args.feature_set,
              'confinement': confinement_kind(), 'tests': evidence,
              'test_count': len(evidence), 'suite_count': len(suites)}
    if len(binaries) == 1:
        report['binary_sha256'] = digest(next(iter(binaries.values())))
    if sys.platform == 'darwin':
        report['listings'] = listings
    write_json(args.output / 'native.json', report)

if __name__ == '__main__':
    main()
