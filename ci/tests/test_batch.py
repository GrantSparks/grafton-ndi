"""Grouped execution, cache tampering, source binding and upload boundaries."""
from contextlib import ExitStack, contextmanager, redirect_stderr, redirect_stdout
from copy import deepcopy
import io
import json
import os
from pathlib import Path
import sys
import subprocess
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import batch
import run
from common import digest, plan_digest, read_json
from evidence_fixture import evidence_fixture
from provision import resolved_package, sanitized_receipt
from test_provision import fixture_archive, fixture_catalog, KEY


@contextmanager
def matching_source(plan, directory):
    source = directory / 'source'
    source.mkdir()
    subprocess.run(['git', 'init', '--quiet', str(source)], check=True)
    (source / 'Cargo.lock').write_text('frozen synthetic dependency resolution\n')
    plan['lockfile_sha256'] = digest(source / 'Cargo.lock')
    identity = {key: plan[key] for key in ('commit', 'tree', 'dirty')}
    with ExitStack() as stack:
        stack.enter_context(patch.object(batch, 'ROOT', source))
        for module in (batch, run):
            stack.enter_context(patch.object(module, 'git_identity', return_value=identity))
            stack.enter_context(patch.object(module, 'suite_fingerprint', return_value=plan['suite_fingerprint']))
        yield source


