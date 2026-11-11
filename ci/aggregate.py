#!/usr/bin/env python3
"""Fail closed on missing, skipped, stale, or mismatched required CI evidence."""
from __future__ import annotations
import argparse
from pathlib import Path
import re
import sys
from provision import RECEIPT_SCHEMA
from common import ROOT, SCHEMA, digest, git_identity, native_configuration, native_modes, plan_digest, profiles, read_json, required_steps, suite_fingerprint, write_json
from native import suite_tests
from runtime_identity import validate_identity
from plan import make_availability, make_groups, make_plan, provisioning_status, UNSUPPORTED_PROFILES


def validate_plan(plan):
    """Validate the frozen policy against this checkout, not against its own claims."""
    if not isinstance(plan, dict) or type(plan.get('schema')) is not int or plan['schema'] != SCHEMA:
        raise ValueError('invalid validation plan schema')
    scope = plan.get('scope')
    if scope not in ('required', 'extended'):
        raise ValueError('unknown validation plan scope')
    cells = make_plan(profiles(), scope=scope)
    if plan.get('cells') != cells or plan.get('groups') != make_groups(cells, scope=scope):
        raise ValueError('validation plan weakens or changes the canonical cells or groups')
    packages, toolchains, issues = provisioning_status(
        cells, read_json(ROOT / 'ci/sdk-packages.json'), environment={})
    if issues or plan.get('packages') != packages or plan.get('toolchains') != toolchains:
        raise ValueError('validation plan differs from the reviewed package or toolchain catalog')
    if plan.get('availability') != make_availability(cells, packages):
        raise ValueError('validation plan changes the required SDK availability inventory')
    if plan.get('unsupported_profiles') != sorted(UNSUPPORTED_PROFILES):
        raise ValueError('validation plan changes the reviewed unsupported profiles')
    if plan.get('dirty') is not False:
        raise ValueError('a dirty or unidentified source tree cannot satisfy the release gate')
    for field, length in (('commit', 40), ('tree', 40), ('suite_fingerprint', 64), ('lockfile_sha256', 64)):
        if not isinstance(plan.get(field), str) or not re.fullmatch(r'[a-f0-9]{' + str(length) + '}', plan[field]):
            raise ValueError(f'validation plan lacks a frozen {field}')
    return plan_digest(plan)


def validate_record_identity(plan, record, group, fingerprint):
    if not isinstance(record, dict):
        raise ValueError('malformed validation evidence')
    for field in ('commit', 'tree', 'suite_fingerprint', 'lockfile_sha256', 'scope'):
        if record.get(field) != plan[field]:
            raise ValueError(f'stale or mixed-scope evidence: {field}')
    if (record.get('dirty') is not False or record.get('group') != group
            or record.get('plan_sha256') != fingerprint):
        raise ValueError('validation evidence belongs to a different plan, group or source')


def validate_checkout(plan, lockfile):
    """Bind an artifact set to the clean checkout executing this gate."""
    identity = git_identity()
    if identity.get('dirty') is not False or any(plan.get(key) != identity.get(key) for key in ('commit', 'tree')):
        raise ValueError('aggregate checkout differs from the clean planned source')
    if plan.get('suite_fingerprint') != suite_fingerprint():
        raise ValueError('aggregate validation suite differs from the frozen plan')
    if not lockfile.is_file() or plan.get('lockfile_sha256') != digest(lockfile):
        raise ValueError('aggregate dependency lockfile differs from the frozen plan')


