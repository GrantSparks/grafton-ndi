"""Guards on the workflow files, the files CI obeys without naming them, and the README's matrix table.

`violations` holds the security rules. It is run on the real workflows, which must
comply, and on a list of plausible wrong edits, each of which it must report.
actionlint, in the same job, checks that the files are valid workflows at all.
"""
from pathlib import Path
import re
import subprocess
import sys
import tomllib
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import sdk  # noqa: E402
import workflows  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / '.github' / 'workflows'

READ_ONLY = {'contents': 'read'}
# The one job that may hold more than read access: the crates.io publishing identity.
JOB_PERMISSIONS = {('publish.yml', 'publish'): {'contents': 'read', 'id-token': 'write'}}
PASSED_SECRETS = {name: f'${{{{ secrets.{name} }}}}' for name in ('NDI_SDK_SOURCES_JSON', 'NDI_SDK_ARCHIVE_TOKEN')}
# Every place where the word `secrets` may occur, as a key or inside any scalar.
SECRET_USES = {
    'rust.yml': {'on.workflow_call.secrets (key)', *(f'jobs.sdk.steps[Provision the SDK].env.{name}'
                                                    for name in PASSED_SECRETS)},
    'publish.yml': {'jobs.ci.secrets (key)', *(f'jobs.ci.secrets.{name}' for name in PASSED_SECRETS)},
}
# Where the secrets exist: not in pull requests from forks, and not in runs that Dependabot starts.
SECRETS_AVAILABLE = ("github.actor != 'dependabot[bot]' && (github.event_name != 'pull_request' || "
                     "github.event.pull_request.head.repo.full_name == github.repository)")
JOB_CONDITIONS = {
    'rust.yml': {'sdk': f"vars.NDI_CI_RELEASED == 'true' && {SECRETS_AVAILABLE}", 'required-ci': 'always()'},
    'publish.yml': {'release': "github.repository == 'GrantSparks/grafton-ndi'"},
}
TRIGGERS = {
    'rust.yml': {'push': {'branches': ['main']}, 'pull_request': {'branches': ['main']}, 'workflow_dispatch': None,
                 'workflow_call': {'secrets': {name: {'required': 'false'} for name in PASSED_SECRETS}}},
    'publish.yml': {'push': {'tags': ['v*']}},
}
# The actions each job uses, in order. The sdk job has SDK files on disk, so it has no
# cache and no artifact action; the publish job holds the registry identity.
ACTIONS = {
    'rust.yml': {
        'lint': ['actions/checkout', 'actions/setup-python', 'dtolnay/rust-toolchain', 'Swatinem/rust-cache'],
        'test': ['actions/checkout', 'dtolnay/rust-toolchain', 'Swatinem/rust-cache'],
        'cross': ['actions/checkout', 'dtolnay/rust-toolchain', 'Swatinem/rust-cache'],
        'sdk': ['actions/checkout', 'actions/setup-python', 'dtolnay/rust-toolchain'],
        'required-ci': ['actions/checkout'],
    },
    'publish.yml': {
        'release': ['actions/checkout'],
        'ci': './.github/workflows/rust.yml',
        'verify': ['actions/checkout', 'dtolnay/rust-toolchain', 'actions/upload-artifact'],
        'publish': ['actions/checkout', 'rust-lang/crates-io-auth-action'],
    },
}
PUBLISH_NEEDS = {'release': None, 'ci': 'release', 'verify': 'ci', 'publish': 'verify'}
OUTSIDE_THE_CHECKOUT = '${{ runner.temp }}'
MANIFEST = '--manifest-path "$GITHUB_WORKSPACE/Cargo.toml"'


def read_workflows():
    return {path.name: path.read_text(encoding='utf-8') for path in sorted(WORKFLOWS.glob('*.yml'))}


def differs(what, actual, expected):
    return [] if actual == expected else [f'{what} is {actual!r}, not {expected!r}']


