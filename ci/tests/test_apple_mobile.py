"""Execute Apple mobile CI with recording tools to verify every lint command."""
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'ci' / 'apple-mobile.sh'
PINNED_TOOLCHAIN = tomllib.loads((ROOT / 'rust-toolchain.toml').read_text(encoding='utf-8'))['toolchain']['channel']
CI_CHOICES = {
    'aarch64-apple-ios': (PINNED_TOOLCHAIN, '16.0'),
    'x86_64-apple-ios': (PINNED_TOOLCHAIN, '16.0'),
    'aarch64-apple-tvos': (PINNED_TOOLCHAIN, '16.0'),
    'aarch64-apple-visionos': (PINNED_TOOLCHAIN, '1.0'),
    'x86_64-apple-tvos': ('nightly-2026-11-10', '16.0'),
}


def static_targets(source):
    """Read the target and deployment input from the build script's one table."""
    table = re.search(r'pub const STATIC_TARGETS: &\[StaticTarget\] = &\[(.*?)\];',
                      source, re.DOTALL).group(1)
    rows = re.findall(r'StaticTarget\s*\{(.*?)\}', table, re.DOTALL)
    return {re.search(r'triple:\s*"([^"]+)"', row).group(1):
            re.search(r'deployment_variable:\s*"([^"]+)"', row).group(1)
            for row in rows}


def documented_static_targets(source):
    return {target for row in source.splitlines()
            if re.match(r'\| (iOS|tvOS|visionOS) \|', row)
            for target in re.findall(r'`([^`]+)`', row.split('|')[2])}


def commands(script, rejection='unsupported'):
    """Run the real script without downloads, compilation or an Apple SDK."""
    with tempfile.TemporaryDirectory(prefix='apple-mobile-ci-') as temporary:
        root = Path(temporary)
        log = root / 'commands.jsonl'
        recorder = f'#!{sys.executable}\n' + '''import json, os, pathlib, sys
with open(os.environ['APPLE_MOBILE_COMMANDS'], 'a', encoding='utf-8') as log:
    log.write(json.dumps({'tool': pathlib.Path(sys.argv[0]).name,
                          'arguments': sys.argv[1:],
                          'environment': {name: os.environ.get(name) for name in
                                          ('IPHONEOS_DEPLOYMENT_TARGET', 'TVOS_DEPLOYMENT_TARGET',
                                           'XROS_DEPLOYMENT_TARGET')}}) + '\\n')
if pathlib.Path(sys.argv[0]).name == 'cargo' and 'check' in sys.argv:
    rejection = os.environ['APPLE_MOBILE_REJECTION']
    if rejection == 'accepted':
        sys.exit(0)
    message = ('Unsupported Apple static target aarch64-apple-ios-sim; see docs/setup.md'
               if rejection == 'unsupported' else 'unrelated compilation error')
    print(message, file=sys.stderr)
    sys.exit(1)
'''
        for tool in ('cargo', 'rustup'):
            executable = root / tool
            executable.write_text(recorder, encoding='utf-8')
            executable.chmod(0o755)
        (root / 'ci').mkdir()
        (root / 'rust-toolchain.toml').write_text((ROOT / 'rust-toolchain.toml').read_text(encoding='utf-8'))
        source = root / 'ci' / 'apple-mobile.sh'
        source.write_text(script, encoding='utf-8')
        environment = {
            **os.environ,
            'PATH': str(root) + os.pathsep + os.environ.get('PATH', os.defpath),
            'NDI_SDK_DIR': str(root / 'sdk'),
            'APPLE_MOBILE_COMMANDS': str(log),
            'APPLE_MOBILE_REJECTION': rejection,
        }
        subprocess.run(['bash', str(source)], env=environment, check=True,
                       capture_output=True, text=True)
        return [json.loads(line) for line in log.read_text(encoding='utf-8').splitlines()]


