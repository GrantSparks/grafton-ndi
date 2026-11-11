#!/usr/bin/env python3
"""Prepare verified public outputs; publishing requires the trusted hosted release gate."""
from __future__ import annotations
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import sys
import tarfile
import tomllib
from common import ROOT, SCHEMA, digest, git_identity, read_json, suite_fingerprint, write_json
from provision import child_environment, environment_sources, provision, resolved_package, verify_installation
from aggregate import validate_plan

PACKAGE = 'current-standard-linux'
PUBLIC_DOC_SUFFIXES = {'.html', '.js', '.css', '.svg', '.png', '.ico', '.woff', '.woff2', '.ttf', '.txt', '.json', '.bin', '.md'}
FORBIDDEN_PARTS = {'target', 'out', 'sdks', 'sdk-downloads', '.git', '__pycache__'}
FORBIDDEN_NAMES = {'provision.json', 'ndi_advanced.rs', 'ndi_static_symbols.rs', 'ndi_file_static_symbols.rs', 'ndi_file.rs'}
SDK_NAME = re.compile(r'(?i)(?:^processing\.ndi\.|\.(?:so(?:\.\d+)*|dll|dylib|a|lib|exe|o|obj|zip|tar|gz|xz|pkg|crate|log)$)')
BINDGEN_MARKER = b'automatically generated' + b' by rust-bindgen'
PRIVATE_PATH = re.compile(rb"/(?:workspace|home|Users)/[^\s\"'<>]*(?:private|scratch|sdk-downloads)[^\s\"'<>]*")
NATIVE_MAGICS = (b'\x7fELF', b'MZ', b'!<arch>\n', b'\xcf\xfa\xed\xfe', b'\xfe\xed\xfa\xcf', b'\xca\xfe\xba\xbe')


def manifest(root=ROOT):
    return tomllib.loads((root / 'Cargo.toml').read_text(encoding='utf-8'))['package']


def release_guard(environment, version, *, kind):
    if environment.get('GITHUB_ACTIONS') != 'true' or environment.get('NDI_CI_RELEASED') != 'true':
        raise ValueError('publication requires explicitly released hosted CI')
    if environment.get('NDI_REQUIRED_CI_PASSED') != 'true':
        raise ValueError('the complete required CI gate did not pass')
    if environment.get('GITHUB_EVENT_NAME') not in ('push', 'workflow_dispatch'):
        raise ValueError('publication requires a trusted push or manual release run')
    if environment.get('GITHUB_REPOSITORY') != 'GrantSparks/grafton-ndi':
        raise ValueError('publication is restricted to the upstream repository')
    ref = environment.get('GITHUB_REF', '')
    if kind == 'docs':
        if ref != 'refs/heads/main':
            raise ValueError('documentation publication requires main')
    elif kind == 'crate':
        if ref not in ('refs/tags/' + version, 'refs/tags/v' + version):
            raise ValueError('release tag must equal Cargo.toml version, optionally prefixed with v')
    else:
        raise ValueError('unknown publication kind')


def validate_evidence(plan, report, lockfile, identity, fingerprint, package):
    plan_hash = validate_plan(plan)
    if plan['scope'] != 'required' or report.get('scope') != 'required':
        raise ValueError('publication requires the approved required scope, not an extended manual run')
    if (report.get('plan_sha256') != plan_hash
            or type(report.get('groups_passed')) is not int or report['groups_passed'] != len(plan['groups'])
            or type(report.get('packages_verified')) is not int or report['packages_verified'] != len(plan['availability'])):
        raise ValueError('publication lacks exact grouped plan or SDK availability evidence')
    if plan.get('schema') != SCHEMA or report.get('schema') != SCHEMA or report.get('status') != 'passed':
        raise ValueError('publication evidence schema or aggregate status is invalid')
    if identity.get('dirty') or plan.get('dirty'):
        raise ValueError('publication requires a clean exact checkout')
    for field in ('commit', 'tree'):
        if plan.get(field) != identity[field] or report.get(field) != identity[field]:
            raise ValueError('publication evidence belongs to a different source identity')
    if not re.fullmatch(r'[a-f0-9]{64}', fingerprint) or any(record.get('suite_fingerprint') != fingerprint for record in (plan, report)):
        raise ValueError('publication evidence belongs to a different validation suite')
    lock_hash = digest(lockfile)
    if any(record.get('lockfile_sha256') != lock_hash for record in (plan, report)):
        raise ValueError('publication dependency lockfile differs from tested resolution')
    cells = plan.get('cells', [])
    if not cells or type(report.get('cells_passed')) is not int or report['cells_passed'] != len(cells):
        raise ValueError('publication requires every planned SDK cell')
    if plan.get('packages', {}).get(PACKAGE) != package:
        raise ValueError('publication SDK identity differs from the validated plan')
    return lock_hash


