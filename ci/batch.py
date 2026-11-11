#!/usr/bin/env python3
"""Execute a frozen group serially; export only individual bounded evidence."""
from __future__ import annotations
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from types import SimpleNamespace

from aggregate import validate_plan
from common import ROOT, digest, git_identity, plan_digest, read_json, secret_key, suite_fingerprint, write_json
from diagnostics import summarize_exception, summarize_log
from provision import RECEIPT_SCHEMA, resolved_package, sanitized_receipt
from run import CellRun, execute_cell


def bounded_exception(error):
    try:
        return summarize_exception(error, ROOT)
    except (ValueError, OSError, subprocess.SubprocessError):
        return {'schema': 1, 'reasons': ['diagnostic-summary-unavailable']}


def validate_source(plan):
    identity = git_identity()
    if identity.get('dirty') or any(identity.get(key) != plan.get(key) for key in ('commit', 'tree')):
        raise ValueError('batch source differs from the clean frozen plan')
    if suite_fingerprint() != plan.get('suite_fingerprint'):
        raise ValueError('batch suite differs from the frozen plan')
    lock = ROOT / 'Cargo.lock'
    if not lock.is_file() or digest(lock) != plan.get('lockfile_sha256'):
        raise ValueError('batch dependency lockfile differs from the frozen plan')


def frozen_group(plan, group_id):
    identity = validate_plan(plan)
    validate_source(plan)
    groups = {group['id']: group for group in plan['groups']}
    if group_id not in groups:
        raise ValueError('unknown group in frozen plan')
    return groups[group_id], identity


class VerifiedSdkCache:
    """Job-local installations keyed by immutable identity; reverify every use."""
    def __init__(self, output, packages, environment=None):
        self.output = output.resolve()
        self.packages = packages
        self.environment = dict(os.environ if environment is None else environment)
        self.catalog = self.output / 'catalog.json'
        write_json(self.catalog, packages)
        self.failures = {}

    def obtain(self, key):
        prefix = 'NDI_SDK_' + secret_key(key)
        override = self.environment.get(prefix + '_SHA256')
        package = resolved_package(key, self.packages, override)
        # Full artifact names plus the vendor's longest Windows documentation
        # paths exceed MAX_PATH during staging. Use a full (untruncated) digest;
        # the receipt still enforces the complete human-readable artifact ID.
        cache_id = plan_digest({'artifact_id': package['artifact_id']})
        destination = self.output / cache_id
        log = self.output / (cache_id + '.log')
        env = dict(self.environment)
        # Generic single-cell selectors must never carry over to another SDK.
        for name in ('NDI_HEADER_URL', 'NDI_HEADER_SHA256', 'NDI_RUNTIME_URL', 'NDI_RUNTIME_SHA256'):
            env.pop(name, None)
        env['NDI_SDK_SOURCE_URL'] = self.environment.get(prefix + '_URL', '')
        env['NDI_SDK_SOURCE_SHA256'] = override or ''
        with log.open('w', encoding='utf-8') as stream:
            process = subprocess.run([sys.executable, str(ROOT / 'ci/provision.py'),
                '--package', key, '--catalog', str(self.catalog), '--output', str(destination),
                '--reuse'], cwd=ROOT, env=env, stdout=stream, stderr=subprocess.STDOUT, text=True)
        if process.returncode:
            try:
                self.failures[key] = summarize_log(log, ROOT)
            except (ValueError, OSError, subprocess.SubprocessError):
                self.failures[key] = {'schema': 1, 'reasons': ['diagnostic-summary-unavailable']}
            raise ValueError('SDK acquisition or inventory verification failed')
        # The child verified the full inventory immediately before returning.
        # Reading its receipt here avoids hashing multi-GB packages twice per use.
        receipt = read_json(destination / 'provision.json')
        relative = Path(receipt.get('root_relative', ''))
        root = (destination / relative).resolve()
        if (relative.is_absolute() or not root.is_relative_to(destination.resolve()) or not root.is_dir()
                or receipt.get('schema') != RECEIPT_SCHEMA or receipt.get('package') != key
                or receipt.get('artifact_id') != package['artifact_id']
                or receipt.get('archive_sha256') != package['sha256']):
            raise ValueError('verified SDK receipt identity differs from requested package')
        self.failures.pop(key, None)
        return dict(receipt, root=str(root))