class BatchPolicyTests(unittest.TestCase):
    def test_plan_and_actual_source_mutations_fail_before_execution(self):
        plan = evidence_fixture()[0]
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name)
            with matching_source(plan, directory) as source:
                group = plan['groups'][0]['id']
                self.assertEqual(batch.frozen_group(plan, group)[1], plan_digest(plan))
                for field, value in [('scope', 'extended'), ('commit', '1' * 40),
                                     ('tree', '2' * 40), ('suite_fingerprint', '3' * 64),
                                     ('lockfile_sha256', '4' * 64), ('dirty', True)]:
                    altered = deepcopy(plan); altered[field] = value
                    with self.subTest(field=field), self.assertRaises(ValueError):
                        batch.frozen_group(altered, group)
                for mutate in (lambda p: p['cells'].pop(),
                               lambda p: p['groups'][0]['cells'].append(p['groups'][0]['cells'][0]),
                               lambda p: p['groups'][0]['availability_packages'].append('unexpected'),
                               lambda p: p['packages'][KEY].update(sha256='0' * 64)):
                    altered = deepcopy(plan); mutate(altered)
                    with self.assertRaises(ValueError):
                        batch.frozen_group(altered, group)
                with self.assertRaises(ValueError):
                    batch.frozen_group(plan, '../../private')
                (source / 'Cargo.lock').write_text('changed after planning')
                with patch.object(batch, 'CellRun') as worker, self.assertRaises(ValueError):
                    batch.execute_group(plan, group, directory / 'work')
                worker.assert_not_called()
                self.assertFalse((directory / 'work').exists())

    def test_target_cache_separates_artifact_pairs_and_compilation_context(self):
        plan = evidence_fixture()[0]
        cell = next(c for c in plan['cells'] if c['profile'] == 'x86_64-linux-gnu' and c['suite'] == 'native')
        wanted = batch.target_cache_key(plan, cell)
        changed = dict(cell, suite='features')
        self.assertEqual(batch.target_cache_key(plan, changed), wanted)
        for field, value in [('profile', 'other'), ('target', 'other'), ('edition', 'advanced'),
                             ('toolchain', 'other'), ('build_std', True),
                             ('header_package', '6.3.2-standard-linux'), ('runtime_package', '6.3.2-standard-linux')]:
            with self.subTest(field=field):
                self.assertNotEqual(batch.target_cache_key(plan, dict(cell, **{field: value})), wanted)
        for field in ('commit', 'tree', 'lockfile_sha256', 'suite_fingerprint'):
            changed_plan = dict(plan, **{field: 'changed'})
            self.assertNotEqual(batch.target_cache_key(changed_plan, cell), wanted)

    def test_evidence_never_contains_work_and_stale_output_is_rejected(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name); work = root / 'work'
            for evidence in (root, work, work / 'cells'):
                with self.subTest(evidence=evidence), self.assertRaises(ValueError):
                    batch.prepare_outputs(work, evidence)
            work.mkdir(); (work / 'stale-result.json').write_text('{}')
            with self.assertRaises(ValueError):
                batch.prepare_outputs(work, work / 'evidence')

    def test_real_failed_child_does_not_stop_remaining_cells_or_publish_raw_data(self):
        plan = evidence_fixture()[0]
        group = next(g for g in plan['groups'] if g['id'] == 'quality-linux')
        visited, targets, envs = [], [], []
        private = 'PRIVATE_SDK_PAYLOAD_AND_LOCATOR'
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name)
            class FixtureRun(run.CellRun):
                def run(self, args):
                    visited.append(self.cell['id']); targets.append(self.target_directory)
                    envs.append(dict(self.env))
                    self.env['BATCH_PREVIOUS_CELL'] = private
                    self.command('fixture-child', [sys.executable, '-c',
                        'import sys; print(' + repr(private) + '); sys.exit(' +
                        ('1' if len(visited) == 1 else '0') + ')'], cwd=directory)
                    self.result['status'] = 'passed'
            class FixtureCache:
                failures = {}
                def __init__(self, *args): pass
                def obtain(self, key):
                    package = plan['packages'][key]
                    return {'schema': 2, 'package': key, 'artifact_id': package['artifact_id'],
                            'archive_sha256': package['sha256'], 'inventory_sha256': 'f' * 64,
                            'root': str(directory / private)}
            output = io.StringIO()
            with matching_source(plan, directory), patch.object(batch, 'CellRun', FixtureRun), \
                    patch.object(batch, 'VerifiedSdkCache', FixtureCache), \
                    redirect_stdout(output), redirect_stderr(output):
                status = batch.execute_group(plan, group['id'], directory / 'work')
            self.assertEqual(status, 1)
            self.assertEqual(visited, group['cells'])
            self.assertTrue(all('BATCH_PREVIOUS_CELL' not in env for env in envs))
            self.assertNotIn('BATCH_PREVIOUS_CELL', os.environ)
            evidence = directory / 'work/evidence'
            files = list(evidence.rglob('*'))
            self.assertTrue(all(p.name in ('result.json', 'availability.json') for p in files if p.is_file()))
            records = [read_json(evidence / 'cells' / key / 'result.json') for key in visited]
            self.assertEqual([r['status'] for r in records], ['failed'] + ['passed'] * (len(records) - 1))
            self.assertTrue(all(r['plan_sha256'] == plan_digest(plan) and r['group'] == group['id'] for r in records))
            exported = ''.join(p.read_text() for p in files if p.is_file())
            self.assertNotIn(private, exported + output.getvalue())
            self.assertIn(private, (directory / 'work/cells' / visited[0] / 'fixture-child.log').read_text())
            self.assertEqual(len({str(p) for p in targets}), len({batch.target_cache_key(plan, next(c for c in plan['cells'] if c['id'] == k)) for k in visited}))

    def test_availability_failure_is_bounded_and_does_not_skip_group_cells(self):
        plan = evidence_fixture()[0]
        group = next(g for g in plan['groups'] if g['availability_packages'])
        visited = []
        class FixtureRun(run.CellRun):
            def run(self, args):
                visited.append(self.cell['id']); self.result['status'] = 'passed'
        class FailingCache:
            failures = {}
            def __init__(self, *args): pass
            def obtain(self, key): raise ValueError('private-source-token-and-path')
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name)
            with matching_source(plan, directory), patch.object(batch, 'CellRun', FixtureRun), \
                    patch.object(batch, 'VerifiedSdkCache', FailingCache), redirect_stdout(io.StringIO()):
                self.assertEqual(batch.execute_group(plan, group['id'], directory / 'work'), 1)
            self.assertEqual(visited, group['cells'])
            records = list((directory / 'work/evidence/availability').rglob('availability.json'))
            self.assertEqual(len(records), len(group['availability_packages']))
            for record in records:
                data = read_json(record)
                self.assertEqual(data['status'], 'unavailable')
                self.assertEqual(data['group'], group['id'])
                self.assertNotIn('private-source-token-and-path', record.read_text())

    def test_final_successful_cell_cannot_change_frozen_inputs(self):
        plan = evidence_fixture()[0]
        group = next(g for g in plan['groups'] if g['id'] == 'quality-linux')
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name)
            class MutatingRun(run.CellRun):
                def run(self, args):
                    self.result['status'] = 'passed'
                    if self.cell['id'] == group['cells'][-1]:
                        (directory / 'source/Cargo.lock').write_text('changed during final command')
            with matching_source(plan, directory), patch.object(batch, 'CellRun', MutatingRun), \
                    patch.object(batch, 'VerifiedSdkCache'), redirect_stdout(io.StringIO()):
                self.assertEqual(batch.execute_group(plan, group['id'], directory / 'work'), 1)
            final = read_json(directory / 'work/evidence/cells' / group['cells'][-1] / 'result.json')
            self.assertEqual(final['status'], 'failed')
            self.assertIn('failure', final)
            private = read_json(directory / 'work/cells' / group['cells'][-1] / 'result.json')
            self.assertEqual(final, private)

    def test_cell_keeps_bounded_sdk_failure_even_when_group_does_not_own_receipt(self):
        plan = evidence_fixture()[0]
        group = next(g for g in plan['groups'] if g['id'] == 'build-apple-devices')
        self.assertEqual(group['availability_packages'], [])
        class FixtureRun(run.CellRun):
            def run(self, args):
                self.sdk('headers', self.cell['header_package'], None)
                self.result['status'] = 'passed'
        class TransientCache:
            def __init__(self, *args): self.failures = {}; self.calls = 0
            def obtain(self, key):
                self.calls += 1
                if self.calls == 1:
                    self.failures[key] = {'schema': 1, 'reasons': ['download-failed']}
                    raise ValueError('private locator and token must not appear')
                self.failures.pop(key, None)
                package = plan['packages'][key]
                return {'schema': 2, 'package': key, 'root': '/private-sdk-root',
                        'artifact_id': package['artifact_id'], 'archive_sha256': package['sha256']}
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name)
            with matching_source(plan, directory), patch.object(batch, 'CellRun', FixtureRun), \
                    patch.object(batch, 'VerifiedSdkCache', TransientCache), \
                    redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
                self.assertEqual(batch.execute_group(plan, group['id'], directory / 'work'), 1)
            records = [read_json(directory / 'work/evidence/cells' / key / 'result.json') for key in group['cells']]
            self.assertEqual(records[0]['provisioning_failure']['diagnostics']['reasons'], ['download-failed'])
            self.assertEqual(records[0]['status'], 'failed')
            self.assertTrue(all(r['status'] == 'passed' and 'provisioning_failure' not in r for r in records[1:]))
            self.assertNotIn('private locator', json.dumps(records))


