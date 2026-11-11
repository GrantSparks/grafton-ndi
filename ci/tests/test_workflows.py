"""Guards on the workflow files that actionlint does not check."""
from pathlib import Path
import re
import sys
import tomllib
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import sdk  # noqa: E402

WORKFLOWS = Path(__file__).resolve().parents[2] / '.github' / 'workflows'
ACQUISITION_SECRETS = {'NDI_SDK_SOURCES_JSON', 'NDI_SDK_ARCHIVE_TOKEN'}


def jobs(text):
    """Split a workflow into {job id: job text}; jobs are the keys indented two spaces under `jobs:`."""
    body = text.split('\njobs:\n', 1)[1]
    starts = list(re.finditer(r'^  ([\w-]+):[ \t]*$', body, re.MULTILINE))
    return {match.group(1): body[match.start():end.start() if end else len(body)]
            for match, end in zip(starts, starts[1:] + [None])}


def matrix_rows(job):
    """The `include` rows of a job's matrix as dictionaries of their scalar keys."""
    matrix = job[job.index('        include:\n'):job.index('    runs-on:')]
    rows = []
    for block in re.split(r'^ {10}- ', matrix, flags=re.MULTILINE)[1:]:
        pairs = re.findall(r'^ *([\w-]+): (.+)$', block, re.MULTILINE)
        rows.append({key: value.strip("'") for key, value in pairs})
    return rows


def checkable(target, pin, *, all_features):
    """What tests can check in a build for `target` running against the package `pin`.

    This restates the crate's test policy: the Discovery Server tests are Linux
    only and need a 6.4 runtime; the Advanced tests need the `advanced` feature
    and an Advanced SDK; the File tests need the `file-io` feature and a File
    library the runtime can drive, which is an Advanced runtime everywhere
    except Windows, where the library is edition-neutral and ships for x64.
    """
    platform, advanced = pin['platform'], pin['edition'] == 'advanced'
    current = sdk.version_tuple(pin['version']) >= (6, 4, 0, 0)
    found = {'1'}
    if platform == 'linux' and current:
        found.add('discovery-server')
    if all_features and advanced:
        found.add('advanced')
    if all_features and current and (target == 'x86_64-pc-windows-msvc' if platform == 'windows' else advanced):
        found.add('file')
    return found


