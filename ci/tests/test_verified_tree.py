import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import verified_tree  # noqa: E402

MERGE, HEAD, TREE, TESTED, BASE = (item * 40 for item in 'abcde')
REPO = 17
RUN = 111
ARTIFACT = 12
PR = 55
ATTEMPT = 2


def archive(proof, *, name=verified_tree.PROOF):
    data = io.BytesIO()
    with zipfile.ZipFile(data, "w") as zipped:
        zipped.writestr(name, json.dumps(proof))
    return data.getvalue()


class Fixture:
    def __init__(self):
        self.pull = {
            "merge_commit_sha": MERGE, "merged_at": "2026-11-18T22:10:06Z", "state": "closed",
            "number": PR, "base": {"ref": "main", "repo": {"id": REPO}},
            "head": {"repo": {"id": REPO}, "sha": HEAD, "ref": "fix"},
        }
        self.run = {
            "status": "completed", "conclusion": "success", "event": "pull_request",
            "path": verified_tree.WORKFLOW, "head_sha": HEAD, "head_branch": "fix",
            "id": RUN, "run_attempt": ATTEMPT, "actor": {"login": "maintainer"},
            "head_repository": {"id": REPO}, "repository": {"id": REPO},
        }
        self.tested = {
            "sha": TESTED, "tree": {"sha": TREE}, "parents": [{"sha": BASE}, {"sha": HEAD}],
            "committer": {"name": "GitHub"}, "verification": {"verified": True},
        }
        self.proof = {
            "version": 1, "tree": TREE, "commit": TESTED, "repository_id": REPO, "pull": PR, "head": HEAD,
            "run_id": RUN, "attempt": ATTEMPT,
        }
        self.artifact = {"id": ARTIFACT, "name": "ci-tested-tree-2", "size_in_bytes": 200, "expired": False}
        self.jobs = [
            {"name": name, "status": "completed", "conclusion": "success"}
            for name in ("required-ci", "test (macos-15)", "sdk (linux)")
        ]
        self.archive_member = verified_tree.PROOF

    def json(self, path):
        return {
            f"commits/{MERGE}/pulls?per_page=20": [self.pull],
            f"git/commits/{MERGE}": {"parents": [{"sha": BASE}, {"sha": HEAD}]},
            f"git/commits/{TESTED}": self.tested,
            f"actions/workflows/rust.yml/runs?event=pull_request&head_sha={HEAD}&status=success&per_page=5":
                {"workflow_runs": [self.run]},
            f"actions/runs/{RUN}/artifacts?name=ci-tested-tree-{ATTEMPT}&per_page=10":
                {"artifacts": [self.artifact]},
            f"actions/runs/{RUN}/attempts/{ATTEMPT}/jobs?per_page=100": {"jobs": self.jobs},
        }[path]

    def read(self, path):
        assert path == f"actions/artifacts/{ARTIFACT}/zip"
        return archive(self.proof, name=self.archive_member)

    def match(self):
        return verified_tree.matching_run(self, MERGE, TREE, REPO)


class VerifiedTree(unittest.TestCase):
    def test_reuses_the_exact_tree_of_a_successful_run(self):
        self.assertEqual(Fixture().match(), RUN)

    def test_reuses_a_pull_request_run_that_skipped_publication(self):
        api = Fixture()
        api.jobs.append({"name": "publish release", "status": "completed", "conclusion": "skipped"})
        self.assertEqual(api.match(), RUN)

    def test_rejects_other_pull_requests_trees_and_attempts(self):
        for field, wrong in {"tree": "0" * 40, "pull": 56, "attempt": 1, "repository_id": 7,
                             "run_id": 42, "head": MERGE}.items():
            with self.subTest(field=field):
                api = Fixture()
                api.proof[field] = wrong
                self.assertIsNone(api.match())

    def test_rejects_other_run_provenance(self):
        for field, wrong in {"path": ".github/workflows/other.yml", "event": "workflow_dispatch",
                             "conclusion": "failure", "head_branch": "other",
                             "actor": {"login": "dependabot[bot]"}, "head_repository": {"id": 23}}.items():
            with self.subTest(field=field):
                api = Fixture()
                api.run[field] = wrong
                self.assertIsNone(api.match())
        api = Fixture()
        api.pull["base"]["ref"] = "maintenance"
        self.assertIsNone(api.match())

    def test_requires_the_signed_merge_commit_and_its_parents(self):
        for field, wrong in {"tree": {"sha": HEAD}, "verification": {"verified": False},
                             "parents": [{"sha": MERGE}, {"sha": HEAD}]}.items():
            with self.subTest(field=field):
                api = Fixture()
                api.tested[field] = wrong
                self.assertIsNone(api.match())

    def test_requires_the_gate_and_native_jobs_to_have_succeeded(self):
        for index in range(3):
            api = Fixture()
            api.jobs[index]["conclusion"] = "skipped"
            self.assertIsNone(api.match())
        api = Fixture()
        api.jobs.pop()
        self.assertIsNone(api.match())

    def test_rejects_missing_expired_large_or_unexpected_artifacts(self):
        for field, value in (("name", "ci-tested-tree-1"), ("expired", True), ("size_in_bytes", 4097)):
            api = Fixture()
            api.artifact[field] = value
            self.assertIsNone(api.match())
        api = Fixture()
        api.archive_member = "../" + verified_tree.PROOF
        self.assertIsNone(api.match())

    def test_api_failure_leaves_the_full_run_selected(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "outputs"
            env = {"GITHUB_SHA": MERGE, "GITHUB_REPOSITORY": "owner/repo", "GITHUB_REPOSITORY_ID": str(REPO),
                   "GITHUB_OUTPUT": str(output)}
            with patch.object(verified_tree, "checked_out_tree", return_value=TREE), \
                    patch.object(verified_tree.GitHub, "read", side_effect=subprocess.CalledProcessError(1, "gh")), \
                    patch("builtins.print"):
                verified_tree.reuse(env)
            self.assertFalse(output.exists())
