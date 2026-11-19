#!/usr/bin/env python3
"""Record a tree after full PR CI and look it up for main and release checks."""

import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import zipfile

PROOF = "ci-tested-tree.json"
WORKFLOW = ".github/workflows/rust.yml"


def oid(value):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{40}", value) is not None


def positive_id(value):
    return type(value) is int and value > 0


def checked_out_tree(sha):
    if not oid(sha) or git("HEAD") != sha:
        raise ValueError("checkout is not the workflow commit")
    return git("HEAD^{tree}")


def git(revision):
    return subprocess.check_output(["git", "rev-parse", revision], text=True, timeout=5).strip()


def record(path, env):
    sha = env["GITHUB_SHA"]
    event = json.loads(Path(env["GITHUB_EVENT_PATH"]).read_text(encoding="utf-8"))
    pull = event["pull_request"]
    payload = {
        "version": 1,
        "tree": checked_out_tree(sha),
        "commit": sha,
        "repository_id": event["repository"]["id"],
        "pull": pull["number"],
        "head": pull["head"]["sha"],
        "run_id": int(env["GITHUB_RUN_ID"]),
        "attempt": int(env["GITHUB_RUN_ATTEMPT"]),
    }
    Path(path).write_text(json.dumps(payload) + "\n", encoding="utf-8")


class GitHub:
    def __init__(self, repository):
        if re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository) is None:
            raise ValueError("invalid repository")
        self.root = "repos/" + repository

    def read(self, path):
        return subprocess.check_output(
            ["gh", "api", f"{self.root}/{path}"], stderr=subprocess.DEVNULL, timeout=12,
        )

    def json(self, path):
        return json.loads(self.read(path))


def artifact_proof(api, run_id, attempt):
    name = f"ci-tested-tree-{attempt}"
    artifacts = api.json(f"actions/runs/{run_id}/artifacts?name={name}&per_page=10")["artifacts"]
    matches = [item for item in artifacts if item.get("name") == name and item.get("expired") is False]
    if len(matches) != 1:
        return None
    item = matches[0]
    size = item.get("size_in_bytes")
    if not positive_id(item.get("id")) or not positive_id(size) or size > 4096:
        return None
    data = api.read(f"actions/artifacts/{item['id']}/zip")
    if len(data) > 4096:
        return None
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        entries = archive.infolist()
        if len(entries) != 1 or entries[0].filename != PROOF or entries[0].file_size > 2048:
            return None
        return json.loads(archive.read(entries[0]))


def matching_run(api, sha, tree, repository_id):
    if not (oid(sha) and oid(tree) and positive_id(repository_id)):
        return None
    pulls = api.json(f"commits/{sha}/pulls?per_page=20")
    merged = [p for p in pulls if p.get("merge_commit_sha") == sha and p.get("merged_at")
              and p.get("state") == "closed" and p.get("base", {}).get("ref") == "main"
              and (p["base"].get("repo") or {}).get("id") == repository_id
              and (p.get("head", {}).get("repo") or {}).get("id") == repository_id]
    if len(merged) != 1:
        return None
    head = merged[0]["head"]["sha"]
    head_branch = merged[0]["head"]["ref"]
    pull = merged[0]["number"]
    if not (oid(head) and positive_id(pull)):
        return None
    parents = api.json(f"git/commits/{sha}")["parents"]
    if not parents or not oid(parents[0].get("sha")):
        return None
    base = parents[0]["sha"]
    runs = api.json(f"actions/workflows/rust.yml/runs?event=pull_request&head_sha={head}"
                    "&status=success&per_page=5")["workflow_runs"]
    for run in runs:
        run_id, attempt = run.get("id"), run.get("run_attempt")
        actor = (run.get("actor") or {}).get("login")
        if not (positive_id(run_id) and positive_id(attempt)
                and run.get("status") == "completed" and run.get("conclusion") == "success"
                and run.get("event") == "pull_request" and run.get("path") == WORKFLOW
                and run.get("head_sha") == head and run.get("head_branch") == head_branch
                and isinstance(actor, str) and actor and actor != "dependabot[bot]"
                and (run.get("repository") or {}).get("id") == repository_id
                and (run.get("head_repository") or {}).get("id") == repository_id):
            continue
        proof = artifact_proof(api, run_id, attempt)
        if (isinstance(proof, dict) and oid(proof.get("commit"))
                and proof == {"version": 1, "tree": tree, "commit": proof["commit"],
                              "repository_id": repository_id, "pull": pull, "head": head,
                              "run_id": run_id, "attempt": attempt}
                and github_merge_tree(api, proof["commit"], tree, base, head)
                and required_check_passed(api, run_id, attempt)):
            return run_id
    return None



def github_merge_tree(api, commit, tree, base, head):
    # GitHub signs the synthetic merge evaluated by a pull_request workflow.
    # Verify its tree and both parents independently of the artifact.
    merged = api.json(f"git/commits/{commit}")
    return (merged.get("sha") == commit and merged.get("tree", {}).get("sha") == tree
            and merged.get("committer", {}).get("name") == "GitHub"
            and merged.get("verification", {}).get("verified") is True
            and [parent.get("sha") for parent in merged.get("parents", [])] == [base, head])


def required_check_passed(api, run_id, attempt):
    jobs = api.json(f"actions/runs/{run_id}/attempts/{attempt}/jobs?per_page=100")["jobs"]
    gate = [job for job in jobs if job.get("name") == "required-ci"]
    checks = [job for job in jobs if job.get("name") not in
              ("Find full CI for the merged tree", "publish documentation")]
    return (len(gate) == 1 and any(job.get("name", "").startswith("sdk (") for job in checks)
            and all(job.get("status") == "completed" and job.get("conclusion") == "success"
                    for job in checks))


def find_run(env, commit=None):
    try:
        sha = commit if commit is not None else env["GITHUB_SHA"]
        if not oid(sha):
            raise ValueError("expected a commit SHA")
        tree = git(f"{sha}^{{tree}}") if commit is not None else checked_out_tree(sha)
        api = GitHub(env["GITHUB_REPOSITORY"])
        run = matching_run(api, sha, tree, int(env["GITHUB_REPOSITORY_ID"]))
    except (OSError, ValueError, KeyError, TypeError, AttributeError, subprocess.SubprocessError,
            zipfile.BadZipFile, RuntimeError):
        print("Could not verify a completed full PR run. Running the full CI matrix.")
        return None
    if run is None:
        print("No completed full PR run matches this tree. Running the full CI matrix.")
    else:
        print(f"Reusing full PR validation from Actions run {run}.")
    return run


def reuse(env):
    run = find_run(env)
    if run is None:
        return
    with open(env["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        print(f"run_id={run}", file=output)


if __name__ == "__main__":
    if sys.argv[1:] == ["reuse"]:
        reuse(os.environ)
    elif len(sys.argv) == 3 and sys.argv[1] == "record":
        record(sys.argv[2], os.environ)
    else:
        raise SystemExit("usage: verified_tree.py reuse | record FILE")
