import contextlib
import io
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
from urllib.error import HTTPError

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import release  # noqa: E402

# Versions here are fixtures. The crate's own version is only ever read from its
# manifest, so these tests do not change when the crate is released.
MANIFEST = '[workspace]\nmembers = ["xtask"]\n\n[package]\nname = "x"\nversion = "{version}"\n'
ENVIRONMENT = 'environments/crates-io'
DEPLOYMENTS = ENVIRONMENT + '/deployment-branch-policies?per_page=100'
RULESETS = 'rulesets?targets=tag&per_page=100&page=1'
TAG_RULES = 'rulesets/42'
MAIN_RULES = 'rules/branches/main'


def protected_repository():
    return {
        ENVIRONMENT: {
            'protection_rules': [{'type': 'required_reviewers', 'reviewers': [{'id': 7}]}],
            'deployment_branch_policy': {'custom_branch_policies': True, 'protected_branches': False},
        },
        DEPLOYMENTS: {'total_count': 1, 'branch_policies': [{'name': 'v*', 'type': 'tag'}]},
        RULESETS: [{'target': 'tag', 'enforcement': 'active', 'id': 42}],
        TAG_RULES: {
            'target': 'tag', 'enforcement': 'active',
            'conditions': {'ref_name': {'include': ['refs/tags/v*'], 'exclude': []}},
            'rules': [{'type': rule} for rule in ('creation', 'update', 'deletion')],
        },
        MAIN_RULES: [
            {'type': 'pull_request', 'parameters': {'require_code_owner_review': True}},
            {'type': 'required_status_checks', 'parameters': {
                'required_status_checks': [{'context': 'required-ci'}],
            }},
        ],
    }


class Release(unittest.TestCase):
    def test_tag_must_be_v_plus_the_crate_version(self):
        self.assertIsNone(release.mismatch('v7.8.9', '7.8.9'))
        for tag in ('7.8.9', 'v7.8', 'v7.8.9-rc.1', 'v7.8.10', 'v8.0.0', 'release-7.8.9', 'V7.8.9', ''):
            with self.subTest(tag=tag):
                self.assertIn('expected v7.8.9', release.mismatch(tag, '7.8.9'))
        self.assertIsNone(release.mismatch('v7.8.9-rc.1', '7.8.9-rc.1'))

    def test_version_is_read_from_the_package_table(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / 'Cargo.toml'
            manifest.write_text(MANIFEST.format(version='7.8.9'))
            self.assertEqual(release.crate_version(manifest), '7.8.9')

    def test_main_checks_the_tag_against_the_checked_in_manifest(self):
        version = release.crate_version()
        output = io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(release.main(['v' + version]), 0)
            self.assertEqual(release.main([version]), 1)
            self.assertEqual(release.main(['v' + version + '.1']), 1)
            self.assertEqual(release.main([]), 2)
        self.assertIn(f'::error::tag {version} does not release crate version {version}; expected v{version}',
                      output.getvalue())

    def test_all_readable_controls_are_required(self):
        self.assertEqual(list(release.control_refusals(protected_repository().__getitem__)), [])
        changes = (
            (ENVIRONMENT, ('protection_rules',), [], 'item 1', 'require reviewers'),
            (ENVIRONMENT, ('protection_rules', 0, 'reviewers'), [], 'item 1', 'require reviewers'),
            (ENVIRONMENT, ('protection_rules', 0, 'reviewers'), 'user', 'item 1', 'invalid control data'),
            (ENVIRONMENT, ('deployment_branch_policy',), None, 'item 1', 'custom deployment'),
            (DEPLOYMENTS, ('branch_policies', 0, 'type'), 'branch', 'item 1', 'exactly the v* tag'),
            (DEPLOYMENTS, ('total_count',), 2, 'item 1', 'exactly the v* tag'),
            (RULESETS, (), [], 'item 2', 'active ruleset'),
            (TAG_RULES, ('enforcement',), 'evaluate', 'item 2', 'active ruleset'),
            (TAG_RULES, ('rules',), [{'type': 'creation'}, {'type': 'update'}], 'item 2', 'deletion'),
            (TAG_RULES, ('conditions', 'ref_name', 'exclude'), ['refs/tags/v3*'], 'item 2', 'active ruleset'),
            (TAG_RULES, ('conditions', 'ref_name', 'include'), 'refs/tags/v*', 'item 2', 'invalid control data'),
            (MAIN_RULES, (0, 'parameters', 'require_code_owner_review'), False, 'item 3', 'Code Owner'),
            (MAIN_RULES, (1, 'parameters', 'required_status_checks'), [], 'item 3', 'required-ci'),
            (MAIN_RULES, (), {}, 'item 3', 'invalid control data'),
        )
        for endpoint, path, value, item, reason in changes:
            with self.subTest(endpoint=endpoint, path=path, value=value):
                data = protected_repository()
                container = data
                keys = (endpoint, *path)
                for key in keys[:-1]:
                    container = container[key]
                container[keys[-1]] = value
                refusal, = release.control_refusals(data.__getitem__)
                self.assertIn(item, refusal)
                self.assertIn(reason, refusal)

    def test_http_errors_refuse_each_control_and_main_reports_all_failures(self):
        data = {RULESETS: [], MAIN_RULES: []}

        def get(endpoint):
            if endpoint == ENVIRONMENT:
                raise HTTPError('https://api.github.com/', 404, 'Not found', {}, None)
            return data[endpoint]

        output = io.StringIO()
        with patch.object(release, 'repository_get', return_value=get), contextlib.redirect_stdout(output):
            self.assertEqual(release.main(['--controls', 'owner/repo']), 1)
        lines = output.getvalue().splitlines()
        self.assertEqual(len(lines), 3)
        for number, line in enumerate(lines, 1):
            self.assertTrue(line.startswith('::error::'))
            self.assertIn(f'item {number}', line)
        self.assertIn('HTTP 404', lines[0])

        def unavailable(endpoint):
            raise HTTPError('https://api.github.com/', 500, 'Unavailable', {}, None)

        refusals = list(release.control_refusals(unavailable))
        self.assertEqual(len(refusals), 3)
        self.assertTrue(all('HTTP 500' in refusal for refusal in refusals))


if __name__ == '__main__':
    unittest.main()
