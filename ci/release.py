#!/usr/bin/env python3
"""Check that a release tag names the crate version and verify release controls.

    python ci/release.py "$GITHUB_REF_NAME"
    python ci/release.py --controls "$GITHUB_REPOSITORY"

The tag must be `v` followed by the `version` in Cargo.toml. The controls
check reads the GitHub API and uses GITHUB_TOKEN when it is set.
"""
import json
import os
from pathlib import Path
import re
import sys
import tomllib
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

MANIFEST = Path(__file__).resolve().parents[1] / 'Cargo.toml'


def crate_version(manifest=MANIFEST):
    return tomllib.loads(Path(manifest).read_text(encoding='utf-8'))['package']['version']


def mismatch(tag, version):
    """Return why `tag` does not release `version`, or None when it does."""
    if tag == 'v' + version:
        return None
    return f'tag {tag} does not release crate version {version}; expected v{version}'


def repository_get(repository, token):
    headers = {
        'Accept': 'application/vnd.github+json',
        'X-GitHub-Api-Version': '2022-11-28',
    }
    if token:
        headers['Authorization'] = f'Bearer {token}'

    def get(endpoint):
        url = f'https://api.github.com/repos/{repository}/{endpoint}'
        with urlopen(Request(url, headers=headers), timeout=15) as response:
            return json.load(response)

    return get


def environment_refusal(get):
    environment = get('environments/crates-io')
    reviewer_rules = environment.get('protection_rules', [])
    if not any(rule.get('type') == 'required_reviewers' and rule.get('reviewers')
               for rule in reviewer_rules):
        return 'crates-io must require reviewers'
    policy = environment.get('deployment_branch_policy')
    if not policy or policy.get('custom_branch_policies') is not True or policy.get('protected_branches') is not False:
        return 'crates-io must use custom deployment policies'
    policies = get('environments/crates-io/deployment-branch-policies?per_page=100')
    rules = policies['branch_policies']
    if (policies['total_count'] != 1 or len(rules) != 1
            or rules[0].get('name') != 'v*' or rules[0].get('type') != 'tag'):
        return 'crates-io deployments must be restricted to exactly the v* tag pattern'
    return None


def tag_ruleset_refusal(get):
    endpoint = 'rulesets?targets=tag&per_page=100'
    for page in range(1, sys.maxsize):
        summaries = get(f'{endpoint}&page={page}')
        if not isinstance(summaries, list):
            raise ValueError('expected a list of rulesets')
        for summary in summaries:
            if summary.get('target') != 'tag' or summary.get('enforcement') != 'active':
                continue
            ruleset_id = summary['id']
            if type(ruleset_id) is not int or ruleset_id < 1:
                raise ValueError('invalid ruleset ID')
            ruleset = get(f'rulesets/{ruleset_id}')
            refs = ruleset.get('conditions', {}).get('ref_name', {})
            types = {rule['type'] for rule in ruleset.get('rules', [])}
            if (ruleset.get('target') == 'tag' and ruleset.get('enforcement') == 'active'
                    and 'refs/tags/v*' in refs.get('include', []) and refs.get('exclude') == []
                    and {'creation', 'update', 'deletion'} <= types):
                return None
        if len(summaries) < 100:
            break
    return 'an active ruleset for refs/tags/v* must restrict creation, update and deletion'


def main_rules_refusal(get):
    rules = get('rules/branches/main')
    if not isinstance(rules, list):
        raise ValueError('expected a list of branch rules')
    review = any(rule['type'] == 'pull_request'
                 and rule.get('parameters', {}).get('require_code_owner_review') is True
                 for rule in rules)
    status = any(check['context'] == 'required-ci'
                 for rule in rules if rule['type'] == 'required_status_checks'
                 for check in rule.get('parameters', {}).get('required_status_checks', []))
    missing = []
    if not review:
        missing.append('pull requests with Code Owner review')
    if not status:
        missing.append('the required-ci status check')
    return 'main must require ' + ' and '.join(missing) if missing else None


def control_refusals(get):
    checks = (
        ('crates-io environment', environment_refusal),
        ('release tag ruleset', tag_ruleset_refusal),
        ('main branch rules', main_rules_refusal),
    )
    for number, (name, check) in enumerate(checks, 1):
        try:
            refusal = check(get)
        except HTTPError as error:
            refusal = f'GitHub API returned HTTP {error.code}'
        except (URLError, TimeoutError):
            refusal = 'could not read the GitHub API'
        except (AttributeError, KeyError, TypeError, ValueError):
            refusal = 'GitHub API returned invalid control data'
        if refusal:
            yield f'{name} (ci/README.md Release controls, item {number}): {refusal}'


def main(argv=None):
    arguments = sys.argv[1:] if argv is None else argv
    if len(arguments) == 2 and arguments[0] == '--controls':
        repository = arguments[1]
        if not re.fullmatch(r'[\w.-]+/[\w.-]+', repository, re.ASCII):
            print('expected an owner/repository argument for --controls', file=sys.stderr)
            return 2
        refusals = list(control_refusals(repository_get(repository, os.environ.get('GITHUB_TOKEN'))))
        for refusal in refusals:
            print(f'::error::{refusal}')
        return int(bool(refusals))
    if len(arguments) != 1:
        print(__doc__, file=sys.stderr)
        return 2
    problem = mismatch(arguments[0], crate_version())
    if problem:
        print(f'::error::{problem}')
    return 1 if problem else 0


if __name__ == '__main__':
    raise SystemExit(main())