def validate_package_member(name, data):
    path = PurePosixPath(name)
    if path.is_absolute() or '..' in path.parts or any(p.lower() in FORBIDDEN_PARTS for p in path.parts):
        raise ValueError('package contains a private or unsafe path')
    if path.name.lower() in FORBIDDEN_NAMES or SDK_NAME.search(path.name):
        raise ValueError('package contains an SDK, generated binding, archive, binary or raw log')
    if data.startswith(NATIVE_MAGICS):
        raise ValueError('package contains a native binary')
    # The reviewed src/ndi_lib.rs adapter is allowed; generated output is not.
    if BINDGEN_MARKER in data or PRIVATE_PATH.search(data):
        raise ValueError('package contains generated SDK bindings or private workspace data')


def inspect_package(archive):
    count = 0
    with tarfile.open(archive, 'r:gz') as stream:
        for member in stream:
            if not member.isfile():
                raise ValueError('crate archive must contain only regular files')
            if member.size > 32 * 1024 * 1024:
                raise ValueError('unexpected oversized crate member')
            data = stream.extractfile(member).read()
            validate_package_member(member.name, data)
            count += 1
    if count == 0:
        raise ValueError('empty crate archive')
    return {'files': count, 'sha256': digest(archive)}


def stage_docs(source, destination):
    source, destination = Path(source), Path(destination)
    if destination.exists():
        raise ValueError('public documentation destination already exists')
    files = []
    for path in source.rglob('*'):
        relative = path.relative_to(source)
        if path.is_symlink():
            raise ValueError('public documentation contains a symbolic link')
        if path.is_dir() or relative.as_posix() == '.lock':
            continue
        if relative.parts[0] == 'src' or any(p in {'ndi_lib', 'native_abi', 'ffi', 'generated'} for p in relative.parts):
            raise ValueError('public documentation contains source or private ABI pages')
        if not path.is_file() or path.suffix not in PUBLIC_DOC_SUFFIXES or path.name == 'src-files.js':
            raise ValueError('unexpected public documentation artifact')
        data = path.read_bytes()
        if data.startswith(NATIVE_MAGICS) or BINDGEN_MARKER in data or PRIVATE_PATH.search(data):
            raise ValueError('public documentation contains native/private generated data')
        if path.suffix == '.html':
            for match in re.finditer(rb'(?:href|src)=["\']([^"\']+)["\']', data):
                target = match.group(1)
                if not target.startswith((b'https://', b'http://')) and re.search(rb'(?:^|/)src/', target):
                    raise ValueError('public documentation still links to source HTML')
        files.append(relative)
    if not (source / 'grafton_ndi/index.html').is_file():
        raise ValueError('crate documentation index is absent')
    destination.mkdir(parents=True)
    for relative in files:
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source / relative, target)
    (destination / 'index.html').write_text('<meta http-equiv="refresh" content="0; url=grafton_ndi/index.html">\n', encoding='utf-8')
    return len(files) + 1


