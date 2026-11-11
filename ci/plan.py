#!/usr/bin/env python3
"""Emit deterministic hosted-only cells; missing profiles are errors, never skips."""
from __future__ import annotations
import argparse
import json
import os
from pathlib import Path
import sys
from provision import resolved_package, source_map, environment_sources
from public_toolchain import recipe_identity, expected_package_receipts
from common import ROOT, SCHEMA, digest, git_identity, package_key, profiles, read_json, secret_key, suite_fingerprint, write_json

NATIVE = {
    'x86_64-linux-gnu': 'ubuntu-24.04',
    'i686-linux-gnu': 'ubuntu-24.04',
    'aarch64-rpi4-linux-gnueabi': 'ubuntu-24.04-arm',
    'windows-x64': 'windows-2025', 'windows-x86': 'windows-2025',
    'macos-arm64': 'macos-15', 'macos-x86_64': 'macos-15-intel',
}


# Explicit validation scope approved for this release. Availability never chooses
# which baselines run; an unavailable retained package must still fail the gate.
COMPATIBILITY_BASELINES = ('6.3.2',)
VALIDATION_VERSIONS = (*COMPATIBILITY_BASELINES, 'current')
PROFILE_NIGHTLY = 'nightly-2026-11-10'
SCOPES = ('required', 'extended')
RUNTIME_REPRESENTATIVES = ('x86_64-linux-gnu', 'windows-x64', 'macos-arm64')
MOBILE_RUNTIME_REPRESENTATIVES = frozenset({
    ('android-x86_64', 'standard'), ('tvos-x86_64-sim', 'standard'),
})

# Reviewed support policy, not an availability filter. Retained vendor artifacts
# do not imply an obtainable, qualified compiler/sysroot or supported platform.
SUPPORTED_PROFILES = frozenset({
    'x86_64-linux-gnu', 'i686-linux-gnu', 'aarch64-rpi4-linux-gnueabi',
    'arm-rpi1-linux-gnueabihf', 'arm-rpi2-linux-gnueabihf',
    'arm-rpi3-linux-gnueabihf', 'arm-rpi4-linux-gnueabihf',
    'windows-x64', 'windows-x86', 'macos-arm64', 'macos-x86_64',
    'android-arm64', 'android-armv7', 'android-x86', 'android-x86_64',
    'ios-arm64', 'ios-x86_64-sim', 'tvos-arm64', 'tvos-x86_64-sim',
    'visionos-arm64',
})
UNSUPPORTED_PROFILES = frozenset({
    'aarch64-himix100-linux', 'aarch64-himix200-linux', 'aarch64-himix210-linux',
    'aarch64-mix210-linux', 'aarch64-mix410-linux', 'aarch64-newtek-linux-gnu',
    'aarch64-rockchip-linux-gnu', 'arm-himix100-linux', 'arm-himix200-linux',
    'arm-himix410-linux', 'arm-hisiv300-linux', 'arm-hisiv400-linux',
    'arm-hisiv500-linux', 'arm-hisiv510-linux', 'arm-hisiv600-linux',
    'arm-histbv310-linux', 'arm-newtek-linux-gnueabihf',
    'arm-rockchip-linux-gnueabihf', 'arm-sigmastar-linux-gnueabihf',
})

# Start with distinct host, linker and execution paths so an SDK/toolchain
# regression does not wait behind every cross-build of one platform family.
REPRESENTATIVE_CELLS = (
    ('x86_64-linux-gnu', 'standard', 'native'),
    ('windows-x64', 'standard', 'native'),
    ('macos-arm64', 'standard', 'native'),
    ('ios-arm64', 'standard', 'mobile'),
    ('android-arm64', 'standard', 'mobile'),
    ('aarch64-rpi4-linux-gnueabi', 'advanced', 'native'),
    ('windows-x86', 'advanced', 'native'),
    ('tvos-x86_64-sim', 'standard', 'mobile'),
)
REQUIRED_REPRESENTATIVE_CELLS = (
    ('x86_64-linux-gnu', 'standard', 'native'),
    ('windows-x64', 'standard', 'native'),
    ('macos-arm64', 'standard', 'native'),
    ('ios-arm64', 'standard', 'mobile'),
    ('android-x86_64', 'standard', 'mobile'),
    ('aarch64-rpi4-linux-gnueabi', 'advanced', 'link'),
    ('windows-x86', 'advanced', 'link'),
    ('tvos-x86_64-sim', 'standard', 'mobile'),
)