def lint_violations(recorded, table=None, documentation=None):
    if table is None:
        table = static_targets((ROOT / 'build_support.rs').read_text(encoding='utf-8'))
    if documentation is None:
        documentation = (ROOT / 'docs/setup.md').read_text(encoding='utf-8')
    cargo = [command for command in recorded
             if command['tool'] == 'cargo' and 'clippy' in command['arguments']]
    failures = []
    if set(table) != set(CI_CHOICES):
        failures.append('static target table and CI toolchain/deployment choices differ')
    if set(table) != documented_static_targets(documentation):
        failures.append('documented Apple static targets differ from the build table')
    negative = [command['arguments'] for command in recorded
                if command['tool'] == 'cargo' and 'check' in command['arguments']]
    if negative != [[f'+{PINNED_TOOLCHAIN}', 'check', '--locked', '--lib', '--target', 'aarch64-apple-ios-sim']]:
        failures.append('must reject the ARM64 simulator with the build-script diagnostic')
    if len(cargo) != 2 * len(table):
        failures.append('every target must lint both feature sets')
    for target, variable in table.items():
        if target not in CI_CHOICES:
            continue
        toolchain, minimum = CI_CHOICES[target]
        calls = [command for command in cargo if target in command['arguments']]
        for all_features in (False, True):
            expected = [f'+{toolchain}', 'clippy', '--locked', '--lib']
            if all_features:
                expected.append('--all-features')
            expected.extend(['--target', target])
            if target == 'x86_64-apple-tvos':
                expected.append('-Zbuild-std=std,panic_abort')
            expected.extend(['--', '-D', 'warnings'])
            matching = [command for command in calls if command['arguments'] == expected]
            if (len(matching) != 1 or matching[0]['environment'].get(variable) != minimum
                    or any(value is not None for name, value in matching[0]['environment'].items()
                           if name != variable)):
                failures.append(f'{target}: missing lint or deployment floor ({all_features=})')
    return failures


class AppleMobileTests(unittest.TestCase):
    def test_added_table_target_requires_lint_and_documentation(self):
        source = (ROOT / 'build_support.rs').read_text(encoding='utf-8').replace(
            'pub const STATIC_TARGETS: &[StaticTarget] = &[',
            'pub const STATIC_TARGETS: &[StaticTarget] = &[\n'
            'StaticTarget { triple: "extra-apple-ios", '
            'deployment_variable: "IPHONEOS_DEPLOYMENT_TARGET" },')
        failures = lint_violations(commands(SCRIPT.read_text(encoding='utf-8')),
                                   table=static_targets(source))
        self.assertIn('static target table and CI toolchain/deployment choices differ', failures)
        self.assertIn('documented Apple static targets differ from the build table', failures)

    def test_missing_target_lint_is_rejected(self):
        source = SCRIPT.read_text(encoding='utf-8').replace(
            'lint "$release" aarch64-apple-ios IPHONEOS_DEPLOYMENT_TARGET 16.0', '')
        self.assertTrue(lint_violations(commands(source)))

    def test_wrong_deployment_variable_is_rejected(self):
        source = SCRIPT.read_text(encoding='utf-8').replace(
            'lint "$release" aarch64-apple-ios IPHONEOS_DEPLOYMENT_TARGET',
            'lint "$release" aarch64-apple-ios TVOS_DEPLOYMENT_TARGET')
        self.assertTrue(lint_violations(commands(source)))

    def test_every_static_target_denies_warnings_with_both_feature_sets(self):
        recorded = commands(SCRIPT.read_text(encoding='utf-8'))
        self.assertEqual(lint_violations(recorded), [])
        rustup = [command['arguments'] for command in recorded if command['tool'] == 'rustup']
        self.assertIn(['component', 'add', '--toolchain', PINNED_TOOLCHAIN, 'clippy'], rustup)
        self.assertIn(['toolchain', 'install', 'nightly-2026-11-10', '--profile', 'minimal',
                       '--component', 'rust-src', '--component', 'clippy'], rustup)
        for command in rustup:
            if '--toolchain' in command:
                self.assertEqual(command[command.index('--toolchain') + 1], PINNED_TOOLCHAIN)

    def test_missing_warning_denial_is_rejected(self):
        source = SCRIPT.read_text(encoding='utf-8').replace('-- -D warnings', '')
        self.assertTrue(lint_violations(commands(source)))

    def test_missing_feature_set_is_rejected(self):
        source = SCRIPT.read_text(encoding='utf-8').replace('--all-features ', '')
        self.assertTrue(lint_violations(commands(source)))

    def test_successful_unsupported_target_build_is_rejected(self):
        with self.assertRaises(subprocess.CalledProcessError):
            commands(SCRIPT.read_text(encoding='utf-8'), rejection='accepted')

    def test_unrelated_build_failure_is_rejected(self):
        with self.assertRaises(subprocess.CalledProcessError):
            commands(SCRIPT.read_text(encoding='utf-8'), rejection='unrelated')


if __name__ == '__main__':
    unittest.main()