def violations(texts):
    """Every way the workflows break a rule; empty when they comply."""
    if set(texts) != set(ACTIONS):
        return [f'the workflows are {sorted(texts)}, not {sorted(ACTIONS)}']
    try:
        parsed = {name: workflows.load(text) for name, text in texts.items()}
    except workflows.Unsupported as error:
        return [f'a workflow uses YAML the guards do not read: {error}']
    found = []
    for name, workflow in parsed.items():
        jobs = workflow['jobs']
        found += differs(f'{name}: triggers', workflow.get('on'), TRIGGERS[name])
        found += differs(f'{name}: default permissions', workflow.get('permissions'), READ_ONLY)
        found += differs(f'{name}: places that mention secrets',
                         {place for place, text in workflows.entries(workflow) if 'secrets' in text.lower()},
                         SECRET_USES[name])
        found += differs(f'{name}: job conditions', {job: body['if'] for job, body in jobs.items() if 'if' in body},
                         JOB_CONDITIONS[name])
        found += [f'{name}: {place} lets a failure pass' for place, text in workflows.entries(workflow)
                  if text == 'continue-on-error' and place.endswith(' (key)')]
        for job, body in jobs.items():
            found += differs(f'{name}: permissions of {job}', body.get('permissions'), JOB_PERMISSIONS.get((name, job)))
            used = body['uses'] if 'uses' in body else [step['uses'] for step in body['steps'] if 'uses' in step]
            pinned = [used] if isinstance(used, str) else [re.sub(r'@[0-9a-f]{40}$', '', action) for action in used]
            found += differs(f'{name}: actions of {job}, each pinned to a commit',
                             pinned[0] if isinstance(used, str) else pinned, ACTIONS[name].get(job))
            if 'runs-on' in body and not re.fullmatch(r'\d+', body.get('timeout-minutes', '')):
                found.append(f'{name}: {job} has no timeout')
    return found + gate_violations(parsed['rust.yml']) + publish_violations(parsed['publish.yml'])


def gate_violations(rust):
    """required-ci must wait for every other job, always run, and expect the sdk job exactly when it can run."""
    jobs = rust['jobs']
    gate = jobs['required-ci']
    step = gate['steps'][-1]
    return (differs('rust.yml: jobs required-ci waits for', sorted(gate.get('needs') or []),
                    sorted(set(jobs) - {'required-ci'}))
            + differs('rust.yml: the gate step', (step.get('run'), step.get('env')), ('python3 ci/gate.py', {
                'RESULTS': '${{ toJSON(needs) }}', 'SDK_EXPECTED': f'${{{{ {SECRETS_AVAILABLE} }}}}'}))
            + differs('rust.yml: the step given the secrets',
                      [(item.get('name'), item['env']) for item in jobs['sdk']['steps']
                       if any('secrets' in value.lower() for value in (item.get('env') or {}).values())],
                      [('Provision the SDK', PASSED_SECRETS)]))


def publish_violations(publish):
    """The job that holds the publishing identity runs nothing from the repository."""
    jobs = publish['jobs']
    job = jobs['publish']
    scripts = [step for step in job['steps'] if 'run' in step]
    text = '\n'.join(step['run'] for step in scripts)
    cargo = re.findall(r'^\s*cargo (\w+)(.*)$', text, re.MULTILINE)
    found = (differs('publish.yml: job order', {name: body.get('needs') for name, body in jobs.items()}, PUBLISH_NEEDS)
             + differs('publish.yml: secrets passed to CI', jobs['ci'].get('secrets'), PASSED_SECRETS)
             + differs('publish.yml: environment of publish', job.get('environment'), 'crates-io')
             + differs('publish.yml: values publish takes from verify', job.get('env'), {
                 'VERIFIED_SHA256': '${{ needs.verify.outputs.sha256 }}',
                 'VERIFIED_TOOLCHAIN': '${{ needs.verify.outputs.toolchain }}'})
             + differs('publish.yml: cargo commands in publish', [command for command, _ in cargo], ['package', 'publish'])
             + differs('publish.yml: where the registry token goes',
                       {place: value for place, value in workflows.entries(publish) if 'steps.auth' in value.lower()},
                       {'jobs.publish.steps[Publish].env.CARGO_REGISTRY_TOKEN': '${{ steps.auth.outputs.token }}'}))
    # Cargo reads configuration from the working directory upwards, so it must not start in the checkout.
    found += [f'publish.yml: step {step["name"]!r} of publish runs in the checkout'
              for step in scripts if step.get('working-directory') != OUTSIDE_THE_CHECKOUT]
    found += [f'publish.yml: cargo {command} in publish lacks {option}' for command, arguments in cargo
              for option in ('--locked', '--no-verify', MANIFEST) if option not in arguments]
    if text.count('GITHUB_WORKSPACE') != len(cargo) or re.search(r'python|(^|[\s"\'=])(ci|scripts|xtask)/', text):
        found.append('publish.yml: publish reads or runs something from the checkout besides the manifest')
    # The verified digest is compared with the digest of the one named file, not with a file of digests.
    for needed in ('[[ "$VERIFIED_SHA256" =~ ^[0-9a-f]{64}$ ', '[ "$actual" != "$VERIFIED_SHA256" ]',
                   'actual=$(sha256sum "$CARGO_TARGET_DIR/package/grafton-ndi-${GITHUB_REF_NAME#v}.crate" | cut'):
        if needed not in text:
            found.append(f'publish.yml: publish no longer contains {needed!r}')
    return found