def representative_first(cells, scope='required'):
    if scope not in SCOPES:
        raise ValueError('unknown CI scope')
    representatives = REQUIRED_REPRESENTATIVE_CELLS if scope == 'required' else REPRESENTATIVE_CELLS
    priority = {(*identity, 'current', 'current'): index
                for index, identity in enumerate(representatives)}
    def identity(cell):
        return tuple(cell[field] for field in ('profile', 'edition', 'suite', 'headers', 'runtime'))
    identities = [identity(cell) for cell in cells]
    if any(identities.count(key) != 1 for key in priority):
        raise ValueError('representative matrix cells are missing or duplicated')
    # Python's stable sort preserves every other cell's original relative order.
    return sorted(cells, key=lambda cell: priority.get(identity(cell), len(priority)))


def make_plan(rows, scope='required'):
    if scope not in SCOPES:
        raise ValueError('unknown CI scope')
    missing = SUPPORTED_PROFILES - rows.keys()
    if missing:
        raise ValueError(f'missing mandatory supported profiles: {sorted(missing)}')
    unclassified = rows.keys() - (SUPPORTED_PROFILES | UNSUPPORTED_PROFILES)
    if unclassified:
        raise ValueError(f'new artifact profiles require explicit support policy: {sorted(unclassified)}')
    cells = []
    def add(profile, edition, suite, headers='current', runtime='current', toolchain='stable'):
        p = rows[profile]
        if edition not in p['editions'].split(','):
            raise ValueError(f'{profile}: edition {edition} not supplied')
        runner = NATIVE.get(profile, 'macos-15-intel' if p['os'] in ('ios', 'tvos', 'visionos') else 'ubuntu-24.04')
        if p['os'] == 'visionos':
            runner = 'macos-15'
        if scope == 'required' and p['os'] in ('ios', 'tvos', 'visionos') and p['arch'] == 'aarch64':
            runner = 'macos-15'
        if p['os'] in ('tvos', 'visionos') and toolchain == 'stable':
            toolchain = '1.98.0'
        build_std = p['target'] == 'x86_64-apple-tvos'
        if build_std:
            # Public compiler/rust-src availability and std compilation were
            # qualified independently; Apple SDK linking remains a required job.
            toolchain = PROFILE_NIGHTLY
        sid = '-'.join([profile, edition, suite, headers, runtime, toolchain])
        header_key = package_key(headers, edition, p['os'])
        runtime_key = package_key(runtime, edition, p['os'])
        coverage = ('native' if suite in ('native', 'compat') else
            ('emulator' if p['os'] == 'android' and p['arch'] in ('x86', 'x86_64') else
             'simulator' if 'sim' in profile else 'package') if suite == 'mobile' else
            'abi-link' if suite == 'link' else 'compile')
        if scope == 'required' and suite == 'mobile' and (profile, edition) not in MOBILE_RUNTIME_REPRESENTATIVES:
            coverage = 'package'
        cells.append({'id': sid, 'profile': profile, 'target': p['target'],
            'edition': edition, 'suite': suite, 'headers': headers, 'runtime': runtime,
            'runner': runner, 'toolchain': toolchain, 'build_std': build_std,
            'requires_toolchain': p['os'] == 'linux' and profile not in NATIVE,
            'toolchain_provider': 'public-gnu' if p['os'] == 'linux' and profile not in NATIVE else 'hosted',
            'coverage': coverage,
            'emulator_api': 30 if profile == 'android-x86' else 35,
            'emulator_arch': 'x86' if profile == 'android-x86' else 'x86_64',
            'header_package': header_key, 'runtime_package': runtime_key,
            'header_url_secret': 'NDI_SDK_' + secret_key(header_key) + '_URL',
            'runtime_url_secret': 'NDI_SDK_' + secret_key(runtime_key) + '_URL',
            'header_hash_variable': 'NDI_SDK_' + secret_key(header_key) + '_SHA256',
            'runtime_hash_variable': 'NDI_SDK_' + secret_key(runtime_key) + '_SHA256'})
    for name in sorted(SUPPORTED_PROFILES):
        p = rows[name]
        for edition in p['editions'].split(','):
            native = name in NATIVE and (scope == 'extended' or name in RUNTIME_REPRESENTATIVES)
            add(name, edition, 'native' if native else 'mobile' if p['os'] in ('android', 'ios', 'tvos', 'visionos') else 'link')
    # Both directions preserve an old-header build, not merely an old-runtime load.
    for name in sorted(NATIVE if scope == 'extended' else RUNTIME_REPRESENTATIVES):
        for baseline in COMPATIBILITY_BASELINES:
            if scope == 'extended':
                add(name, 'standard', 'compat', baseline, baseline)
            add(name, 'standard', 'compat', 'current', baseline)
            add(name, 'standard', 'compat', baseline, 'current')
    for version in VALIDATION_VERSIONS:
        add('x86_64-linux-gnu', 'standard', 'msrv', version, version, '1.88.0')
    if scope == 'extended':
        for name in sorted(NATIVE):
            if name != 'x86_64-linux-gnu':
                add(name, 'standard', 'msrv', toolchain='1.88.0')
        for name in sorted(SUPPORTED_PROFILES):
            if rows[name]['os'] == 'android':
                for version in VALIDATION_VERSIONS:
                    add(name, 'standard', 'msrv', version, version, '1.88.0')
    else:
        # Preserve the Android MSRV and retained baseline package witness without
        # multiplying the same language-version check across every ABI.
        add('android-arm64', 'standard', 'msrv', '6.3.2', '6.3.2', '1.88.0')
    for baseline in VALIDATION_VERSIONS:
        add('x86_64-linux-gnu', 'standard', 'features', baseline, baseline)
    for name in RUNTIME_REPRESENTATIVES if scope == 'extended' else ('x86_64-linux-gnu',):
        add(name, 'standard', 'semver')
    if len({c['id'] for c in cells}) != len(cells):
        raise ValueError('duplicate CI cell ID')
    if len(cells) > 256:
        raise ValueError('matrix exceeds hosted Actions limit; split explicitly')
    return representative_first(cells, scope)


