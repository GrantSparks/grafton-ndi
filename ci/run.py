#!/usr/bin/env python3
"""Run one declared cell locally or on a hosted runner; emit honest evidence."""
from __future__ import annotations
import argparse
import json
import os
from pathlib import Path
import platform
import re
import shlex
import subprocess
import sys
import time
import traceback
from common import ROOT, SCHEMA, FEATURE_SETS, digest, feature_flags, git_identity, profiles, read_json, required_steps, native_configuration, native_modes, suite_fingerprint, write_json
from plan import make_plan, NATIVE
from provision import provision, sanitized_receipt, environment_sources, child_environment
from public_toolchain import prepare as prepare_public_toolchain
import cargo_artifacts
from diagnostics import summarize_log, summarize_exception
from runtime_identity import parse_identity


class CellRun:
    def __init__(self, cell, output, *, sdk_provider=None, target_directory=None, rust_setup=None):
        self.cell, self.output = cell, output.resolve()
        self.output.mkdir(parents=True, exist_ok=True)
        self.sdk_provider = sdk_provider
        self.target_directory = (target_directory or self.output / 'target').resolve()
        self.rust_setup = rust_setup
        self.env = os.environ.copy()
        self.commands = []
        self.clang_args = []
        self.abi_cc = 'clang'
        self.link_cc = None
        self.link_args = []
        self.result = {'schema': SCHEMA, 'cell': cell, **git_identity(),
                       'suite_fingerprint': suite_fingerprint(), 'status': 'failed',
                       'coverage': cell['coverage'], 'commands': self.commands,
                       'host': {'system': platform.system(), 'machine': platform.machine()}}

    def command(self, label, args, *, capture=False, cwd=ROOT, executor=None, env_overrides=None):
        log = self.output / (label + '.log')
        start = time.monotonic()
        with log.open('w', encoding='utf-8') as stream:
            argv = [str(x) for x in args]
            env = child_environment({**self.env, **(env_overrides or {})})
            if executor is None:
                process = subprocess.run(argv, cwd=cwd, env=env,
                                         stdout=stream, stderr=subprocess.STDOUT, text=True)
            else:
                process = executor(argv, env)
                stream.write(process.stdout or '')
                stream.write(process.stderr or '')
        self.commands.append({'label': label, 'exit_code': process.returncode,
                              'seconds': round(time.monotonic() - start, 3),
                              'log_sha256': digest(log)})
        if process.returncode:
            # Export only bounded error codes and independently checked public
            # source/test identities. Never copy arbitrary tool output or snippets.
            try:
                diagnostic = summarize_log(log, ROOT)
            except (OSError, ValueError, subprocess.SubprocessError):
                diagnostic = {'schema': 1, 'reasons': ['diagnostic-summary-unavailable']}
            self.commands[-1]['diagnostics'] = diagnostic
            print(f'{label}: exit {process.returncode}; bounded diagnostics: ' + json.dumps(diagnostic, sort_keys=True))
            raise ValueError(f'{label} failed with exit code {process.returncode}')
        return log.read_text(encoding='utf-8') if capture else None

    def runtime_probe(self, probe, *, macos):
        from native import confinement_kind, isolate
        scratch = self.output / 'runtime-confinement'
        scratch.mkdir()
        self.result['runtime_probe_sha256'] = digest(probe)
        self.result['runtime_confinement_kind'] = 'macos-pf-offline' if macos else confinement_kind()
        if macos:
            from macos_native import execute
            def confined_runtime(argv, env):
                result = execute(Path(argv[0]), [], scratch, env, operation='runtime-identity')
                self.result['runtime_confinement'] = getattr(result, 'confinement_evidence', None)
                return result
            executor = confined_runtime
        else:
            executor = lambda argv, env: isolate(Path(argv[0]), [], scratch, env)
        output = self.command('runtime-identity', [probe], executor=executor, capture=True)
        self.result['runtime_identity'] = parse_identity(output, edition=self.cell['edition'])

    def native_suite(self, label):
        mode, selection = native_configuration(label)
        output = self.output / ('native-' + label)
        self.command('native-' + label, [sys.executable, 'ci/native.py', '--mode', mode,
            '--feature-set', selection, '--target', self.cell['target'], '--output', output])
        self.result.setdefault('native_suites', {})[label] = read_json(output / 'native.json')

    def pure_tests(self, label, args):
        # The shared loader treats this directory as an exact selection with
        # no fallback. Ordinary tests cannot accidentally initialize the SDK;
        # loader-contract fixtures still choose their explicit fake modules.
        unavailable = self.output / (label + '-no-runtime')
        unavailable.mkdir()
        self.command(label, args, env_overrides={'NDI_RUNTIME_DIR_V6': str(unavailable)})

    def sdk(self, role, key, supplied):
        if supplied:
            if os.environ.get('GITHUB_ACTIONS') == 'true':
                raise ValueError('CI requires verified archive provisioning; local SDK override is forbidden')
            root = supplied.resolve()
            result = {'package': key, 'root': str(root), 'archive_sha256': None, 'local_override': True}
        elif self.sdk_provider is not None:
            result = self.sdk_provider(key)
            root = Path(result['root'])
        else:
            prefix = 'NDI_HEADER' if role == 'headers' else 'NDI_RUNTIME'
            result = provision(key, self.output / role, os.environ.get(prefix + '_URL', ''),
                               os.environ.get(prefix + '_SHA256'), sources=environment_sources())
            root = Path(result['root'])
        self.result[role] = ({k: v for k, v in result.items() if k != 'root'} if supplied else sanitized_receipt(result))
        return root

    def configure_target(self, profile):
        target = profile['target']
        clang_target = target
        cargo_target = target.upper().replace('-', '_')
        if profile['os'] == 'android':
            ndk = Path(self.env.get('ANDROID_NDK_HOME', ''))
            host = 'darwin-x86_64' if platform.system() == 'Darwin' else 'linux-x86_64'
            tools = ndk / 'toolchains/llvm/prebuilt' / host
            stem = 'armv7a-linux-androideabi' if target == 'armv7-linux-androideabi' else target
            header = 'arm-linux-androideabi' if target == 'armv7-linux-androideabi' else target
            linker = tools / 'bin' / (stem + '24-clang')
            if not linker.is_file():
                raise ValueError('configured Android NDK r27d/API24 compiler is unavailable')
            self.env['CARGO_TARGET_' + cargo_target + '_LINKER'] = str(linker)
            self.abi_cc = str(linker)
            # Select API24 through Clang's target, including libclang/bindgen.
            # Redefining __ANDROID_API__ conflicts with the Android driver's
            # builtin macro and fails the independent ABI probe's -Werror check.
            self.clang_args += ['--target=' + stem + '24', '--sysroot=' + str(tools / 'sysroot'),
                               '-isystem', str(tools / 'sysroot/usr/include' / header)]
        elif profile['os'] in ('ios', 'tvos', 'visionos'):
            sdk = {'ios': 'iphonesimulator' if 'sim' in profile['id'] else 'iphoneos',
                   'tvos': 'appletvsimulator' if 'sim' in profile['id'] else 'appletvos',
                   'visionos': 'xros'}[profile['os']]
            sdk_path = self.command('apple-sdk', ['xcrun', '--sdk', sdk, '--show-sdk-path'], capture=True).strip()
            self.env['SDKROOT'] = sdk_path
            self.env[{'ios': 'IPHONEOS_DEPLOYMENT_TARGET', 'tvos': 'TVOS_DEPLOYMENT_TARGET', 'visionos': 'XROS_DEPLOYMENT_TARGET'}[profile['os']]] = '1.0' if profile['os'] == 'visionos' else '16.0'
            self.clang_args += ['-isysroot', sdk_path]
            os_name = {'ios': 'ios16.0', 'tvos': 'tvos16.0', 'visionos': 'xros1.0'}[profile['os']]
            arch = 'arm64' if profile['arch'] == 'aarch64' else profile['arch']
            clang_target = arch + '-apple-' + os_name + ('-simulator' if 'sim' in profile['id'] else '')
            self.result['apple_toolchain'] = self.command('xcode-version', ['xcodebuild', '-version'], capture=True)
        elif profile['os'] == 'macos':
            sdk_path = self.command('apple-sdk', ['xcrun', '--sdk', 'macosx', '--show-sdk-path'], capture=True).strip()
            self.env['SDKROOT'] = sdk_path
            self.clang_args += ['-isysroot', sdk_path]
            self.result['apple_toolchain'] = self.command('xcode-version', ['xcodebuild', '-version'], capture=True)
        elif profile['os'] == 'linux' and profile['id'] not in NATIVE:
            bundle = self.output / 'target-toolchain'
            local_bundle = self.env.get('NDI_LOCAL_TOOLCHAIN')
            if local_bundle and self.env.get('GITHUB_ACTIONS') != 'true':
                bundle = Path(local_bundle).resolve()
            else:
                if self.cell.get('toolchain_provider') != 'public-gnu':
                    raise ValueError('profile has no reviewed public toolchain provider')
                prepared = prepare_public_toolchain(profile['id'], bundle)
                bundle = Path(prepared['root'])
                self.result.update(toolchain_provider='public-gnu',
                    toolchain_recipe_sha256=prepared['recipe_sha256'],
                    toolchain_packages=prepared['package_receipts'])
            config = read_json(bundle / 'ci-toolchain.json')
            if config.get('profile') != profile['id'] or config.get('target') != target:
                raise ValueError('toolchain manifest must match the exact artifact profile and Rust target')
            def member(name):
                path = (bundle / config[name]).resolve()
                if not path.is_relative_to(bundle.resolve()) or not path.exists():
                    raise ValueError(f'toolchain {name} must exist inside its verified bundle')
                return path
            self.link_cc = str(member('linker'))
            self.link_args = config.get('c_args', [])
            self.result['toolchain_config_sha256'] = digest(bundle / 'ci-toolchain.json')
            self.env['CARGO_TARGET_' + cargo_target + '_LINKER'] = self.link_cc
            self.clang_args += ['--sysroot=' + str(member('sysroot'))]
            self.clang_args += config.get('clang_args', [])
            if config.get('rustflags'):
                self.env['CARGO_ENCODED_RUSTFLAGS'] = '\x1f'.join(config['rustflags'])
        if profile['os'] != 'android':
            self.clang_args.insert(0, '--target=' + clang_target)
        self.env['BINDGEN_EXTRA_CLANG_ARGS_' + target.replace('-', '_')] = ' '.join(shlex.quote(x) for x in self.clang_args)

    def run(self, args):
        if self.env.get('GITHUB_ACTIONS') == 'true':
            if self.env.get('NDI_CI_RELEASED') != 'true':
                raise ValueError('SDK CI remains disabled until publication is authorized')
            if self.result['dirty']:
                raise ValueError('hosted CI must use a clean exact commit')
        profile = profiles()[self.cell['profile']]
        header_root = self.sdk('headers', self.cell['header_package'], args.header_sdk)
        runtime_root = header_root if self.cell['runtime_package'] == self.cell['header_package'] and not args.runtime_sdk else self.sdk('runtime', self.cell['runtime_package'], args.runtime_sdk)
        if runtime_root == header_root:
            self.result['runtime'] = dict(self.result['headers'])
        toolchain = self.cell['toolchain']
        self.env.update({'RUSTUP_TOOLCHAIN': toolchain, 'NDI_SDK_DIR': str(header_root),
                         'NDI_SDK_PROFILE': self.cell['profile'], 'NDI_SDK_EDITION': self.cell['edition'],
                         'CARGO_TARGET_DIR': str(self.target_directory), 'CARGO_TERM_COLOR': 'never',
                         'RUSTDOCFLAGS': '-D warnings'})
        if args.install_toolchain and self.rust_setup is not None:
            self.rust_setup(self)
        elif args.install_toolchain:
            components = ['rust-src'] if self.cell['build_std'] else ['clippy']
            self.command('install-rust', ['rustup', 'toolchain', 'install', toolchain, '--profile', 'minimal', '--component', ','.join(components)])
            if not self.cell['build_std']:
                self.command('install-target', ['rustup', 'target', 'add', '--toolchain', toolchain, self.cell['target']])
        if 'rustc' not in self.result:
            self.result['rustc'] = self.command('rustc-version', ['rustc', '-vV'], capture=True)
        self.configure_target(profile)
        lib_root = next((runtime_root / n for n in ('lib', 'Lib') if (runtime_root / n).is_dir()), None)
        if lib_root is None:
            raise ValueError('runtime library root is absent')
        directory = (runtime_root / 'Bin' / profile['subdir']) if profile['os'] == 'windows' else lib_root / profile['subdir']
        self.env['NDI_RUNTIME_DIR_V6'] = str(directory)
        search = 'PATH' if profile['os'] == 'windows' else 'DYLD_LIBRARY_PATH' if profile['os'] == 'macos' else 'LD_LIBRARY_PATH'
        self.env[search] = str(directory) + os.pathsep + self.env.get(search, '')
        names = {'windows': f"Processing.NDI.Lib.{ 'Advanced.' if self.cell['edition'] == 'advanced' else ''}{'x64' if profile['arch'] == 'x86_64' else 'x86'}.dll",
                 'macos': 'libndi_advanced.dylib' if self.cell['edition'] == 'advanced' else 'libndi.dylib',
                 'linux': 'libndi_advanced.so' if self.cell['edition'] == 'advanced' else 'libndi.so',
                 'android': 'libndi_advanced.so' if self.cell['edition'] == 'advanced' else 'libndi.so'}
        if profile['os'] in names:
            library = directory / names[profile['os']]
            if not library.is_file():
                raise ValueError('selected edition runtime artifact is absent')
            self.result['runtime_binary_sha256'] = digest(library)
            self.env['NDI_EXPECTED_RUNTIME'] = str(library.resolve())
            include = next((header_root / n for n in ('include', 'Include') if (header_root / n).is_dir()), None)
            link_library = lib_root / profile['subdir'] / library.with_suffix('.lib').name if profile['os'] == 'windows' else library
            probe = self.output / ('sdk-link.exe' if profile['os'] == 'windows' else 'sdk-link')
            cc = self.link_cc or self.abi_cc
            cc_args = self.link_args if self.link_cc else self.clang_args
            self.command('link-sdk-smoke', [cc, *cc_args, '-I', include, 'ci/link_probe.c', link_library, '-o', probe])
        flags = ['--target', self.cell['target']]
        if self.cell['build_std']:
            flags += ['-Zbuild-std=std,panic_abort']
        if not (ROOT / 'Cargo.lock').is_file():
            if self.env.get('GITHUB_ACTIONS') == 'true':
                raise ValueError('shared CI dependency lockfile was not supplied')
            self.command('resolve-lockfile', ['cargo', 'generate-lockfile'])
        self.result['lockfile_sha256'] = digest(ROOT / 'Cargo.lock')
        # Mobile coverage comes from the library and the final all-feature app
        # below. Desktop CLI examples add ctrlc/nix process APIs unavailable on
        # tvOS, independently of the supported library feature surface.
        build_label = 'build-library' if self.cell['suite'] == 'mobile' else 'link-examples'
        build_selection = '--lib' if self.cell['suite'] == 'mobile' else '--examples'
        build_log = self.command(build_label, ['cargo', 'build', '--locked', build_selection, '--all-features',
            '--message-format=json-render-diagnostics', *flags], capture=True)
        artifacts = cargo_artifacts.messages(build_log)
        target_root = self.target_directory / self.cell['target']
        bindings = cargo_artifacts.generated_bindings(artifacts, target_root)
        abi_rust_args = []
        if self.cell['build_std']:
            abi_rust_args = ['--rustc-arg=' + value
                for value in cargo_artifacts.rebuilt_std_arguments(artifacts, target_root)]
        self.command('abi-probe', [sys.executable, 'tools/sdk/abi_probe.py', '--sdk-root', header_root,
            '--rust-bindings', bindings, '--cc', self.abi_cc, *abi_rust_args, '--target', self.cell['target'], '--output', self.output / 'abi.json',
            *['--cc-arg=' + x for x in self.clang_args]])
        if self.cell['headers'] == 'current':
            for probe in ('discovery', 'file'):
                self.command(probe + '-abi-probe', [sys.executable, 'tools/sdk/' + probe + '_probe.py',
                    '--sdk-root', header_root, '--cc', self.abi_cc, *abi_rust_args, '--target', self.cell['target'],
                    '--output', self.output / (probe + '-abi.json'),
                    *['--cc-arg=' + x for x in self.clang_args]])
        suite = self.cell['suite']
        if self.cell['profile'] == 'x86_64-linux-gnu' and self.cell['headers'] == 'current' and suite == 'native':
            includes = []
            include_root = next((header_root / n for n in ('include', 'Include') if (header_root / n).is_dir()), None)
            for name in ('Processing.NDI.Advanced.h', 'Processing.NDI.Lib.File.h'):
                if include_root and (include_root / name).is_file():
                    includes += ['--include-header', name]
            self.command('sdk-snapshot', [sys.executable, 'tools/sdk/audit.py', 'snapshot',
                '--sdk-root', header_root, '--edition', self.cell['edition'], '--target', self.cell['target'],
                '--profile', self.cell['profile'], '--output', self.output / 'sdk.json', *includes])
            self.command('sdk-api-coverage', [sys.executable, 'tools/sdk/audit.py', 'classify',
                '--manifest', self.output / 'sdk.json', '--output', self.output / 'api-coverage.json',
                '--require-implemented'])
        if suite in ('native', 'compat'):
            library = cargo_artifacts.library(artifacts, 'grafton_ndi', '.rlib', target_root)
            probe = self.output / ('runtime-probe.exe' if profile['os'] == 'windows' else 'runtime-probe')
            self.command('link-runtime-probe', ['rustc', '--edition=2021', '--target', self.cell['target'],
                'ci/runtime_probe.rs', '--extern', 'grafton_ndi=' + str(library),
                *cargo_artifacts.dependency_arguments(artifacts, target_root),
                *cargo_artifacts.native_link_arguments(artifacts, target_root), '-o', probe])
            self.runtime_probe(probe, macos=profile['os'] == 'macos')
            self.pure_tests('tests', ['cargo', 'test', '--locked', '--all-features', *flags])
            for mode in native_modes(self.cell):
                self.native_suite(mode)
            if suite == 'native':
                fresh = self.command('fresh-build', ['cargo', 'build', '--locked', '--examples', '--all-features', *flags], capture=True)
                if re.search(r'^\s*Compiling grafton-ndi v', fresh, re.MULTILINE):
                    raise ValueError('unchanged build unexpectedly rebuilt grafton-ndi')
                self.command('sdk-regressions', [sys.executable, 'ci/native.py', '--mode', 'regressions',
                    '--target', self.cell['target'], '--output', self.output / 'sdk-regressions'])
                self.result['sdk_regressions'] = read_json(self.output / 'sdk-regressions/native.json')
                self.command('clippy', ['cargo', 'clippy', '--locked', '--all-targets', '--all-features', *flags, '--', '-D', 'warnings'])
        elif suite == 'msrv':
            self.command('msrv-check', ['cargo', 'check', '--locked', '--all-targets', '--all-features', *flags])
        elif suite == 'mobile':
            mobile_flags = ['--build-std'] if self.cell['build_std'] else []
            execute = self.cell['coverage'] in ('emulator', 'simulator')
            if execute:
                mobile_flags += ['--execute']
            self.command('mobile-package', [sys.executable, 'ci/mobile.py', '--profile', self.cell['profile'],
                '--edition', self.cell['edition'], '--sdk-root', runtime_root, '--output', self.output / 'mobile', *mobile_flags])
            self.result['mobile'] = read_json(self.output / 'mobile/mobile.json')
            if self.result['mobile']['coverage'] != self.cell['coverage']:
                raise ValueError('mobile execution coverage differs from required policy')
            if execute:
                self.commands.append({'label': 'mobile-runtime', 'exit_code': 0,
                    'evidence_sha256': digest(self.output / 'mobile/mobile.json')})
        elif suite == 'semver':
            self.command('install-semver', ['cargo', 'install', 'cargo-semver-checks', '--version', '0.51.0', '--locked'])
            self.command('semver', ['cargo', 'semver-checks', '--all-features', '--target', self.cell['target']])
        elif suite == 'features':
            for index in range(len(FEATURE_SETS)):
                self.pure_tests(f'features-{index}', ['cargo', 'test', '--locked', *feature_flags(str(index)), *flags])
                self.native_suite(f'core-{index}')
            self.command('rustdoc', ['cargo', 'doc', '--locked', '--no-deps', '--all-features', *flags])
        completed = {command['label'] for command in self.commands if command['exit_code'] == 0}
        if not required_steps(self.cell).issubset(completed):
            raise ValueError('cell omitted a required validation step')
        self.result['status'] = 'passed'