# Plausible wrong edits: (name, workflow, text to replace, replacement, words the report must contain).
WRONG_EDITS = (
    ('workflow-level id-token', 'publish.yml', 'permissions:\n  contents: read\n\n',
     'permissions:\n  contents: read\n  id-token: write\n\n', 'default permissions'),
    ('write-all on verify', 'publish.yml', '    name: verify package\n',
     '    name: verify package\n    permissions: write-all\n', 'permissions of verify'),
    ('id-token on lint', 'rust.yml', '    name: lint\n', '    name: lint\n    permissions:\n      id-token: write\n',
     'permissions of lint'),
    ('permissions as a flow mapping', 'rust.yml', '    name: lint\n',
     '    name: lint\n    permissions: {id-token: write}\n', 'YAML the guards do not read'),
    ('every secret to a lint step', 'rust.yml', '      - name: Format\n        run:',
     '      - name: Format\n        env:\n          ALL: ${{ toJSON(secrets) }}\n        run:', 'mention secrets'),
    ('an indexed secret to a lint step', 'rust.yml', '      - name: Format\n        run:',
     "      - name: Format\n        env:\n          T: ${{ secrets['NDI_SDK_ARCHIVE_TOKEN'] }}\n        run:",
     'mention secrets'),
    ('a secret in other letter case', 'rust.yml', '      - name: Format\n        run:',
     '      - name: Format\n        env:\n          T: ${{ SECRETS.NDI_SDK_ARCHIVE_TOKEN }}\n        run:',
     'mention secrets'),
    ('a secret inside a script', 'rust.yml', '        run: cargo fmt --all -- --check\n',
     '        run: cargo fmt --all -- --check "${{ secrets.NDI_SDK_ARCHIVE_TOKEN }}"\n', 'mention secrets'),
    ('a secret to a second sdk step', 'rust.yml', '      - name: Vendored bindings are current\n',
     '      - name: Vendored bindings are current\n        env:\n'
     '          NDI_SDK_ARCHIVE_TOKEN: ${{ secrets.NDI_SDK_ARCHIVE_TOKEN }}\n', 'mention secrets'),
    ('all secrets inherited by CI', 'publish.yml', '    secrets:\n      NDI_SDK_SOURCES_JSON: ${{ secrets.NDI_SDK_SOURCES_JSON }}\n'
     '      NDI_SDK_ARCHIVE_TOKEN: ${{ secrets.NDI_SDK_ARCHIVE_TOKEN }}\n', '    secrets: inherit\n', 'mention secrets'),
    ('a job the gate does not wait for', 'rust.yml', '  required-ci:\n',
     '  docs:\n    runs-on: ubuntu-24.04\n    timeout-minutes: 5\n    steps:\n      - run: exit 1\n\n  required-ci:\n',
     'required-ci waits for'),
    ('the gate no longer waits for sdk', 'rust.yml', '    needs: [lint, test, cross, sdk]\n',
     '    needs: [lint, test, cross]\n', 'required-ci waits for'),
    ('the gate skipped when a job fails', 'rust.yml', '    name: required-ci\n    if: always()\n',
     '    name: required-ci\n', 'job conditions'),
    ('continue-on-error on sdk', 'rust.yml', '    runs-on: ${{ matrix.runner }}\n',
     '    runs-on: ${{ matrix.runner }}\n    continue-on-error: true\n', 'lets a failure pass'),
    ('continue-on-error on a test step', 'rust.yml', '      - name: Tests, all features\n',
     '      - name: Tests, all features\n        continue-on-error: true\n', 'lets a failure pass'),
    ('a condition on lint', 'rust.yml', '    name: lint\n', "    name: lint\n    if: github.event_name == 'push'\n",
     'job conditions'),
    ('Dependabot expected to have secrets', 'rust.yml', "${{ github.actor != 'dependabot[bot]' &&\n            (", '${{ (',
     'the gate step'),
    ('sdk runs for Dependabot', 'rust.yml', " && github.actor != 'dependabot[bot]' &&\n      (", ' &&\n      (',
     'job conditions'),
    ('a cache in the sdk job', 'rust.yml', '      - name: Linux prerequisites\n',
     '      - uses: Swatinem/rust-cache@d1527c8f67ef9af357290475d4af4b0fab8f12e0 # v2.9.3\n'
     '      - name: Linux prerequisites\n', 'actions of sdk'),
    ('an upload from the sdk job', 'rust.yml', '      - name: Vendored bindings are current\n',
     '      - uses: actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9 # v7\n'
     '        with:\n          path: target\n      - name: Vendored bindings are current\n', 'actions of sdk'),
    ('an action pinned by tag', 'publish.yml', 'rust-lang/crates-io-auth-action@c6f97d42243bad5fab37ca0427f495c86d5b1a18',
     'rust-lang/crates-io-auth-action@v1', 'pinned to a commit'),
    ('a job without a timeout', 'rust.yml', '    name: cross\n    runs-on: ubuntu-24.04\n    timeout-minutes: 45\n',
     '    name: cross\n    runs-on: ubuntu-24.04\n', 'no timeout'),
    ('pull_request_target', 'rust.yml', '  pull_request:\n    branches: [main]\n',
     '  pull_request_target:\n    branches: [main]\n', 'triggers'),
    ('publishing from a branch push', 'publish.yml', "  push:\n    tags: ['v*']\n",
     "  push:\n    tags: ['v*']\n    branches: [main]\n", 'triggers'),
    ('publish without the environment', 'publish.yml', '    environment: crates-io\n', '', 'environment of publish'),
    ('publish without waiting for verify', 'publish.yml', '    name: publish to crates.io\n    needs: verify\n',
     '    name: publish to crates.io\n    needs: release\n', 'job order'),
    ('cargo package started in the checkout', 'publish.yml',
     '      - name: Re-create the verified package\n        working-directory: ${{ runner.temp }}\n',
     '      - name: Re-create the verified package\n', 'runs in the checkout'),
    ('cargo publish started in the checkout', 'publish.yml',
     '      - name: Publish\n        working-directory: ${{ runner.temp }}\n', '      - name: Publish\n',
     'runs in the checkout'),
    ('cargo publish without the manifest path', 'publish.yml',
     'cargo publish --locked --no-verify --manifest-path "$GITHUB_WORKSPACE/Cargo.toml"',
     'cd "$GITHUB_WORKSPACE" && cargo publish --locked --no-verify', 'cargo commands in publish'),
    ('publish builds the crate', 'publish.yml', 'cargo publish --locked --no-verify', 'cargo publish --locked',
     'lacks --no-verify'),
    ('publish runs a repository script', 'publish.yml', '          rustup toolchain install "$VERIFIED_TOOLCHAIN"',
     '          python3 "$GITHUB_WORKSPACE/ci/release.py" "$GITHUB_REF_NAME"\n'
     '          rustup toolchain install "$VERIFIED_TOOLCHAIN"', 'from the checkout'),
    ('the digest checked against a file of digests', 'publish.yml', 'if [ "$actual" != "$VERIFIED_SHA256" ]; then',
     'if ! sha256sum --check SHA256SUMS; then', 'no longer contains'),
    ('the digest taken unchecked', 'publish.yml', '[[ "$VERIFIED_SHA256" =~ ^[0-9a-f]{64}$ && ', '[[ ',
     'no longer contains'),
    ('the registry token to another step', 'publish.yml',
     '      - name: Re-create the verified package\n        working-directory: ${{ runner.temp }}\n',
     '      - name: Re-create the verified package\n        working-directory: ${{ runner.temp }}\n'
     '        env:\n          T: ${{ steps.auth.outputs.token }}\n', 'registry token'),
    ('a download into the publish job', 'publish.yml', '      - name: Re-create the verified package\n',
     '      - uses: actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8\n'
     '      - name: Re-create the verified package\n', 'actions of publish'),
)