def make_groups(cells, scope='required'):
    """Group explicit checks by compatible hosted setup; never omit a check."""
    if scope not in SCOPES:
        raise ValueError('unknown CI scope')
    if not cells or len({cell['id'] for cell in cells}) != len(cells):
        raise ValueError('group checks must be nonempty and have unique identities')
    families = dict(zip(RUNTIME_REPRESENTATIVES, ('linux', 'windows', 'macos')))
    group_ids = ([f'native-{family}-{edition}' for edition in ('standard', 'advanced')
                  for family in ('linux', 'windows', 'macos')]
        + ['build-linux-32', 'build-linux-arm64', 'build-windows-32', 'build-apple-intel',
           'build-apple-devices', 'build-android', 'compat-linux', 'compat-windows',
           'compat-macos', 'quality-linux']) if scope == 'required' else ['extended-' + cell['id'] for cell in cells]
    grouped = {identity: [] for identity in group_ids}
    for cell in cells:
        profile, suite = cell['profile'], cell['suite']
        if scope == 'extended':
            identity = 'extended-' + cell['id']
        elif suite == 'native' and profile in families:
            identity = f'native-{families[profile]}-{cell["edition"]}'
        elif suite == 'compat' and profile in families:
            identity = 'compat-' + families[profile]
        elif suite in ('msrv', 'features', 'semver') and profile == 'x86_64-linux-gnu':
            identity = 'quality-linux'
        elif profile.startswith('android-'):
            identity = 'build-android'
        elif profile in ('i686-linux-gnu', *(f'arm-rpi{i}-linux-gnueabihf' for i in range(1, 5))):
            identity = 'build-linux-32'
        elif profile == 'aarch64-rpi4-linux-gnueabi':
            identity = 'build-linux-arm64'
        elif profile == 'windows-x86':
            identity = 'build-windows-32'
        elif profile in ('macos-x86_64', 'ios-x86_64-sim', 'tvos-x86_64-sim'):
            identity = 'build-apple-intel'
        elif profile in ('ios-arm64', 'tvos-arm64', 'visionos-arm64'):
            identity = 'build-apple-devices'
        else:
            raise ValueError('check has no reviewed CI group')
        if identity not in grouped:
            raise ValueError('check has no reviewed CI group')
        grouped[identity].append(cell)
    groups, assigned_packages = [], set()
    for identity, members in grouped.items():
        if not members or len({cell['runner'] for cell in members}) != 1:
            raise ValueError('CI group must contain checks on exactly one hosted runner')
        packages = {cell[role] for cell in members for role in ('header_package', 'runtime_package')}
        group = {'id': identity, 'runner': members[0]['runner'],
            'cells': [cell['id'] for cell in members],
            'needs_multilib': any(cell['target'] == 'i686-unknown-linux-gnu' for cell in members),
            'android': any(cell['profile'].startswith('android-') for cell in members),
            'availability_packages': sorted(packages - assigned_packages)}
        emulators = {(cell['emulator_api'], cell['emulator_arch']) for cell in members if cell['coverage'] == 'emulator'}
        if len(emulators) > 1:
            raise ValueError('CI group requires incompatible emulator images')
        if emulators:
            group['emulator_api'], group['emulator_arch'] = next(iter(emulators))
        assigned_packages.update(packages)
        groups.append(group)
    return groups


