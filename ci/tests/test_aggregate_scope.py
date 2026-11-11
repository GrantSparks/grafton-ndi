"""Complete canonical plans, grouped provenance and availability cannot be weakened."""
from copy import deepcopy
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from aggregate import main, validate, validate_checkout, validate_plan
from common import digest, plan_digest
from evidence_fixture import catalog_with_unselected_package, evidence_fixture


class ScopeAggregateTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.required = evidence_fixture()

    def fixture(self):
        return deepcopy(self.required)

    def test_complete_required_and_extended_scopes_are_distinct(self):
        for scope, checks, groups in (('required', 52, 16), ('extended', 82, 82)):
            with self.subTest(scope=scope):
                plan, records, available = evidence_fixture(scope)
                report = validate(plan, records, available)
                self.assertEqual((report['scope'], report['cells_passed'], report['groups_passed'],
                                  report['packages_verified']), (scope, checks, groups, 12))
                self.assertEqual(report['plan_sha256'], plan_digest(plan))

    def test_retaining_an_unselected_sdk_does_not_expand_required_ci(self):
        catalog = catalog_with_unselected_package()
        plan, records, available = evidence_fixture(catalog=catalog)
        self.assertEqual(plan['cells'], self.required[0]['cells'])
        self.assertEqual(plan['groups'], self.required[0]['groups'])
        self.assertEqual(plan['availability'], self.required[0]['availability'])
        self.assertEqual(len(plan['packages']), 13)
        with patch('aggregate.read_json', return_value=catalog):
            report = validate(plan, records, available)
            self.assertEqual((report['cells_passed'], report['groups_passed'], report['packages_verified']),
                             (52, 16, 12))
            with self.assertRaisesRegex(ValueError, 'missing'):
                validate(plan, records, available[:-1])
            unexpected = {**available[0], 'package': '6.3.1-standard-linux'}
            with self.assertRaisesRegex(ValueError, 'unexpected'):
                validate(plan, records, [*available, unexpected])

    def test_self_consistent_omission_cannot_weaken_the_gate(self):
        plan, records, available = self.fixture()
        omitted = plan['cells'].pop()['id']
        for group in plan['groups']:
            if omitted in group['cells']:
                group['cells'].remove(omitted)
        records = [record for record in records if record['cell']['id'] != omitted]
        for record in [*records, *available]:
            record['plan_sha256'] = plan_digest(plan)
        with self.assertRaisesRegex(ValueError, 'canonical'):
            validate(plan, records, available)

    def test_scope_and_group_policy_are_recomputed(self):
        mutations = [lambda p: p.update(scope='partial'), lambda p: p.update(scope='extended'),
                     lambda p: p['groups'].pop(), lambda p: p['groups'][0]['cells'].pop(),
                     lambda p: p['cells'].append(deepcopy(p['cells'][0])),
                     lambda p: p['groups'][0].update(runner='unreviewed'),
                     lambda p: p['groups'][0]['availability_packages'].clear(),
                     lambda p: p['packages']['current-standard-linux'].update(sha256='0' * 64)]
        for index, mutate in enumerate(mutations):
            plan, _, _ = self.fixture()
            mutate(plan)
            with self.subTest(mutation=index), self.assertRaises(ValueError):
                validate_plan(plan)

    def test_omitted_duplicate_failed_or_unbound_result_is_rejected(self):
        plan, records, available = self.fixture()
        for changed in (records[:-1], [*records, records[0]],
                        [{**records[0], 'status': 'failed'}, *records[1:]]):
            with self.subTest(count=len(changed)), self.assertRaises(ValueError):
                validate(plan, changed, available)
        for field, value in (('scope', 'extended'), ('group', 'wrong'), ('plan_sha256', '0' * 64),
                             ('commit', 'a' * 40), ('tree', 'a' * 40), ('suite_fingerprint', 'a' * 64),
                             ('lockfile_sha256', 'a' * 64), ('dirty', True)):
            changed = deepcopy(records)
            changed[0][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                validate(plan, changed, available)

    def test_every_package_has_one_bound_designated_receipt(self):
        plan, records, available = self.fixture()
        for changed in (None, available[:-1], [*available, available[0]]):
            with self.subTest(count=None if changed is None else len(changed)), self.assertRaises(ValueError):
                validate(plan, records, changed)
        for field, value in (('scope', 'extended'), ('group', 'wrong'), ('plan_sha256', '0' * 64),
                             ('commit', 'a' * 40), ('lockfile_sha256', 'a' * 64), ('status', 'unavailable'),
                             ('archive_sha256', '0' * 64), ('inventory_sha256', 'a' * 64),
                             ('root', '/private/fixture')):
            changed = deepcopy(available)
            changed[0][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                validate(plan, records, changed)

    def test_plan_digest_ignores_json_formatting_but_binds_policy(self):
        plan, _, _ = self.fixture()
        self.assertEqual(plan_digest(plan), plan_digest(dict(reversed(list(plan.items())))))
        plan['scope'] = 'extended'
        self.assertNotEqual(plan_digest(plan), plan_digest(self.required[0]))

    def test_coherent_stale_artifacts_cannot_pass_on_a_new_checkout(self):
        plan, records, available = self.fixture()
        # A coherent old artifact set passes only its pure internal provenance
        # checks. The CLI must also compare it with its actual source checkout.
        self.assertEqual(validate(plan, records, available)['status'], 'passed')
        with tempfile.TemporaryDirectory() as directory:
            lock = Path(directory) / 'Cargo.lock'; lock.write_text('frozen resolution')
            plan['lockfile_sha256'] = digest(lock)
            identity = {field: plan[field] for field in ('commit', 'tree', 'dirty')}
            with patch('aggregate.git_identity', return_value=identity), \
                    patch('aggregate.suite_fingerprint', return_value=plan['suite_fingerprint']):
                validate_checkout(plan, lock)
                for field, value in (('commit', 'a' * 40), ('tree', 'a' * 40), ('dirty', True)):
                    with self.subTest(field=field), patch('aggregate.git_identity', return_value={**identity, field: value}), \
                            self.assertRaisesRegex(ValueError, 'checkout'):
                        validate_checkout(plan, lock)
                with patch('aggregate.suite_fingerprint', return_value='0' * 64), self.assertRaisesRegex(ValueError, 'suite'):
                    validate_checkout(plan, lock)
                lock.write_text('different resolution')
                with self.assertRaisesRegex(ValueError, 'lockfile'):
                    validate_checkout(plan, lock)

    def test_cli_rejects_coherent_old_artifacts_before_writing_success(self):
        plan, records, available = self.fixture()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            lock = root / 'Cargo.lock'; lock.write_text('frozen resolution')
            plan['lockfile_sha256'] = digest(lock)
            for record in [*records, *available]:
                record.update(lockfile_sha256=digest(lock), plan_sha256=plan_digest(plan))
            frozen = root / 'ci-plan.json'; frozen.write_text(json.dumps(plan))
            for name, values, filename in (('results', records, 'result.json'),
                                            ('availability', available, 'availability.json')):
                for index, record in enumerate(values):
                    path = root / name / str(index); path.mkdir(parents=True)
                    (path / filename).write_text(json.dumps(record))
            output = root / 'aggregate.json'
            arguments = ['aggregate.py', '--plan', str(frozen), '--lockfile', str(lock),
                         '--results', str(root / 'results'), '--availability', str(root / 'availability'),
                         '--output', str(output)]
            current = {'commit': '0' * 40, 'tree': plan['tree'], 'dirty': False}
            with patch.object(sys, 'argv', arguments), patch('aggregate.git_identity', return_value=current), \
                    patch('aggregate.suite_fingerprint', return_value=plan['suite_fingerprint']), \
                    self.assertRaisesRegex(ValueError, 'checkout'):
                main()
            self.assertFalse(output.exists())


if __name__ == '__main__':
    unittest.main()
