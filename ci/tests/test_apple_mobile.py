"""Execute Apple mobile CI with recording tools to verify every lint command."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'ci' / 'apple-mobile.sh'
TARGETS = {
    'aarch64-apple-ios': ('stable', 'IPHONEOS_DEPLOYMENT_TARGET', '16.0'),
    'x86_64-apple-ios': ('stable', 'IPHONEOS_DEPLOYMENT_TARGET', '16.0'),
    'aarch64-apple-tvos': ('stable', 'TVOS_DEPLOYMENT_TARGET', '16.0'),
    'aarch64-apple-visionos': ('stable', 'XROS_DEPLOYMENT_TARGET', '1.0'),
    'x86_64-apple-tvos': ('nightly-2026-11-10', 'TVOS_DEPLOYMENT_TARGET', '16.0'),
}


def commands(script):
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
'''
        for tool in ('cargo', 'rustup'):
            executable = root / tool
            executable.write_text(recorder, encoding='utf-8')
            executable.chmod(0o755)
        source = root / 'apple-mobile.sh'
        source.write_text(script, encoding='utf-8')
        environment = {
            **os.environ,
            'PATH': str(root) + os.pathsep + os.environ.get('PATH', os.defpath),
            'NDI_SDK_DIR': str(root / 'sdk'),
            'APPLE_MOBILE_COMMANDS': str(log),
        }
        subprocess.run(['bash', str(source)], env=environment, check=True,
                       capture_output=True, text=True)
        return [json.loads(line) for line in log.read_text(encoding='utf-8').splitlines()]


def lint_violations(recorded):
    cargo = [command for command in recorded if command['tool'] == 'cargo']
    failures = []
    if len(cargo) != 2 * len(TARGETS):
        failures.append('every target must lint both feature sets')
    for target, (toolchain, variable, minimum) in TARGETS.items():
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
            if len(matching) != 1 or matching[0]['environment'].get(variable) != minimum:
                failures.append(f'{target}: missing lint or deployment floor ({all_features=})')
    return failures


class AppleMobileTests(unittest.TestCase):
    def test_every_static_target_denies_warnings_with_both_feature_sets(self):
        recorded = commands(SCRIPT.read_text(encoding='utf-8'))
        self.assertEqual(lint_violations(recorded), [])
        rustup = [command['arguments'] for command in recorded if command['tool'] == 'rustup']
        self.assertIn(['component', 'add', '--toolchain', 'stable', 'clippy'], rustup)
        self.assertIn(['toolchain', 'install', 'nightly-2026-11-10', '--profile', 'minimal',
                       '--component', 'rust-src', '--component', 'clippy'], rustup)

    def test_missing_warning_denial_is_rejected(self):
        source = SCRIPT.read_text(encoding='utf-8').replace('-- -D warnings', '')
        self.assertTrue(lint_violations(commands(source)))

    def test_missing_feature_set_is_rejected(self):
        source = SCRIPT.read_text(encoding='utf-8').replace('--all-features ', '')
        self.assertTrue(lint_violations(commands(source)))


if __name__ == '__main__':
    unittest.main()