def validate_native_report(cell, evidence, label):
    mode, selection = native_configuration(label)
    platform = next((value for prefix, value in (
        ('ubuntu-', 'linux'), ('macos-', 'darwin'), ('windows-', 'win32'))
        if cell['runner'].startswith(prefix)), None)
    if platform is None:
        raise ValueError('native regression runner has no reviewed applicability policy')
    suites = suite_tests(mode, platform)
    expected = {(suite, test) for suite, tests in suites.items() for test in tests}
    confinement = {'linux': 'linux-network-namespace', 'darwin': 'macos-pf-offline',
                   'win32': 'windows-firewall'}[platform]
    if (not isinstance(evidence, dict) or evidence.get('mode') != mode
            or evidence.get('feature_set') != selection
            or evidence.get('confinement') != confinement
            or type(evidence.get('test_count')) is not int or evidence['test_count'] != len(expected)
            or type(evidence.get('suite_count')) is not int or evidence['suite_count'] != len(suites)
            or not isinstance(evidence.get('tests'), list)
            or set(evidence) - {'mode', 'feature_set', 'confinement', 'test_count', 'suite_count',
                                'tests', 'binary_sha256', 'listings'}):
        raise ValueError('native regression execution or confinement evidence is absent')
    found, binaries = set(), {}
    for test in evidence['tests']:
        if not isinstance(test, dict):
            raise ValueError('native regression test evidence is malformed')
        key = (test.get('suite'), test.get('test'))
        fields = {'suite', 'test', 'status', 'passed', 'ignored', 'binary_sha256', 'log_sha256'}
        if platform == 'darwin':
            fields.add('confinement_evidence')
        if (set(test) != fields or any(not isinstance(value, str) for value in key)
                or key not in expected or key in found or test.get('status') != 'passed'
                or type(test.get('passed')) is not int or test['passed'] != 1
                or type(test.get('ignored')) is not int or test['ignored'] != 0
                or any(not isinstance(test.get(field), str) or not re.fullmatch(r'[a-f0-9]{64}', test[field])
                       for field in ('binary_sha256', 'log_sha256'))):
            raise ValueError('native regression test is missing, duplicate, skipped, failed or lacks identity')
        suite = test['suite']
        if suite in binaries and binaries[suite] != test['binary_sha256']:
            raise ValueError('native regression executable identity changed within a suite')
        binaries[suite] = test['binary_sha256']
        if platform == 'darwin':
            from macos_native import validate_receipt
            validate_receipt(test['confinement_evidence'], operation=test['test'],
                             binary_sha256=test['binary_sha256'])
        found.add(key)
    if found != expected:
        raise ValueError('required native regression tests did not all execute')
    if 'binary_sha256' in evidence and (len(binaries) != 1
            or evidence['binary_sha256'] != next(iter(binaries.values()))):
        raise ValueError('native regression report executable identity differs from its tests')
    if platform == 'darwin' and mode in ('core', 'regressions'):
        listings = evidence.get('listings')
        if not isinstance(listings, list) or len(listings) != len(suites):
            raise ValueError('native regression listing confinement evidence is absent')
        listed = set()
        for listing in listings:
            if (not isinstance(listing, dict)
                    or set(listing) != {'suite', 'binary_sha256', 'confinement_evidence'}
                    or not isinstance(listing.get('suite'), str)
                    or listing['suite'] not in suites or listing['suite'] in listed
                    or listing['binary_sha256'] != binaries[listing['suite']]):
                raise ValueError('native regression listing is unknown, duplicated or changed')
            validate_receipt(listing['confinement_evidence'], operation='list',
                             binary_sha256=listing['binary_sha256'])
            listed.add(listing['suite'])
    elif evidence.get('listings'):
        raise ValueError('unexpected native regression listing evidence')


def validate_regressions(cell, evidence):
    validate_native_report(cell, evidence, 'regressions')