class Workflows(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.text = {path.name: path.read_text(encoding='utf-8') for path in sorted(WORKFLOWS.glob('*.yml'))}
        cls.jobs = {name: jobs(text) for name, text in cls.text.items()}

    def test_the_two_workflows_exist_with_the_gate_job(self):
        self.assertEqual(set(self.text), {'rust.yml', 'publish.yml'})
        self.assertIn('required-ci', self.jobs['rust.yml'])
        self.assertIn('name: required-ci\n', self.jobs['rust.yml']['required-ci'])

    def test_every_action_is_pinned_to_a_commit(self):
        for name, text in self.text.items():
            for reference in re.findall(r'^\s*(?:- )?uses:\s*(\S+)', text, re.MULTILINE):
                with self.subTest(workflow=name, uses=reference):
                    self.assertRegex(reference, r'^(\./\.github/workflows/[\w.-]+\.yml|[\w.-]+/[\w./-]+@[0-9a-f]{40})$')

    def test_every_job_has_a_timeout(self):
        for name, workflow in self.jobs.items():
            for job, text in workflow.items():
                if 'runs-on:' in text:
                    with self.subTest(workflow=name, job=job):
                        self.assertRegex(text, r'\n    timeout-minutes: \d+\n')

    def test_default_token_permission_is_read_only(self):
        for name, text in self.text.items():
            with self.subTest(workflow=name):
                self.assertIn('\npermissions:\n  contents: read\n', text)

    def test_secrets_reach_only_the_provisioning_step_and_the_reusable_call(self):
        for name, workflow in self.jobs.items():
            for job, text in workflow.items():
                with self.subTest(workflow=name, job=job):
                    used = set(re.findall(r'secrets\.(\w+)', text))
                    allowed = ACQUISITION_SECRETS if (name, job) in {('rust.yml', 'sdk'), ('publish.yml', 'ci')} else set()
                    self.assertEqual(used, allowed)
        steps = self.jobs['rust.yml']['sdk'].split('\n      - ')
        self.assertEqual([step.splitlines()[0] for step in steps if re.search(r'secrets\.\w', step)],
                         ['name: Provision the SDK'])

    def test_the_sdk_job_keeps_sdk_files_out_of_caches_and_artifacts(self):
        sdk_job = self.jobs['rust.yml']['sdk']
        for action in ('rust-cache', 'actions/cache', 'upload-artifact', 'download-artifact'):
            with self.subTest(action=action):
                self.assertNotIn(action, sdk_job)

    def test_matrix_rows_use_pinned_packages_for_their_target(self):
        pins = sdk.load_pins()
        rows = matrix_rows(self.jobs['rust.yml']['sdk'])
        self.assertGreaterEqual(len(rows), 9)
        for row in rows:
            with self.subTest(row=row['name']):
                self.assertEqual(sdk.RUNTIMES[row['target']][0], pins[row['package']]['platform'])
        self.assertEqual({row['package'].rsplit('-', 1)[0] for row in rows},
                         {'6.3.2-standard', 'current-standard', 'current-advanced'})

    def test_each_step_promises_exactly_what_its_build_can_check(self):
        """A promise no test in the build can check fails that build; a missing one lets tests skip."""
        pins = sdk.load_pins()
        sdk_job = self.jobs['rust.yml']['sdk']
        for row in matrix_rows(sdk_job):
            pin, target = pins[row['package']], row['target']
            for step, promise, all_features in (('all features', row['require'], True),
                                                ('no default features', row.get('require-no-default', '1'), False)):
                with self.subTest(row=row['name'], step=step):
                    # `1` is implied by every other entry, so it is spelt out only when it stands alone.
                    self.assertEqual(set(promise.split(',')) | {'1'}, checkable(target, pin, all_features=all_features))
        for flags, promise in (('--all-features', 'matrix.require'),
                               ('--no-default-features', "matrix.require-no-default || '1'")):
            with self.subTest(flags=flags):
                self.assertRegex(sdk_job, r'GRAFTON_NDI_REQUIRE_RUNTIME: \$\{\{ ' + re.escape(promise) + r' \}\}\n'
                                 r'        run: cargo test --locked --config ci/offline\.toml --target "\$TARGET" '
                                 + flags + r'\n')
        self.assertEqual(sdk_job.count('GRAFTON_NDI_REQUIRE_RUNTIME:'), 2)

    def test_readme_matrix_table_matches_the_workflow(self):
        pins = sdk.load_pins()
        readme = (WORKFLOWS.parents[1] / 'ci' / 'README.md').read_text(encoding='utf-8')
        documented = []
        for line in readme.splitlines():
            cells = [cell.strip().replace('`', '') for cell in line.strip('|').split('|')]
            if len(cells) == 7 and cells[3] in pins:
                documented.append(cells)
        expected = []
        for row in matrix_rows(self.jobs['rust.yml']['sdk']):
            extras = [label for key, label in (('semver', 'semver check'), ('bindings', 'vendored bindings check'))
                      if key in row]
            extras += ['Advanced lint'] if 'advanced' in row['package'] else []
            extras += ['Apple mobile builds'] if 'apple-mobile' in row else []
            expected.append([row['name'], row['runner'], row['target'], row['package'], row['require'],
                             row.get('require-no-default', '1'), ', '.join(extras)])
        self.assertEqual(documented, expected)

    def test_semver_check_takes_no_overrides(self):
        commands = re.findall(r'^\s*(cargo semver-checks.*)$', self.text['rust.yml'], re.MULTILINE)
        self.assertEqual(commands, ['cargo semver-checks'])

    def test_workflows_name_only_features_the_crate_has(self):
        manifest = (WORKFLOWS.parents[1] / 'Cargo.toml').read_text(encoding='utf-8')
        features = set(tomllib.loads(manifest)['features']) - {'default'}
        named = set()
        for text in self.text.values():
            for match in re.findall(r'--features[ =]"?([\w,-]+)', text):
                named.update(match.split(','))
            for match in re.findall(r'for features in ([^;]+); do', text):
                named.update(part for group in match.split() for part in group.strip('"').split(',') if part)
        self.assertGreaterEqual(named, {'advanced', 'file-io'})
        self.assertLessEqual(named, features)

    def test_only_the_publish_job_can_obtain_the_registry_identity(self):
        holders = [(name, job) for name, workflow in self.jobs.items()
                   for job, text in workflow.items() if 'id-token: write' in text]
        self.assertEqual(holders, [('publish.yml', 'publish')])
        publish = self.jobs['publish.yml']['publish']
        self.assertIn('\n    environment: crates-io\n', publish)
        for command in re.findall(r'cargo (?:\+\S+ )?(\w+)([^\n]*)', publish):
            with self.subTest(command=command):
                self.assertIn(command[0], ('package', 'publish'))
                self.assertIn('--no-verify', command[1])
                self.assertIn('--locked', command[1])

    def test_publish_runs_only_for_version_tags_after_ci(self):
        text = self.text['publish.yml']
        self.assertRegex(text, r"\non:\n  push:\n    tags: \['v\*'\]\n\n")
        self.assertNotIn('workflow_dispatch', text)
        needs = {job: re.search(r'\n    needs: (\S+)', body) for job, body in self.jobs['publish.yml'].items()}
        self.assertEqual({job: match and match.group(1) for job, match in needs.items()},
                         {'release': None, 'ci': 'release', 'verify': 'ci', 'publish': 'verify'})
        self.assertNotIn('tags:', self.text['rust.yml'])


if __name__ == '__main__':
    unittest.main()