class Publication:
    def __init__(self, work, plan, report, lockfile, kind):
        self.work = Path(work).resolve()
        self.work.mkdir(parents=True, exist_ok=True)
        self.plan, self.report, self.lockfile, self.kind = Path(plan), Path(report), Path(lockfile), kind
        self.metadata = manifest()
        release_guard(os.environ, self.metadata['version'], kind=kind)
        identity = git_identity()
        if identity['commit'] != os.environ.get('GITHUB_SHA'):
            raise ValueError('publication checkout differs from the triggering commit')
        self.lock_hash = validate_evidence(read_json(plan), read_json(report), self.lockfile, identity,
                                          suite_fingerprint(), resolved_package(PACKAGE))
        shutil.copyfile(self.lockfile, ROOT / 'Cargo.lock')
        self.env = child_environment()
        self.env.update({'CARGO_TARGET_DIR': str(self.work / 'target'), 'CARGO_TERM_COLOR': 'never',
                         'NDI_SDK_PROFILE': 'x86_64-linux-gnu', 'NDI_SDK_EDITION': 'standard'})

    def command(self, label, command):
        log = self.work / (label + '.log')
        with log.open('w', encoding='utf-8') as output:
            process = subprocess.run(command, cwd=ROOT, env=self.env, stdout=output, stderr=subprocess.STDOUT, text=True)
        if process.returncode:
            raise ValueError(f'{label} failed; diagnostics remain on the ephemeral runner')
        print(f'{label}: passed (log SHA256 {digest(log)})')
        return log

    def sdk(self, *, reuse=False):
        if reuse:
            receipt = verify_installation(PACKAGE, self.work / 'sdk')
        else:
            # Provision uses the shared exact catalog and credential-stripping
            # boundary. Keep every source URL, receipt and SDK payload local.
            log = self.work / 'provision.log'
            with log.open('w', encoding='utf-8') as output:
                # Vendor extraction subprocess output must also remain private.
                import contextlib
                with contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
                    receipt = provision(PACKAGE, self.work / 'sdk', os.environ.get('NDI_SDK_SOURCE_URL', ''),
                                        sources=environment_sources())
        self.env['NDI_SDK_DIR'] = receipt['root']

    def docs(self, destination):
        self.sdk()
        self.env['RUSTDOCFLAGS'] = '-D warnings --cfg ndi_public_docs'
        self.command('public-docs', ['cargo', 'doc', '--locked', '--all-features', '--no-deps'])
        count = stage_docs(self.work / 'target/doc', destination)
        print(f'Public documentation staged: {count} files; source pages disabled and checked')

    def package_archive(self):
        return self.work / 'target/package' / f"{self.metadata['name']}-{self.metadata['version']}.crate"

    def package(self):
        self.sdk()
        self.command('package-list', ['cargo', 'package', '--locked', '--list'])
        # Verify the build as well as auditing the exact archive contents.
        self.command('package-verify', ['cargo', 'package', '--locked'])
        evidence = inspect_package(self.package_archive())
        write_json(self.work / 'package-verified.json', dict(evidence, lockfile_sha256=self.lock_hash))
        print(f"Crate archive verified: {evidence['files']} files (SHA256 {evidence['sha256']})")

    def publish(self):
        self.sdk(reuse=True)
        expected = read_json(self.work / 'package-verified.json')
        actual = inspect_package(self.package_archive())
        if any(expected.get(key) != value for key, value in actual.items()) or expected.get('lockfile_sha256') != self.lock_hash:
            raise ValueError('prepared crate changed after package verification')
        if not self.env.get('CARGO_REGISTRY_TOKEN'):
            raise ValueError('OIDC crates.io authentication is required')
        self.command('crate-publish', ['cargo', 'publish', '--locked'])
        print('Crate publication completed')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=('docs', 'package', 'publish'))
    parser.add_argument('--work', type=Path, required=True)
    parser.add_argument('--plan', type=Path, required=True)
    parser.add_argument('--report', type=Path, required=True)
    parser.add_argument('--lockfile', type=Path, required=True)
    parser.add_argument('--stage', type=Path)
    args = parser.parse_args()
    try:
        publication = Publication(args.work, args.plan, args.report, args.lockfile, 'docs' if args.operation == 'docs' else 'crate')
        if args.operation == 'docs':
            if args.stage is None:
                raise ValueError('documentation staging path is required')
            publication.docs(args.stage)
        elif args.operation == 'package':
            publication.package()
        else:
            publication.publish()
    except Exception as error:
        # Do not echo arbitrary subprocess, URL or SDK parsing diagnostics.
        detail = str(error) if isinstance(error, ValueError) else type(error).__name__
        print('Publication preparation failed: ' + detail[:240], file=sys.stderr)
        return 1
    return 0

if __name__ == '__main__':
    raise SystemExit(main())
