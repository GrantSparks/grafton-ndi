import contextlib
import io
import json
import os
from pathlib import Path
import re
import sys
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import gate  # noqa: E402

GREEN = {'lint': 'success', 'test': 'success', 'cross': 'success', 'sdk': 'success'}
FORK = {**GREEN, 'sdk': 'skipped'}
WORKFLOW = Path(__file__).resolve().parents[2] / '.github' / 'workflows' / 'rust.yml'


class Gate(unittest.TestCase):
    def test_the_gate_needs_exactly_the_jobs_these_tests_assume(self):
        needs = re.search(r'\n  required-ci:\n(?:.*\n)*?    needs: \[([^\]]+)\]', WORKFLOW.read_text(encoding='utf-8'))
        self.assertEqual(sorted(needs.group(1).split(', ')), sorted(GREEN))
        self.assertIn(gate.SDK_JOB, GREEN)

    def test_same_repository_events_need_every_job(self):
        self.assertEqual(gate.failures(GREEN, True), [])
        for job in GREEN:
            for result in ('failure', 'cancelled', 'skipped'):
                with self.subTest(job=job, result=result):
                    self.assertEqual(gate.failures({**GREEN, job: result}, True),
                                     [f'{job}: {result} (required: success)'])

    def test_fork_pull_requests_pass_on_the_secret_free_jobs(self):
        self.assertEqual(gate.failures(FORK, False), [])
        self.assertEqual(gate.failures({**FORK, 'test': 'failure'}, False), ['test: failure (required: success)'])
        self.assertEqual(gate.failures({**FORK, 'lint': 'skipped'}, False), ['lint: skipped (required: success)'])

    def test_dependabot_runs_are_judged_like_fork_pull_requests(self):
        # Dependabot's runs are given no Actions secrets, although its branches are in this repository.
        # The workflow must therefore skip the sdk job for them and tell the gate not to expect it.
        text = WORKFLOW.read_text(encoding='utf-8')
        condition = re.search(r'\n  sdk:\n(?:.*\n)*?    if: >-\n((?:      .*\n)+)', text).group(1)
        expected = re.search(r'\n          SDK_EXPECTED: >-\n((?:            .*\n)+)', text).group(1)
        for expression in (condition, expected):
            self.assertIn("github.actor != 'dependabot[bot]' &&", expression)
        self.assertEqual(self.run_main(FORK, 'false'), (0, '', ''))
        self.assertEqual(self.run_main(GREEN, 'false')[0], 1)

    def test_the_sdk_job_never_runs_for_forks_and_never_skips_elsewhere(self):
        self.assertEqual(gate.failures(GREEN, False), ['sdk: success (required: skipped)'])
        self.assertEqual(gate.failures(FORK, True), ['sdk: skipped (required: success)'])

    def run_main(self, results, sdk_expected):
        environ = {'RESULTS': json.dumps({job: {'result': result, 'outputs': {}} for job, result in results.items()}),
                   'SDK_EXPECTED': sdk_expected}
        stdout, stderr = io.StringIO(), io.StringIO()
        with mock.patch.dict(os.environ, environ), contextlib.redirect_stdout(stdout), \
                contextlib.redirect_stderr(stderr):
            return gate.main(), stdout.getvalue(), stderr.getvalue()

    def test_main_reads_the_needs_context(self):
        self.assertEqual(self.run_main(GREEN, 'true'), (0, '', ''))
        self.assertEqual(self.run_main(FORK, 'false'), (0, '', ''))
        status, stdout, stderr = self.run_main(FORK, 'true')
        self.assertEqual((status, stdout), (1, '::error::sdk: skipped (required: success)\n'))
        self.assertIn('NDI_CI_RELEASED', stderr)
        status, stdout, stderr = self.run_main({**GREEN, 'sdk': 'failure'}, 'true')
        self.assertEqual((status, stdout, stderr), (1, '::error::sdk: failure (required: success)\n', ''))


if __name__ == '__main__':
    unittest.main()
