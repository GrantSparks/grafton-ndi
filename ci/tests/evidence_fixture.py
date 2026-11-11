"""SDK-free canonical policy fixtures shared by aggregation/publication tests."""
from common import ROOT, SCHEMA, native_configuration, native_modes, plan_digest, profiles, read_json, required_steps
from native import suite_tests
from plan import make_availability, make_groups, make_plan, provisioning_status, UNSUPPORTED_PROFILES
from provision import RECEIPT_SCHEMA


def confinement_receipt(operation, binary_sha256='a' * 64):
    from macos_native import RECEIPT_CHECKS
    return {'schema': 1, 'status': 'passed', 'operation': operation,
            'binary_sha256': binary_sha256, 'worker_limit_seconds': 30,
            'offline_limit_seconds': 45, 'offline_elapsed_seconds': 4.0,
            'block_packets_delta': 1, 'external_ipv6': 'not-probed',
            'checks': {name: True for name in RECEIPT_CHECKS}, 'details_sha256': 'b' * 64}


def native_report(cell, label='regressions'):
    mode, selection = native_configuration(label)
    platform = ('linux' if cell['runner'].startswith('ubuntu-') else
                'darwin' if cell['runner'].startswith('macos-') else 'win32')
    suites = suite_tests(mode, platform)
    tests = [{'suite': suite, 'test': test, 'status': 'passed', 'passed': 1, 'ignored': 0,
              'binary_sha256': 'a' * 64, 'log_sha256': 'a' * 64,
              **({'confinement_evidence': confinement_receipt(test)} if platform == 'darwin' else {})}
             for suite, names in suites.items() for test in names]
    report = {'mode': mode, 'feature_set': selection,
        'confinement': {'linux': 'linux-network-namespace', 'darwin': 'macos-pf-offline',
                        'win32': 'windows-firewall'}[platform],
        'test_count': len(tests), 'suite_count': len(suites), 'tests': tests}
    if len(suites) == 1:
        report['binary_sha256'] = 'a' * 64
    if platform == 'darwin' and mode in ('core', 'regressions'):
        report['listings'] = [{'suite': suite, 'binary_sha256': 'a' * 64,
            'confinement_evidence': confinement_receipt('list')} for suite in suites]
    return report


def catalog_with_unselected_package():
    catalog = read_json(ROOT / 'ci/sdk-packages.json')
    package = dict(catalog['6.3.2-standard-linux'], version='6.3.1')
    package['artifact_id'] = 'ndi-6.3.1-standard-linux-' + package['sha256']
    catalog['6.3.1-standard-linux'] = package
    return catalog


def evidence_fixture(scope='required', *, catalog=None):
    cells = make_plan(profiles(), scope=scope)
    packages, toolchains, issues = provisioning_status(
        cells, read_json(ROOT / 'ci/sdk-packages.json') if catalog is None else catalog, environment={})
    if issues:
        raise AssertionError(issues)
    groups = make_groups(cells, scope=scope)
    plan = {'schema': SCHEMA, 'scope': scope, 'cells': cells, 'groups': groups,
            'packages': packages, 'toolchains': toolchains,
            'unsupported_profiles': sorted(UNSUPPORTED_PROFILES),
            'commit': 'b' * 40, 'tree': 'c' * 40, 'suite_fingerprint': 'd' * 64,
            'lockfile_sha256': 'e' * 64, 'dirty': False,
            'availability': make_availability(cells, packages)}
    binding = {field: plan[field] for field in (
        'commit', 'tree', 'suite_fingerprint', 'lockfile_sha256', 'dirty', 'scope')}
    binding['plan_sha256'] = plan_digest(plan)
    owners = {key: group['id'] for group in groups for key in group['cells']}

    def receipt(key):
        package = packages[key]
        return {'schema': RECEIPT_SCHEMA, 'package': key, 'artifact_id': package['artifact_id'],
                'archive_sha256': package['sha256'], 'edition': package['edition'],
                'platform': package['platform'], 'version': package['version'],
                'version_evidence': package['version_evidence'], 'inventory_sha256': 'f' * 64}

    records = []
    for cell in cells:
        record = {'schema': SCHEMA, **binding, 'group': owners[cell['id']], 'cell': cell,
                  'status': 'passed', 'coverage': cell['coverage'], 'rustc': 'rustc fixture',
                  'headers': receipt(cell['header_package']), 'runtime': receipt(cell['runtime_package']),
                  'commands': [{'label': label, 'exit_code': 0, 'log_sha256': 'a' * 64}
                               for label in sorted(required_steps(cell))]}
        if cell['suite'] in ('native', 'compat'):
            record.update(runtime_binary_sha256='a' * 64, runtime_identity={
                'schema': 1, 'version': [6, 4, 0, 2], 'edition': cell['edition'], 'edition_verified': True})
            record.update(runtime_probe_sha256='a' * 64, runtime_confinement_kind=(
                'linux-network-namespace' if cell['runner'].startswith('ubuntu-') else
                'macos-pf-offline' if cell['runner'].startswith('macos-') else 'windows-firewall'))
            if cell['runner'].startswith('macos-'):
                record['runtime_confinement'] = confinement_receipt('runtime-identity')
        if cell['suite'] == 'native':
            record['sdk_regressions'] = native_report(cell)
        if native_modes(cell):
            record['native_suites'] = {mode: native_report(cell, mode) for mode in native_modes(cell)}
        if cell['suite'] == 'mobile':
            record['mobile'] = {'coverage': cell['coverage'], 'binary_sha256': 'a' * 64}
        if cell['requires_toolchain']:
            toolchain = toolchains[cell['profile']]
            record.update(toolchain_provider=toolchain['provider'],
                          toolchain_recipe_sha256=toolchain['recipe_sha256'],
                          toolchain_packages=toolchain['packages'], toolchain_config_sha256='a' * 64)
        records.append(record)
    available = [{**receipt(key), **binding, 'group': group['id'], 'status': 'available'}
                 for group in groups for key in group['availability_packages']]
    return plan, records, available