class Rules(unittest.TestCase):
    def test_workflows_break_no_rule(self):
        self.assertEqual(violations(read_workflows()), [])

    def test_every_wrong_edit_is_reported(self):
        texts = read_workflows()
        self.assertEqual(len({name for name, *_ in WRONG_EDITS}), len(WRONG_EDITS))
        for name, workflow, old, new, expected in WRONG_EDITS:
            with self.subTest(name):
                self.assertEqual(texts[workflow].count(old), 1, 'the edit no longer applies to the workflow')
                report = violations({**texts, workflow: texts[workflow].replace(old, new)})
                self.assertTrue(any(expected in line for line in report), report)

    def test_reader_refuses_yaml_it_cannot_see_through(self):
        self.assertEqual(workflows.load('a:\n  - b: c\n    d: [e, \'f\']  # g\n  - h\ni: >-\n  j\n  k\nl: |\n  m\n  # n\n'),
                         {'a': [{'b': 'c', 'd': ['e', 'f']}, 'h'], 'i': 'j k', 'l': 'm\n# n'})
        for text in ('a: &x 1\n', 'a: *x\n', 'a: {b: c}\n', 'a: !!str 1\n', 'a:\n\tb: 1\n', 'a: 1\na: 2\n',
                     'a: b\n  c\n', '---\na: 1\n', 'a:\n- b\n', 'a: "x\\ny"\n', 'a: [b, [c]]\n', 'a: b: c\n'):
            with self.subTest(text=text), self.assertRaises(workflows.Unsupported):
                workflows.load(text)