class SdkCacheTests(unittest.TestCase):
    def test_actual_cli_reuses_verified_installation_and_rejects_changed_payload(self):
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name)
            archive = fixture_archive(directory); catalog = fixture_catalog(archive)
            environment = dict(os.environ, GITHUB_ACTIONS='false',
                NDI_SDK_SOURCES_JSON=json.dumps({'schema': 1, 'packages': {KEY: {'archive': str(archive)}}}),
                NDI_HEADER_URL='https://unused.invalid/private-header', NDI_RUNTIME_URL='https://unused.invalid/private-runtime',
                NDI_SDK_SOURCE_URL='https://unused.invalid/private-generic')
            cache = batch.VerifiedSdkCache(directory / 'cache', catalog, environment)
            first = cache.obtain(KEY)
            archive.unlink()  # A verified installed hit must not reacquire bytes.
            second = cache.obtain(KEY)
            self.assertEqual(first, second)
            artifact = resolved_package(KEY, catalog)['artifact_id']
            self.assertEqual(Path(first['root']).parents[1].name, plan_digest({'artifact_id': artifact}))
            self.assertNotIn(str(directory), json.dumps(sanitized_receipt(first)))
            header = Path(first['root']) / 'include/Processing.NDI.Lib.h'
            header.write_text('mutated SDK header')
            with self.assertRaisesRegex(ValueError, 'inventory verification failed'):
                cache.obtain(KEY)
            self.assertEqual(header.read_text(), 'mutated SDK header')
            self.assertIn(KEY, cache.failures)

    def test_per_package_hash_override_must_match_reviewed_catalog(self):
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name); archive = fixture_archive(directory)
            cache = batch.VerifiedSdkCache(directory / 'cache', fixture_catalog(archive),
                {'NDI_SDK_CURRENT_STANDARD_LINUX_SHA256': '0' * 64})
            with patch.object(batch.subprocess, 'run') as child, self.assertRaises(ValueError):
                cache.obtain(KEY)
            child.assert_not_called()


