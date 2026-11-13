#!/usr/bin/env python3
"""Decide the `required-ci` result from the results of the jobs it needs.

    RESULTS='{"lint": {"result": "success"}, ...}' SDK_EXPECTED=true python ci/gate.py

RESULTS is the workflow's `toJSON(needs)`. SDK_EXPECTED is "true" for events
that can read the SDK secrets (everything except pull requests from forks).
There the `sdk` job must succeed; for fork pull requests it must be skipped.
Every other job must succeed in both cases.
"""
import json
import os
import sys

SDK_JOB = 'sdk'


def failures(results, sdk_expected):
    """Return one message per job whose result is not the one this event requires."""
    found = []
    for job, result in sorted(results.items()):
        wanted = 'success' if sdk_expected or job != SDK_JOB else 'skipped'
        if result != wanted:
            found.append(f'{job}: {result} (required: {wanted})')
    return found


def main():
    results = {job: need['result'] for job, need in json.loads(os.environ['RESULTS']).items()}
    sdk_expected = os.environ['SDK_EXPECTED'] == 'true'
    found = failures(results, sdk_expected)
    for message in found:
        print(f'::error::{message}')
    if sdk_expected and results.get(SDK_JOB) == 'skipped':
        print('The sdk job runs for same-repository events only while the repository variable '
              'NDI_CI_RELEASED is "true"; a skipped sdk job is not a pass.', file=sys.stderr)
    return 1 if found else 0


if __name__ == '__main__':
    raise SystemExit(main())