def checkable(target, pin, *, all_features, headers=None, driver=None, decoders=True):
    """What tests can check in a build for `target` running against the package `pin`.

    `headers` is the package the build reads its headers from, `pin` itself
    unless given, and `driver` the driver package installed beside the runtime.
    This restates the crate's test policy: the Discovery Server tests are Linux
    only and need a 6.4 runtime; the Advanced tests need the `advanced` feature,
    an Advanced runtime and Advanced headers; the File tests need the `file-io`
    feature and a File library the runtime can drive, which is an Advanced
    runtime everywhere except Windows, where the library is edition-neutral and
    ships for x64. The H.264 and HEVC 4:4:4 decode tests are Advanced tests that
    need an NDI|HX decoder, and the HEVC one a 6.4 runtime for its Best444
    receiver. The macOS runtime decodes both itself; on Linux the decoder is the
    NDI|HX driver of the runtime's release. The Windows runtime decodes H.264
    itself (every hosted Windows Advanced row decoded it in run 43342188690), but
    the hosted Windows runners delivered the runtime's black placeholder frames
    instead of HEVC 4:4:4, so the HEVC test runs there without being counted.
    `decoders=False` leaves both out, as the compatibility runs do.
    """
    platform, advanced = pin['platform'], pin['edition'] == 'advanced'
    declared = (headers or pin)['edition'] == 'advanced'
    current = sdk.version_tuple(pin['version']) >= (6, 4, 0, 0)
    driven = (driver is not None and driver['driver'] == 'hx' and driver['platform'] == platform
              and driver['version'] == pin['version'])
    decoder = decoders and (platform == 'apple' or driven)
    h264 = decoder or (decoders and platform == 'windows')
    found = {'1'}
    if platform == 'linux' and current:
        found.add('discovery-server')
    if all_features and advanced and declared:
        found.add('advanced')
        if h264:
            found.add('h264-decoder')
        if decoder and current:
            found.add('hevc-444-decoder')
    if all_features and current and (target == 'x86_64-pc-windows-msvc' if platform == 'windows' else advanced):
        found.add('file')
    return found


def exported_promises(step, require):
    """The GRAFTON_NDI_REQUIRE_RUNTIME that each edition's pass of a compatibility step exports.

    Bash runs the step's loop with only its lines that set the promise, `matrix.require` replaced by `require`.
    """
    lines = [line for line in step['run'].splitlines()
             if line.lstrip().startswith('for edition in') or 'require' in line.lower()]
    script = '\n'.join(lines + ['  echo "$edition $GRAFTON_NDI_REQUIRE_RUNTIME"', 'done'])
    script = script.replace('${{ matrix.require }}', require)
    if '${{' in script:
        raise AssertionError(f'a promise line reads another expression:\n{script}')
    output = subprocess.run(['bash', '-eo', 'pipefail', '-c', script], capture_output=True, text=True, check=True)
    return {edition: set(promise.split(',')) | {'1'}
            for edition, promise in (line.split(' ', 1) for line in output.stdout.splitlines())}