def execute_cell(run, args):
    """Always retain a bounded cell result; detailed exceptions stay private."""
    try:
        run.run(args)
    except Exception as error:
        run.result['status'] = 'failed'
        with (run.output / 'failure.log').open('w', encoding='utf-8') as stream:
            traceback.print_exc(file=stream)
        try:
            diagnostic = summarize_exception(error, ROOT)
        except (OSError, ValueError, subprocess.SubprocessError):
            diagnostic = {'schema': 1, 'reasons': ['diagnostic-summary-unavailable']}
        run.result['failure'] = diagnostic
        print('Cell failed; bounded diagnostics: ' + json.dumps(diagnostic, sort_keys=True), file=sys.stderr)
    finally:
        write_json(run.output / 'result.json', run.result)
    return 0 if run.result['status'] == 'passed' else 1


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--cell', required=True)
    p.add_argument('--scope', choices=('required', 'extended'), default='required')
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--header-sdk', type=Path, help='local-only extracted SDK override; cannot satisfy hosted release gate')
    p.add_argument('--runtime-sdk', type=Path)
    p.add_argument('--install-toolchain', action='store_true')
    args = p.parse_args()
    cells = {c['id']: c for c in make_plan(profiles(), args.scope)}
    if args.cell not in cells:
        p.error('unknown cell in selected scope; generate ci/plan.py first')
    return execute_cell(CellRun(cells[args.cell], args.output), args)

if __name__ == '__main__':
    raise SystemExit(main())