def validate_cells(plan, records):
    """Check exact per-cell execution evidence; validate() is the complete gate."""
    expected = {cell['id']: cell for cell in plan['cells']}
    if not expected:
        raise ValueError('empty required matrix')
    if not plan.get('lockfile_sha256'):
        raise ValueError('release plan has no frozen dependency lockfile')
    if plan.get('dirty'):
        raise ValueError('a dirty source tree cannot satisfy the release gate')
    found = {}
    for record in records:
        key = record.get('cell', {}).get('id')
        if key not in expected:
            raise ValueError(f'unexpected result cell: {key}')
        if key in found:
            raise ValueError(f'duplicate result cell: {key}')
        if type(record.get('schema')) is not int or record['schema'] != SCHEMA or record.get('status') != 'passed':
            raise ValueError(f'{key}: required execution did not pass')
        for field in ('commit', 'tree', 'suite_fingerprint', 'lockfile_sha256'):
            if record.get(field) != plan.get(field):
                raise ValueError(f'{key}: stale or mismatched {field}')
        if record.get('dirty') or record.get('cell') != expected[key]:
            raise ValueError(f'{key}: source or cell policy differs')
        if record.get('coverage') != expected[key]['coverage']:
            raise ValueError(f'{key}: claimed coverage differs from the plan')
        for role in ('headers', 'runtime'):
            provision = record.get(role, {})
            if provision.get('local_override') or not provision.get('archive_sha256'):
                raise ValueError(f'{key}: {role} lacks verified archive provenance')
            package_key = expected[key]['header_package' if role == 'headers' else 'runtime_package']
            if provision.get('package') != package_key:
                raise ValueError(f'{key}: wrong {role} SDK package')
            package = plan.get('packages', {}).get(package_key, {})
            if provision.get('artifact_id') != package.get('artifact_id') or not provision.get('artifact_id'):
                raise ValueError(f'{key}: {role} immutable artifact identity differs from plan')
            if not re.fullmatch(r'[a-f0-9]{64}', provision.get('inventory_sha256', '')):
                raise ValueError(f'{key}: {role} installed inventory identity is absent')
            wanted = package.get('sha256')
            if not wanted or provision['archive_sha256'].lower() != wanted.lower():
                raise ValueError(f'{key}: {role} archive differs from frozen release plan')
        if expected[key].get('requires_toolchain'):
            wanted = plan.get('toolchains', {}).get(expected[key]['profile'])
            if (not isinstance(wanted, dict) or wanted.get('provider') != 'public-gnu'
                    or record.get('toolchain_provider') != wanted['provider']
                    or record.get('toolchain_recipe_sha256') != wanted.get('recipe_sha256')
                    or not re.fullmatch(r'[a-f0-9]{64}', record.get('toolchain_recipe_sha256', ''))
                    or not wanted.get('packages')
                    or record.get('toolchain_packages') != wanted['packages']):
                raise ValueError(f'{key}: target toolchain differs from frozen release plan')
            if not re.fullmatch(r'[a-f0-9]{64}', record.get('toolchain_config_sha256', '')):
                raise ValueError(f'{key}: target toolchain configuration was not recorded')
        if not record.get('rustc'):
            raise ValueError(f'{key}: compiler identity is absent')
        if expected[key]['suite'] in ('native', 'compat'):
            if not re.fullmatch(r'[a-f0-9]{64}', record.get('runtime_binary_sha256', '')):
                raise ValueError(f'{key}: loaded runtime identity is absent')
            validate_identity(record.get('runtime_identity'), edition=expected[key]['edition'])
            confinement = next((value for prefix, value in (
                ('ubuntu-', 'linux-network-namespace'), ('macos-', 'macos-pf-offline'),
                ('windows-', 'windows-firewall')) if expected[key]['runner'].startswith(prefix)), None)
            if record.get('runtime_confinement_kind') != confinement:
                raise ValueError(f'{key}: runtime identity probe confinement is absent or different')
            binary_hash = record.get('runtime_probe_sha256')
            if not isinstance(binary_hash, str) or not re.fullmatch(r'[a-f0-9]{64}', binary_hash):
                raise ValueError(f'{key}: runtime probe executable identity is absent')
            if expected[key]['runner'].startswith('macos-'):
                from macos_native import validate_receipt
                validate_receipt(record.get('runtime_confinement'), operation='runtime-identity',
                                 binary_sha256=binary_hash)
        if expected[key]['suite'] == 'native':
            validate_regressions(expected[key], record.get('sdk_regressions'))
        if native_modes(expected[key]):
            suites = record.get('native_suites')
            if not isinstance(suites, dict) or set(suites) != set(native_modes(expected[key])):
                raise ValueError(f'{key}: required native SDK suite evidence is absent or unexpected')
            for mode, evidence in suites.items():
                validate_native_report(expected[key], evidence, mode)
        elif record.get('native_suites'):
            raise ValueError(f'{key}: unexpected native SDK suite evidence')
        if expected[key]['suite'] == 'mobile':
            mobile = record.get('mobile', {})
            if mobile.get('coverage') != expected[key]['coverage'] or not (mobile.get('package_sha256') or mobile.get('binary_sha256')):
                raise ValueError(f'{key}: mobile package or execution evidence is absent')
        commands = record.get('commands', [])
        labels = {command['label'] for command in commands if command.get('exit_code') == 0}
        required = required_steps(expected[key])
        if not required.issubset(labels) or any(c.get('exit_code') != 0 for c in commands):
            raise ValueError(f'{key}: required commands missing or failed')
        found[key] = record
    if set(found) != set(expected):
        raise ValueError(f'missing required results: {sorted(set(expected) - set(found))}')
    return {'schema': SCHEMA, 'status': 'passed', 'commit': plan['commit'], 'tree': plan['tree'],
            'suite_fingerprint': plan['suite_fingerprint'], 'lockfile_sha256': plan['lockfile_sha256'],
            'cells_passed': len(found)}