class VerifiedRustSetup:
    """Reuse successful in-job setup only after checking local compiler and inventories."""
    def __init__(self):
        self.toolchains = {}

    @staticmethod
    def inventory(text):
        names = text.splitlines()
        if (not names or len(names) != len(set(names))
                or any(not re.fullmatch(r'[a-z0-9_]+(?:[.-][a-z0-9_]+)*', name) for name in names)):
            raise ValueError('Rust installed inventory is malformed')
        return set(names)

    def ensure(self, run):
        cell = run.cell
        toolchain = cell['toolchain']
        component = 'rust-src' if cell['build_std'] else 'clippy'
        previous = self.toolchains.get(toolchain)
        installed = previous is None
        if previous is None:
            run.command('install-rust', ['rustup', 'toolchain', 'install', toolchain,
                '--profile', 'minimal', '--component', component])
        compiler = run.command('rustc-version', ['rustc', '-vV'], capture=True)
        run.result['rustc'] = compiler
        compiler_digest = hashlib.sha256(compiler.encode('utf-8')).hexdigest()
        if previous and compiler_digest != previous['compiler_identity_sha256']:
            raise ValueError('Rust compiler identity SHA256 mismatch against verified batch configuration')
        host = re.findall(r'^host: ([a-z0-9_]+(?:-[a-z0-9_]+)*)$', compiler, re.MULTILINE)
        if len(host) != 1:
            raise ValueError('Rust compiler host identity is missing or ambiguous')
        required = 'rust-src' if cell['build_std'] else 'clippy-' + host[0]
        component_command = ['rustup', 'component', 'list', '--installed', '--toolchain', toolchain]
        components = self.inventory(run.command('verify-rust-components', component_command, capture=True))
        if previous and not previous['components'].issubset(components):
            raise ValueError('verified Rust component inventory lost an installed component')
        if required not in components:
            if previous is None or required in previous['components']:
                raise ValueError('verified Rust component is absent')
            run.command('install-rust-component', ['rustup', 'component', 'add', '--toolchain', toolchain, component])
            installed = True
            components = self.inventory(run.command('verify-rust-components-after-install', component_command, capture=True))
            if required not in components:
                raise ValueError('requested Rust component was not installed')
        target_command = ['rustup', 'target', 'list', '--installed', '--toolchain', toolchain]
        targets = self.inventory(run.command('verify-rust-targets', target_command, capture=True))
        if previous and not previous['targets'].issubset(targets):
            raise ValueError('verified Rust target inventory lost an installed target')
        target = None if cell['build_std'] else cell['target']
        if target is not None and target not in targets:
            if previous and target in previous['targets']:
                raise ValueError('verified Rust target is absent')
            run.command('install-target', ['rustup', 'target', 'add', '--toolchain', toolchain, target])
            installed = True
            targets = self.inventory(run.command('verify-rust-targets-after-install', target_command, capture=True))
            if target not in targets:
                raise ValueError('requested Rust target was not installed')
        self.toolchains[toolchain] = {'compiler_identity_sha256': compiler_digest,
                                     'components': components, 'targets': targets}
        run.result['rust_setup'] = {'toolchain': toolchain, 'compiler_identity_sha256': compiler_digest,
                                   'component': required, 'target': target, 'reused': not installed}


def target_cache_key(plan, cell):
    # Suite-only changes can reuse Cargo's feature-aware dependency graph. SDK
    # pairs, targets, editions and toolchains never share generated bindings.
    identity = {key: plan[key] for key in ('commit', 'tree', 'lockfile_sha256', 'suite_fingerprint')}
    identity['cell'] = {key: cell[key] for key in ('profile', 'target', 'edition', 'toolchain', 'build_std')}
    identity['artifacts'] = [plan['packages'][cell[role]]['artifact_id']
                             for role in ('header_package', 'runtime_package')]
    return plan_digest(identity)