class RustSetupTests(unittest.TestCase):
    HOST = 'x86_64-apple-darwin'
    IOS = 'x86_64-apple-ios'

    class Fixture:
        def __init__(self, host, target, state, *, build_std=False):
            self.cell = {'toolchain': 'fixture', 'target': target, 'build_std': build_std}
            self.result = {}; self.commands = []; self.state = state; self.host = host

        def command(self, label, args, *, capture=False):
            self.commands.append((label, list(args)))
            if args == ['rustc', '-vV']:
                return self.state['compiler']
            if args[:3] == ['rustup', 'component', 'list']:
                return '\n'.join(sorted(self.state['components'])) + '\n'
            if args[:3] == ['rustup', 'target', 'list']:
                return '\n'.join(sorted(self.state['targets'])) + '\n'
            if not self.state['network']:
                raise ValueError('fixture Rust download failed')
            if args[:3] == ['rustup', 'toolchain', 'install']:
                component = args[-1]
                self.state['components'].add(component if component == 'rust-src' else component + '-' + self.host)
            elif args[:3] == ['rustup', 'target', 'add']:
                self.state['targets'].add(args[-1])
            elif args[:3] == ['rustup', 'component', 'add']:
                self.state['components'].add(args[-1] if args[-1] == 'rust-src' else args[-1] + '-' + self.host)
            else:
                raise AssertionError(args)
            return ''

    def state(self):
        return {'compiler': 'rustc fixture\nhost: ' + self.HOST + '\nrelease: fixture\n',
                'components': {'rustc-' + self.HOST}, 'targets': {self.HOST}, 'network': True}

    def test_successful_configuration_reuses_local_setup_when_network_fails(self):
        cache = batch.VerifiedRustSetup(); state = self.state()
        first = self.Fixture(self.HOST, self.IOS, state)
        cache.ensure(first)
        self.assertFalse(first.result['rust_setup']['reused'])
        state['network'] = False
        for target in (self.IOS, self.HOST):
            later = self.Fixture(self.HOST, target, state)
            cache.ensure(later)
            self.assertTrue(later.result['rust_setup']['reused'])
            self.assertEqual(first.result['rust_setup']['compiler_identity_sha256'], later.result['rust_setup']['compiler_identity_sha256'])
            self.assertFalse(any(label.startswith('install-') for label, _ in later.commands))
            self.assertEqual({label for label, _ in later.commands},
                             {'rustc-version', 'verify-rust-components', 'verify-rust-targets'})

    def test_nightly_build_std_reuses_verified_source_component_without_binary_target(self):
        cache = batch.VerifiedRustSetup(); state = self.state()
        first = self.Fixture(self.HOST, 'x86_64-apple-tvos', state, build_std=True)
        cache.ensure(first); state['network'] = False
        later = self.Fixture(self.HOST, 'x86_64-apple-tvos', state, build_std=True)
        cache.ensure(later)
        self.assertTrue(later.result['rust_setup']['reused'])
        self.assertEqual(later.result['rust_setup']['component'], 'rust-src')
        self.assertIsNone(later.result['rust_setup']['target'])
        self.assertFalse(any(label.startswith('install-') for label, _ in later.commands))

    def test_compiler_component_and_target_mutations_never_trigger_silent_repair(self):
        for mutation in ('compiler', 'component', 'target'):
            with self.subTest(mutation=mutation):
                cache = batch.VerifiedRustSetup(); state = self.state()
                cache.ensure(self.Fixture(self.HOST, self.IOS, state))
                state['network'] = False
                if mutation == 'compiler': state['compiler'] += 'changed compiler identity\n'
                elif mutation == 'component': state['components'].remove('clippy-' + self.HOST)
                else: state['targets'].remove(self.IOS)
                later = self.Fixture(self.HOST, self.IOS, state)
                with self.assertRaises(ValueError): cache.ensure(later)
                self.assertNotIn('rust_setup', later.result)
                self.assertFalse(any(label.startswith('install-') for label, _ in later.commands))

    def test_new_target_install_failure_stays_failed_and_is_not_cached(self):
        cache = batch.VerifiedRustSetup(); state = self.state()
        cache.ensure(self.Fixture(self.HOST, self.HOST, state))
        state['network'] = False
        failed = self.Fixture(self.HOST, self.IOS, state)
        with self.assertRaisesRegex(ValueError, 'download failed'): cache.ensure(failed)
        self.assertNotIn('rust_setup', failed.result)
        self.assertNotIn(self.IOS, cache.toolchains['fixture']['targets'])
        self.assertEqual([label for label, _ in failed.commands if label.startswith('install-')], ['install-target'])
        state['network'] = True
        retry = self.Fixture(self.HOST, self.IOS, state); cache.ensure(retry)
        self.assertFalse(retry.result['rust_setup']['reused'])
        self.assertIn(self.IOS, cache.toolchains['fixture']['targets'])

    def test_intervening_host_cell_cannot_forget_previously_verified_target(self):
        cache = batch.VerifiedRustSetup(); state = self.state()
        cache.ensure(self.Fixture(self.HOST, self.IOS, state))
        state['targets'].remove(self.IOS)
        host = self.Fixture(self.HOST, self.HOST, state)
        with self.assertRaisesRegex(ValueError, 'lost an installed target'):
            cache.ensure(host)
        self.assertIn(self.IOS, cache.toolchains['fixture']['targets'])
        revisit = self.Fixture(self.HOST, self.IOS, state)
        with self.assertRaises(ValueError): cache.ensure(revisit)
        self.assertFalse(any(label.startswith('install-') for label, _ in revisit.commands))

    def test_incomplete_initial_install_is_not_cached_and_inventory_is_strict(self):
        cache = batch.VerifiedRustSetup(); state = self.state(); state['network'] = False
        with self.assertRaises(ValueError): cache.ensure(self.Fixture(self.HOST, self.HOST, state))
        self.assertEqual(cache.toolchains, {})
        self.assertEqual(cache.inventory('rust-std-thumbv8m.main-none-eabi\n'),
                         {'rust-std-thumbv8m.main-none-eabi'})
        for text in ('', 'rust-src\nrust-src\n', 'rust-src private-locator\n', '../rust-src\n'):
            with self.subTest(text=text), self.assertRaises(ValueError):
                cache.inventory(text)


if __name__ == '__main__':
    unittest.main()
