#!/usr/bin/env python3
"""Verify the explicit active SDK archive retention policy without acquiring SDKs.

A catalog pin alone never satisfies retention. Only the exact reviewed artifact
does; other retained releases do not expand the policy. Architecture execution
and native qualification remain separate evidence.
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import re
import sys

sys.dont_write_bytecode = True
from common import ROOT, read_json, write_json
import provision
import sdk_store


def version_components(version):
    if not isinstance(version, str) or not re.fullmatch(r'\d+(?:\.\d+){1,3}', version):
        raise ValueError('invalid SDK artifact version')
    parts = tuple(int(c) for c in version.split('.'))
    return parts + (0,) * (4 - len(parts))


def load_requirements(path):
    value = read_json(Path(path)) if not isinstance(path, dict) else path
    if value.get('schema') != 2:
        raise ValueError('unsupported archive requirements schema')
    if value.get('editions') != ['standard', 'advanced'] or set(value.get('platforms', [])) != {'linux', 'windows', 'apple', 'android'}:
        raise ValueError('archive requirements must define both editions and all four bundle platforms')
    if len(value['platforms']) != 4:
        raise ValueError('duplicate archive requirement platform')
    families, prefixes = set(), set()
    for release in value.get('releases', []):
        family, prefix = release.get('family'), release.get('package_prefix')
        if not isinstance(family, str) or not re.fullmatch(r'6\.\d+(?:\.\d+)?', family):
            raise ValueError('invalid archive release family')
        if not isinstance(prefix, str) or not re.fullmatch(r'[a-z0-9]+(?:[.-][a-z0-9]+)*', prefix):
            raise ValueError('invalid archive package prefix')
        if version_components(family)[:3] != version_components(release.get('version'))[:3]:
            raise ValueError('archive release version contradicts its family')
        editions = release.get('editions')
        if not isinstance(editions, list) or not editions or any(e not in value['editions'] for e in editions) or len(set(editions)) != len(editions):
            raise ValueError('archive release must explicitly select valid editions')
        if family in families or prefix in prefixes:
            raise ValueError('duplicate archive release family or package prefix')
        if release.get('channel') not in ('public', 'private-preview'):
            raise ValueError('archive release channel is missing')
        families.add(family)
        prefixes.add(prefix)
    if not families:
        raise ValueError('archive requirements contain no release families')
    return value


def required_packages(requirements):
    return {f"{release['package_prefix']}-{edition}-{platform}": {
        'family': release['family'], 'version': release['version'], 'channel': release['channel'],
        'edition': edition, 'platform': platform,
    } for release in requirements['releases'] for edition in release['editions']
        for platform in requirements['platforms']}


def coverage(requirements, catalog, store=None):
    requirements = load_requirements(requirements)
    packages = provision.catalog_packages(catalog)
    rows = {key: dict(expected, package=key, state='missing', artifacts=[], expected_artifact_id=None)
            for key, expected in required_packages(requirements).items()}
    expected_artifacts = {}
    for key, row in rows.items():
        package = packages.get(key)
        if package is None:
            row['reason'] = 'reviewed-catalog-package-missing'
            continue
        if (package.get('edition'), package.get('platform')) != (row['edition'], row['platform']) or version_components(package.get('version')) != version_components(row['version']):
            raise ValueError(key + ': catalog identity contradicts the active retention policy')
        # The catalog is the identity authority, including when an alias has
        # changed. An older retained archive cannot satisfy a new reviewed pin.
        package = provision.resolved_package(key, packages)
        row['expected_artifact_id'] = package['artifact_id']
        row['reason'] = 'reviewed-archive-not-retained-and-verified'
        expected_artifacts[package['artifact_id']] = key

    outside_scope = []
    retained = 0
    if store is not None:
        store = Path(store)
        # Hash every original archive and verify installed payloads and portable
        # helpers before reading the index as evidence. Do not trust receipts alone.
        sdk_store.verify(store)
        index = sdk_store.load_index(store)
        for artifact_id, entry in sorted(index['archives'].items()):
            package = entry['catalog_entry']
            artifact = {'artifact_id': artifact_id, 'version': package['version'],
                        'edition': package['edition'], 'platform': package['platform'],
                        'archive_sha256': package['sha256'], 'bytes': entry['bytes'],
                        'installed': artifact_id in index['artifacts'],
                        'complete_original_archive': True,
                        'architecture_inventory': 'not-assessed-by-this-retention-audit'}
            retained += 1
            key = expected_artifacts.get(artifact_id)
            if key is None:
                outside_scope.append(artifact)
                continue
            rows[key]['artifacts'].append(artifact)
            rows[key]['state'] = 'verified'
            rows[key].pop('reason', None)

    verified = sum(row['state'] == 'verified' for row in rows.values())
    return {'schema': 2, 'complete': verified == len(rows),
            'desired_intersections': len(rows), 'verified_intersections': verified,
            'missing_intersections': len(rows) - verified, 'retained_archive_count': retained,
            'scope': 'explicit-active-complete-vendor-bundle-retention',
            'architecture_note': requirements['architecture_evidence'],
            'policy_note': requirements['purpose'],
            'requirements': list(rows.values()), 'outside_scope_artifacts': outside_scope,
            'outside_scope_catalog_packages': sorted(set(packages) - set(rows))}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--requirements', type=Path, default=ROOT / 'ci/archive-requirements.json')
    parser.add_argument('--catalog', type=Path, default=ROOT / 'ci/sdk-packages.json')
    parser.add_argument('--store', type=Path, help='private retained SDK store; omitting it cannot produce verified rows')
    parser.add_argument('--output', type=Path, help='write the bounded JSON report; no source URLs or private store paths')
    parser.add_argument('--json', action='store_true')
    parser.add_argument('--allow-missing', action='store_true', help='report incomplete coverage without a failing exit status')
    args = parser.parse_args(argv)
    try:
        result = coverage(args.requirements, args.catalog, args.store)
        if args.output:
            write_json(args.output, result)
        if args.json:
            print(json.dumps(result, indent=2, sort_keys=True))
        else:
            print(f"Verified complete vendor bundles: {result['verified_intersections']}/{result['desired_intersections']} required packages; {result['retained_archive_count']} exact archives retained.")
            for row in result['requirements']:
                if row['state'] == 'missing':
                    print(f"MISSING {row['package']}: {row['reason']}")
            print('Requirements are explicit; extra retained releases do not expand the policy. Architecture execution is not assessed.')
        return 0 if result['complete'] or args.allow_missing else 2
    except (ValueError, OSError, KeyError, TypeError) as error:
        print(f'Archive coverage failed: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