def prepare_outputs(output, evidence):
    output, evidence = output.resolve(), evidence.resolve()
    if output.is_relative_to(ROOT.resolve()) or evidence.is_relative_to(ROOT.resolve()):
        raise ValueError('batch outputs must remain outside the source checkout')
    if (output.is_relative_to(evidence)
            or (evidence.is_relative_to(output) and evidence != output / 'evidence')):
        raise ValueError('evidence directory must not contain raw work or SDK installations')
    for path in (output, evidence):
        if path.exists() and (not path.is_dir() or any(path.iterdir())):
            raise ValueError('batch output and evidence directories must be fresh')
    output.mkdir(parents=True, exist_ok=True)
    evidence.mkdir(parents=True, exist_ok=True)
    return output, evidence


def execute_group(plan, group_id, output, *, evidence=None, install_toolchain=False):
    group, identity = frozen_group(plan, group_id)
    output, evidence = prepare_outputs(output, evidence or output / 'evidence')
    binding = {key: plan[key] for key in ('scope', 'commit', 'tree', 'dirty', 'suite_fingerprint', 'lockfile_sha256')}
    binding.update(group=group_id, plan_sha256=identity)
    cache = VerifiedSdkCache(output / 'sdks', plan['packages'])
    rust_setup = VerifiedRustSetup()
    failed = False
    for key in group['availability_packages']:
        try:
            receipt = dict(sanitized_receipt(cache.obtain(key)), status='available')
        except Exception as error:
            failed = True
            receipt = {'schema': RECEIPT_SCHEMA, 'package': key, 'status': 'unavailable',
                       'failure': cache.failures.get(key) or bounded_exception(error)}
        write_json(evidence / 'availability' / key / 'availability.json', dict(receipt, **binding))
    cells = {cell['id']: cell for cell in plan['cells']}
    for key in group['cells']:
        cell = cells[key]
        run = CellRun(cell, output / 'cells' / key, sdk_provider=cache.obtain,
                      target_directory=output / 'targets' / target_cache_key(plan, cell),
                      rust_setup=rust_setup.ensure)
        def sdk_for_cell(package, result=run.result):
            try:
                return cache.obtain(package)
            except Exception as error:
                result['provisioning_failure'] = {'package': package,
                    'diagnostics': cache.failures.get(package) or bounded_exception(error)}
                raise
        run.sdk_provider = sdk_for_cell
        # Retain actual per-cell source identity captured by CellRun. Do not
        # replace a changed checkout with the plan's desired identity.
        run.result.update(scope=plan['scope'], group=group_id, plan_sha256=identity)
        args = SimpleNamespace(header_sdk=None, runtime_sdk=None, install_toolchain=install_toolchain)
        try:
            validate_source(plan)
        except (ValueError, OSError, subprocess.SubprocessError) as error:
            run.result['failure'] = bounded_exception(error)
            write_json(run.output / 'result.json', run.result)
            status = 1
        else:
            status = execute_cell(run, args)
            if status == 0:
                try:
                    validate_source(plan)
                except (ValueError, OSError, subprocess.SubprocessError) as error:
                    run.result.update(status='failed', failure=bounded_exception(error))
                    write_json(run.output / 'result.json', run.result)
                    status = 1
        failed = bool(status) or failed
        write_json(evidence / 'cells' / key / 'result.json', run.result)
        print(f'{key}: {run.result["status"]}', flush=True)
    return 1 if failed else 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--plan', type=Path, required=True)
    parser.add_argument('--group', required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--evidence', type=Path, help='bounded upload directory; defaults to OUTPUT/evidence')
    parser.add_argument('--install-toolchain', action='store_true')
    args = parser.parse_args(argv)
    try:
        return execute_group(read_json(args.plan), args.group, args.output,
                             evidence=args.evidence, install_toolchain=args.install_toolchain)
    except Exception as error:
        print('Batch failed; bounded diagnostics: ' + json.dumps(bounded_exception(error), sort_keys=True), file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