def validate_availability(plan, records, *, fingerprint=None):
    fingerprint = validate_plan(plan) if fingerprint is None else fingerprint
    expected = {entry['package'] for entry in plan['availability']}
    assignments = [(package, group['id']) for group in plan['groups'] for package in group['availability_packages']]
    owners = dict(assignments)
    if set(owners) != expected or len(owners) != len(assignments):
        raise ValueError('every selected SDK package requires one designated availability witness')
    found = set()
    for record in records:
        if not isinstance(record, dict):
            raise ValueError('malformed SDK availability evidence')
        key = record.get('package')
        if (key not in expected or key in found or record.get('status') != 'available'
                or type(record.get('schema')) is not int or record['schema'] != RECEIPT_SCHEMA):
            raise ValueError('missing, duplicate, unexpected or unavailable SDK acquisition result')
        validate_record_identity(plan, record, owners[key], fingerprint)
        package = plan['packages'][key]
        fields = {'artifact_id': package['artifact_id'], 'archive_sha256': package['sha256'],
                  'edition': package['edition'], 'platform': package['platform']}
        if any(record.get(name) != value for name, value in fields.items()):
            raise ValueError(f'{key}: acquisition identity differs from frozen plan')
        if record.get('version_evidence') != package.get('version_evidence'):
            raise ValueError(f'{key}: acquisition version evidence differs from plan')
        if package.get('version_evidence') == 'version-txt' and not record.get('version'):
            raise ValueError(f'{key}: parsed version identity is absent')
        if not re.fullmatch(r'[a-f0-9]{64}', record.get('inventory_sha256', '')):
            raise ValueError(f'{key}: acquisition has no verified installed inventory')
        if any(name in record for name in ('root', 'root_relative', 'inventory', 'url', 'archive', 'sources')):
            raise ValueError(f'{key}: acquisition receipt contains private source or SDK inventory data')
        found.add(key)
    if found != expected:
        raise ValueError('required SDK acquisition results are missing')
    return len(found)


def validate(plan, records, availability_records=None):
    fingerprint = validate_plan(plan)
    if availability_records is None:
        raise ValueError('all selected SDK availability receipts are required')
    owners = {cell: group['id'] for group in plan['groups'] for cell in group['cells']}
    records = list(records)
    for record in records:
        key = record.get('cell', {}).get('id') if isinstance(record, dict) else None
        if key not in owners:
            raise ValueError('unexpected result cell')
        validate_record_identity(plan, record, owners[key], fingerprint)
    report = validate_cells(plan, records)
    availability_records = list(availability_records)
    packages_verified = validate_availability(plan, availability_records, fingerprint=fingerprint)
    for receipt in availability_records:
        witnesses = [record[role] for record in records if record['group'] == receipt['group']
                     for role in ('headers', 'runtime') if record[role]['package'] == receipt['package']]
        if not witnesses or any(witness['inventory_sha256'] != receipt['inventory_sha256'] for witness in witnesses):
            raise ValueError('SDK availability inventory differs from its designated execution group')
    report.update(scope=plan['scope'], plan_sha256=fingerprint, groups_passed=len(plan['groups']),
                  packages_verified=packages_verified)
    return report


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--plan', type=Path, required=True)
    p.add_argument('--lockfile', type=Path, required=True, help='frozen lockfile transferred with the plan')
    p.add_argument('--results', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--availability', type=Path, required=True,
                   help='designated SDK acquisition receipts from hosted validation groups')
    args = p.parse_args()
    plan = read_json(args.plan)
    validate_checkout(plan, args.lockfile)
    report = validate(plan, [read_json(p) for p in args.results.rglob('result.json')],
                      [read_json(p) for p in args.availability.rglob('availability.json')])
    write_json(args.output, report)
    print(f"All {report['cells_passed']} {report['scope']} checks passed in {report['groups_passed']} groups "
          f"for {report['commit']}; {report['packages_verified']} SDK packages verified")

if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, KeyError) as error:
        print(f'Release gate failed: {error}', file=sys.stderr)
        raise SystemExit(1)