def provisioning_status(cells, packages, environment=None, require_sources=False):
    environment = os.environ if environment is None else environment
    resolved = {key: dict(value) for key, value in packages.items()}
    issues, toolchains = [], {}
    try:
        private_sources = source_map(environment_sources(environment))
    except ValueError as error:
        issues.append(str(error))
        private_sources = {}
    keys = sorted({cell[role] for cell in cells for role in ('header_package', 'runtime_package')})
    for key in keys:
        if key not in resolved:
            issues.append(f'{key}: package metadata is absent')
            continue
        variable = 'NDI_SDK_' + secret_key(key) + '_SHA256'
        try:
            resolved[key] = resolved_package(key, resolved, environment.get(variable))
        except ValueError as error:
            issues.append(str(error))
        if require_sources:
            secret = 'NDI_SDK_' + secret_key(key) + '_URL'
            value = environment.get(secret, '')
            entry = private_sources.get(key) or private_sources.get(resolved[key].get('artifact_id'))
            if entry and value:
                issues.append(f'{key}: private source map conflicts with configured URL ({secret})')
            elif entry and 'github_release_asset' in entry:
                token_env = entry['github_release_asset'].get('token_env', 'NDI_SDK_ARCHIVE_TOKEN')
                if not environment.get(token_env):
                    issues.append(f'{key}: private GitHub asset token is not configured ({token_env})')
            elif entry and 'url_env' in entry:
                source_env = entry['url_env']
                source_url = environment.get(source_env, '')
                if not source_url.startswith('https://'):
                    issues.append(f'{key}: private URL source is missing or not HTTPS ({source_env})')
            elif entry and 'archive' in entry:
                if not Path(entry['archive']).is_file():
                    issues.append(f'{key}: configured local archive is unavailable')
            elif not value:
                issues.append(f'{key}: source is not configured ({secret})')
            elif not value.startswith('https://'):
                issues.append(f'{key}: configured source must use HTTPS ({secret})')
    for cell in cells:
        if cell['requires_toolchain'] and cell['profile'] not in toolchains:
            try:
                if cell['toolchain_provider'] != 'public-gnu':
                    raise ValueError('unsupported required toolchain provider')
                toolchains[cell['profile']] = {'provider': 'public-gnu',
                    'recipe_sha256': recipe_identity(cell['profile']),
                    'packages': expected_package_receipts(cell['profile'])}
            except (OSError, ValueError) as error:
                issues.append(f'{cell["profile"]}: public toolchain recipe is invalid ({error})')
    return resolved, toolchains, sorted(issues)


def make_availability(cells, packages):
    """Catalog inventory; designated group checks provide the acquisition proof."""
    return [{'package': key,
        'runner': {'windows': 'windows-2025', 'apple': 'macos-15'}.get(packages[key]['platform'], 'ubuntu-24.04'),
        'url_secret': 'NDI_SDK_' + secret_key(key) + '_URL',
        'hash_variable': 'NDI_SDK_' + secret_key(key) + '_SHA256'}
        for key in sorted({cell[role] for cell in cells for role in ('header_package', 'runtime_package')})
        if key in packages]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--github-output', type=Path)
    parser.add_argument('--scope', choices=SCOPES, default='required')
    parser.add_argument('--require-provisioning', action='store_true')
    parser.add_argument('--require-sources', action='store_true', help='check every required SDK source and public toolchain recipe without network requests')
    parser.add_argument('--preflight-output', type=Path, help='bounded configuration report; never contains source URLs')
    args = parser.parse_args()
    cells = make_plan(profiles(), args.scope)
    groups = make_groups(cells, args.scope)
    packages, toolchains, issues = provisioning_status(cells, read_json(ROOT / 'ci/sdk-packages.json'), require_sources=args.require_sources)
    availability = make_availability(cells, packages)
    plan = {'schema': SCHEMA, 'scope': args.scope, 'groups': groups, 'packages': packages, 'toolchains': toolchains,
            **git_identity(), 'suite_fingerprint': suite_fingerprint(), 'cells': cells, 'availability': availability,
            'unsupported_profiles': sorted(UNSUPPORTED_PROFILES),
            'lockfile_sha256': digest(ROOT / 'Cargo.lock') if (ROOT / 'Cargo.lock').exists() else None}
    if args.preflight_output:
        write_json(args.preflight_output, {'schema': 1, 'status': 'missing-configuration' if issues else 'configured',
            'packages': [item['package'] for item in availability], 'issues': issues,
            'download_verified': False})
    write_json(args.output, plan)
    if (args.require_provisioning or args.require_sources) and issues:
        raise ValueError('provisioning prerequisites are incomplete:\n- ' + '\n- '.join(issues))
    if args.github_output:
        with args.github_output.open('a', encoding='utf-8') as stream:
            stream.write('matrix=' + json.dumps({'include': groups}, separators=(',', ':')) + '\n')
    print(f'Planned {len(cells)} {args.scope} checks in {len(groups)} jobs across {len(SUPPORTED_PROFILES)} supported profiles; '
          f'{len(UNSUPPORTED_PROFILES)} retained artifact profiles are unsupported')

if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError) as error:
        print(f'CI plan failed: {error}', file=sys.stderr)
        raise SystemExit(1)