# What every step of the sdk job after the isolation check runs on: the job was not cancelled and the runner
# was shown to isolate, whatever the steps in between did.
ROW_READY = "!cancelled() && steps.isolation.outcome == 'success'"


def when(condition=None):
    """The `if` of a step of the sdk job that runs on ROW_READY, and on `condition` when one is given."""
    return '${{ ' + ROW_READY + (f' && {condition}' if condition else '') + ' }}'


class SdkJob(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.texts = read_workflows()
        cls.job = workflows.load(cls.texts['rust.yml'])['jobs']['sdk']
        cls.rows = cls.job['strategy']['matrix']['include']
        cls.pins = sdk.load_pins()

    def test_matrix_rows_use_pinned_packages_for_their_target(self):
        self.assertGreaterEqual(len(self.rows), 9)
        for row in self.rows:
            with self.subTest(row=row['name']):
                self.assertEqual(sdk.RUNTIMES[row['target']][0], self.pins[row['package']]['platform'])
        self.assertEqual({row['package'].rsplit('-', 1)[0] for row in self.rows},
                         {'6.1.1-standard', '6.3.2-standard', '6.3.2-advanced', 'current-standard', 'current-advanced'})

    def test_a_row_installs_only_a_driver_built_for_its_runtime(self):
        rows = [row for row in self.rows if 'hx-driver' in row]
        self.assertEqual([row['name'] for row in rows], ['linux-x64 advanced-6.4'])
        for row in rows:
            driver, package = self.pins[row['hx-driver']], self.pins[row['package']]
            self.assertEqual((driver['platform'], driver['version']), (package['platform'], package['version']))
            self.assertIn(row['target'], sdk.DRIVERS[driver['driver']].directories)
        provision = next(step for step in self.job['steps'] if step.get('name') == 'Provision the SDK')['run']
        self.assertIn('if [ -n "${{ matrix.hx-driver }}" ]; then\n  python ci/sdk.py install-driver '
                      '"${{ matrix.hx-driver }}" --sdk "$RUNNER_TEMP/ndi-sdk" --target "$TARGET"\nfi', provision)

    def test_each_step_promises_exactly_what_its_build_can_check(self):
        """A promise no test in the build can check fails that build; a missing one lets tests skip."""
        for row in self.rows:
            pin, target, driver = self.pins[row['package']], row['target'], self.pins.get(row.get('hx-driver'))
            for step, promise, all_features in (('all features', row['require'], True),
                                                ('no default features', row.get('require-no-default', '1'), False)):
                with self.subTest(row=row['name'], step=step):
                    # `1` is implied by every other entry, so it is spelt out only when it stands alone.
                    self.assertEqual(set(promise.split(',')) | {'1'},
                                     checkable(target, pin, all_features=all_features, driver=driver))
        # A step that may fail without failing the job is a diagnostic, and promises nothing.
        tests = {step['run']: step['env'] for step in self.job['steps']
                 if step.get('run', '').startswith('cargo test') and 'continue-on-error' not in step}
        command = 'cargo test --locked --config ci/offline.toml --target "$TARGET" --no-fail-fast '
        self.assertEqual(tests, {
            command + '--all-features': {'GRAFTON_NDI_REQUIRE_RUNTIME': '${{ matrix.require }}'},
            command + '--no-default-features': {
                'GRAFTON_NDI_REQUIRE_RUNTIME': "${{ matrix.require-no-default || '1' }}"}})

    def test_a_failing_step_does_not_hide_the_steps_after_it(self):
        """Every step after the isolation check runs whenever that check passed, so one run reports everything
        that is wrong with a row; none runs when the runner was not shown to isolate."""
        names = [step.get('name') for step in self.job['steps']]
        check = names.index('Test runner leaves loopback networking only')
        self.assertEqual(self.job['steps'][check].get('id'), 'isolation')
        self.assertNotIn('if', self.job['steps'][check])
        after = self.job['steps'][check + 1:]
        self.assertGreaterEqual(len(after), 7)
        for step in after:
            with self.subTest(step=step['name']):
                self.assertTrue(step.get('if', '').startswith('${{ ' + ROW_READY),
                                f"{step['name']} runs on {step.get('if')!r}")
        self.assertEqual({step['name']: step['if'] for step in after if 'matrix.' in step['if']}, {
            'Lint and document the Advanced implementation': when("contains(matrix.package, 'advanced')"),
            'Vendored bindings are current': when('matrix.bindings'),
            'Build for the statically linked Apple mobile targets': when('matrix.apple-mobile'),
            'Current Advanced headers with baseline runtimes': when('matrix.compatibility'),
            'Baseline SDK builds with the current Advanced runtime': when('matrix.compatibility')})

    def test_every_test_step_is_bounded_inside_the_job(self):
        """A test that never returns fails its own step, well before the job's limit, and a failing test binary
        does not keep the ones after it from running."""
        job = int(self.job['timeout-minutes'])
        steps = [step for step in self.job['steps'] if 'cargo test' in step.get('run', '')]
        self.assertGreaterEqual(len(steps), 4)
        for step in steps:
            with self.subTest(step=step['name']):
                self.assertRegex(step.get('timeout-minutes', ''), r'^\d+$')
                self.assertLess(int(step['timeout-minutes']), job)
                if 'continue-on-error' not in step:
                    self.assertIn('--no-fail-fast', step['run'])

    def test_compatibility_runs_use_independent_headers_and_runtime_identity(self):
        rows = [row for row in self.rows if 'compatibility' in row]
        self.assertEqual({self.pins[row['compatibility']]['platform'] for row in rows},
                         {'linux', 'windows', 'apple'})
        for row in rows:
            self.assertEqual(self.pins[row['compatibility']]['edition'], 'standard')
            self.assertEqual(self.pins[row['compatibility-advanced']]['edition'], 'advanced')
            self.assertEqual(self.pins[row['compatibility-advanced']]['platform'], self.pins[row['compatibility']]['platform'])
            self.assertLess(sdk.version_tuple(self.pins[row['compatibility-advanced']]['version']),
                            sdk.version_tuple(self.pins[row['package']]['version']))
            self.assertLess(sdk.version_tuple(self.pins[row['compatibility']]['version']),
                            sdk.version_tuple(self.pins[row['package']]['version']))
            self.assertEqual(self.pins[row['compatibility']]['platform'],
                             self.pins[row['package']]['platform'])
        steps = [step for step in self.job['steps'] if step.get('if') == when('matrix.compatibility')]
        self.assertEqual(len(steps), 2)
        for step in steps:
            self.assertNotIn('env', step)
            self.assertIn('export GRAFTON_NDI_REQUIRE_RUNTIME=', step['run'])
            self.assertIn('cargo test --locked --config ci/offline.toml --target "$TARGET" --no-fail-fast --all-features',
                          step['run'])
            self.assertIn('ndi-compat-$edition.env', step['run'])
            self.assertIn('for edition in standard advanced; do', step['run'])
        self.assertIn('export NDI_SDK_DIR="${{ steps.sdk.outputs.NDI_SDK_DIR }}"', steps[0]['run'])
        for name, source in [('GRAFTON_NDI_PEER_RUNTIME', 'GRAFTON_NDI_EXPECTED_RUNTIME'),
                             ('GRAFTON_NDI_PEER_VERSION', 'GRAFTON_NDI_EXPECTED_VERSION'),
                             ('GRAFTON_NDI_PEER_EDITION', 'GRAFTON_NDI_EXPECTED_EDITION')]:
            self.assertIn(f'export {name}="${{{{ steps.sdk.outputs.{source} }}}}"', steps[0]['run'])
            self.assertIn(f'export {name}="${source}"', steps[1]['run'])
        for name in ('NDI_RUNTIME_DIR_V6', 'GRAFTON_NDI_EXPECTED_VERSION',
                     'GRAFTON_NDI_EXPECTED_EDITION', 'GRAFTON_NDI_EXPECTED_RUNTIME'):
            self.assertIn(f'export {name}="${{{{ steps.sdk.outputs.{name} }}}}"', steps[1]['run'])

    def test_each_compatibility_pass_promises_exactly_what_it_can_check(self):
        """The first step builds with the row's headers against each baseline runtime, the second with each
        baseline's headers against the row's runtime; a Standard baseline has no headers a build reads. Only
        the row's own runtime has the row's driver beside it. Neither step promises an NDI|HX decoder: the
        row's own runs do, and the decode tests still run, and pass or skip visibly, in the crossed builds."""
        baseline_runtimes, baseline_headers = [step for step in self.job['steps']
                                               if step.get('if') == when('matrix.compatibility')]
        for row in (row for row in self.rows if 'compatibility' in row):
            current, target, driver = self.pins[row['package']], row['target'], self.pins.get(row.get('hx-driver'))
            baselines = {'standard': self.pins[row['compatibility']], 'advanced': self.pins[row['compatibility-advanced']]}
            with self.subTest(row=row['name'], step=baseline_runtimes['name']):
                self.assertEqual(exported_promises(baseline_runtimes, row['require']),
                                 {edition: checkable(target, pin, all_features=True, headers=current, decoders=False)
                                  for edition, pin in baselines.items()})
            with self.subTest(row=row['name'], step=baseline_headers['name']):
                self.assertEqual(exported_promises(baseline_headers, row['require']),
                                 {edition: checkable(target, current, all_features=True, headers=pin, driver=driver,
                                                     decoders=False)
                                  for edition, pin in baselines.items()})

    def test_readme_matrix_table_matches_the_workflow(self):
        readme = (ROOT / 'ci' / 'README.md').read_text(encoding='utf-8')
        documented = []
        for line in readme.splitlines():
            cells = [cell.strip().replace('`', '') for cell in line.strip('|').split('|')]
            if len(cells) == 7 and cells[3] in self.pins:
                documented.append(cells)
        expected = []
        for row in self.rows:
            extras = ['vendored bindings check'] if 'bindings' in row else []
            extras += ['Advanced lint'] if 'advanced' in row['package'] else []
            extras += ['Apple mobile builds'] if 'apple-mobile' in row else []
            expected.append([row['name'], row['runner'], row['target'], row['package'], row['require'],
                             row.get('require-no-default', '1'), ', '.join(extras)])
        self.assertEqual(documented, expected)


class Repository(unittest.TestCase):
    def test_semver_check_runs_where_no_secret_is(self):
        texts = read_workflows()
        lint = workflows.load(texts['rust.yml'])['jobs']['lint']
        self.assertIn('python ci/semver.py', [step.get('run') for step in lint['steps']])
        self.assertNotIn('semver-checks', ''.join(texts.values()))

    def test_workflows_name_only_features_the_crate_has(self):
        features = set(tomllib.loads((ROOT / 'Cargo.toml').read_text(encoding='utf-8'))['features']) - {'default'}
        named = set()
        for text in read_workflows().values():
            for match in re.findall(r'--features[ =]"?([\w,-]+)', text):
                named.update(match.split(','))
            for match in re.findall(r'for features in ([^;]+); do', text):
                named.update(part for group in match.split() for part in group.strip('"').split(',') if part)
        self.assertGreaterEqual(named, {'advanced', 'file-io'})
        self.assertLessEqual(named, features)

    def test_repository_cargo_configuration_holds_only_the_xtask_alias(self):
        """Cargo obeys this file in every job that starts in the checkout; it can run programs and redirect registries."""
        directory = ROOT / '.cargo'
        self.assertEqual(sorted(path.name for path in directory.iterdir()), ['config.toml'])
        self.assertEqual(tomllib.loads((directory / 'config.toml').read_text(encoding='utf-8')),
                         {'alias': {'xtask': 'run --package xtask --'}})

    def test_code_owners_cover_what_ci_obeys(self):
        lines = (ROOT / '.github' / 'CODEOWNERS').read_text(encoding='utf-8').splitlines()
        owned = {line.split()[0]: line.split()[1:] for line in lines if line.strip() and not line.startswith('#')}
        self.assertEqual(set(owned), {'/.github/', '/ci/', '/scripts/', '/.cargo/', '/rust-toolchain.toml',
                                      '/Cargo.toml', '/Cargo.lock'})
        self.assertEqual({owner for owners in owned.values() for owner in owners}, {'@GrantSparks'})


if __name__ == '__main__':
    unittest.main()
