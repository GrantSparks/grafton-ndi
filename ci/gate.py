#!/usr/bin/env python3
"""Decide the `required-ci` result from the results of the jobs it needs.

    RESULTS='{"lint": {"result": "success"}, ...}' SDK_EXPECTED=true python ci/gate.py

RESULTS is the workflow's `toJSON(needs)`. SDK_EXPECTED is "true" for events
that can read the SDK secrets (everything except pull requests from forks).
A main push can replace the runner-heavy jobs with a verified full PR run.
Lint still validates the checkout and builds the Pages artifact.
"""
import json
import os
import re
import sys

SDK_JOB = 'sdk'
LOOKUP_JOB = 'verified-tree'
CHECKS = {'lint', 'test', 'cross', 'hx-pictures', SDK_JOB, 'miri'}


def failures(results, sdk_expected, *, reused=False):
    """Return one message per job whose result is not the one this event requires."""
    found = []
    for job in sorted(CHECKS):
        result = results.get(job, 'missing')
        skip = (reused and job != 'lint') or (not sdk_expected and job == SDK_JOB)
        wanted = 'skipped' if skip else 'success'
        if result != wanted:
            found.append(f'{job}: {result} (required: {wanted})')
    return found


def main():
    needs = json.loads(os.environ['RESULTS'])
    results = {job: need['result'] for job, need in needs.items()}
    sdk_expected = os.environ['SDK_EXPECTED'] == 'true'
    lookup = needs.get(LOOKUP_JOB, {})
    run_id = lookup.get('outputs', {}).get('run_id') or ''
    reused = (os.environ.get('REUSE_ALLOWED') == 'true' and sdk_expected
              and lookup.get('result') == 'success' and re.fullmatch(r'[1-9][0-9]*', run_id) is not None)
    found = failures(results, sdk_expected, reused=reused)
    if run_id and not reused:
        found.append('invalid workflow result for full PR CI reuse')
    if lookup.get('result') not in ('success', 'skipped'):
        found.append('verified-tree did not complete successfully')
    for message in found:
        print(f'::error::{message}')
    if reused and not found:
        print(f'Full native and platform checks passed in Actions run {run_id}.')
    if sdk_expected and not reused and results.get(SDK_JOB) == 'skipped':
        print('The sdk job runs for same-repository events only while the repository variable '
              'NDI_CI_RELEASED is "true"; a skipped sdk job is not a pass.', file=sys.stderr)
    return 1 if found else 0


if __name__ == '__main__':
    raise SystemExit(main())
