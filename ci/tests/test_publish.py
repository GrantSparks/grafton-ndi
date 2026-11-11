"""SDK-free publication policy, staging and exact archive privacy regressions."""
import copy
import io
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import publish
from common import digest, plan_digest
from aggregate import validate
from evidence_fixture import catalog_with_unselected_package, evidence_fixture


class PublicationTests(unittest.TestCase):
    def environment(self, ref='refs/heads/main'):
        return {'GITHUB_ACTIONS': 'true', 'NDI_CI_RELEASED': 'true',
                'NDI_REQUIRED_CI_PASSED': 'true', 'GITHUB_EVENT_NAME': 'push',
                'GITHUB_REPOSITORY': 'GrantSparks/grafton-ndi', 'GITHUB_REF': ref}

    def test_release_requires_clearance_complete_gate_and_trusted_ref(self):
        publish.release_guard(self.environment(), '2.0.1', kind='docs')
        for field, value in [('GITHUB_ACTIONS', 'false'), ('NDI_CI_RELEASED', ''),
                             ('NDI_REQUIRED_CI_PASSED', 'false'), ('GITHUB_EVENT_NAME', 'pull_request'),
                             ('GITHUB_REPOSITORY', 'someone/fork'), ('GITHUB_REF', 'refs/heads/topic')]:
            environment = self.environment(); environment[field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                publish.release_guard(environment, '2.0.1', kind='docs')
        for ref in ('refs/tags/2.0.1', 'refs/tags/v2.0.1'):
            publish.release_guard(self.environment(ref), '2.0.1', kind='crate')
        for ref in ('refs/tags/v2.0.2', 'refs/heads/main', 'refs/tags/release'):
            with self.subTest(ref=ref), self.assertRaises(ValueError):
                publish.release_guard(self.environment(ref), '2.0.1', kind='crate')

    def test_evidence_binds_source_suite_dependencies_complete_matrix_and_sdk(self):
        with tempfile.TemporaryDirectory() as directory:
            lock = Path(directory) / 'Cargo.lock'; lock.write_text('tested dependencies')
            plan, records, available = evidence_fixture()
            plan['lockfile_sha256'] = digest(lock)
            for record in [*records, *available]:
                record.update(lockfile_sha256=digest(lock), plan_sha256=plan_digest(plan))
            report = validate(plan, records, available)
            package = plan['packages'][publish.PACKAGE]
            identity = {field: plan[field] for field in ('commit', 'tree', 'dirty')}
            fingerprint = plan['suite_fingerprint']
            publish.validate_evidence(plan, report, lock, identity, fingerprint, package)
            for field, value in [('commit', 'stale'), ('tree', 'stale'), ('suite_fingerprint', 'e' * 64),
                                 ('lockfile_sha256', 'f' * 64), ('cells_passed', 0), ('status', 'skipped'),
                                 ('groups_passed', 0), ('packages_verified', 11),
                                 ('plan_sha256', '0' * 64), ('scope', 'extended')]:
                bad = copy.deepcopy(report); bad[field] = value
                with self.subTest(field=field), self.assertRaises(ValueError):
                    publish.validate_evidence(plan, bad, lock, identity, fingerprint, package)
            bad = copy.deepcopy(plan); bad['packages'][publish.PACKAGE]['artifact_id'] = 'wrong'
            with self.assertRaises(ValueError):
                publish.validate_evidence(bad, report, lock, identity, fingerprint, package)

    def test_extended_success_and_weakened_required_plan_cannot_publish(self):
        with tempfile.TemporaryDirectory() as directory:
            lock = Path(directory) / 'Cargo.lock'; lock.write_text('tested dependencies')
            plan, records, available = evidence_fixture('extended')
            plan['lockfile_sha256'] = digest(lock)
            for record in [*records, *available]:
                record.update(lockfile_sha256=digest(lock), plan_sha256=plan_digest(plan))
            report = validate(plan, records, available)
            identity = {field: plan[field] for field in ('commit', 'tree', 'dirty')}
            with self.assertRaisesRegex(ValueError, 'required scope'):
                publish.validate_evidence(plan, report, lock, identity, plan['suite_fingerprint'],
                                          plan['packages'][publish.PACKAGE])
            plan['scope'] = report['scope'] = 'required'
            report['plan_sha256'] = plan_digest(plan)
            with self.assertRaisesRegex(ValueError, 'canonical'):
                publish.validate_evidence(plan, report, lock, identity, plan['suite_fingerprint'],
                                          plan['packages'][publish.PACKAGE])

    def test_unselected_archive_does_not_add_a_publication_receipt_requirement(self):
        catalog = catalog_with_unselected_package()
        with tempfile.TemporaryDirectory() as directory:
            lock = Path(directory) / 'Cargo.lock'; lock.write_text('tested dependencies')
            plan, records, available = evidence_fixture(catalog=catalog)
            plan['lockfile_sha256'] = digest(lock)
            for record in [*records, *available]:
                record.update(lockfile_sha256=digest(lock), plan_sha256=plan_digest(plan))
            identity = {field: plan[field] for field in ('commit', 'tree', 'dirty')}
            with patch('aggregate.read_json', return_value=catalog):
                report = validate(plan, records, available)
                self.assertEqual((len(plan['packages']), report['packages_verified']), (13, 12))
                publish.validate_evidence(plan, report, lock, identity, plan['suite_fingerprint'],
                                          plan['packages'][publish.PACKAGE])
                report['packages_verified'] = 11
                with self.assertRaisesRegex(ValueError, 'availability evidence'):
                    publish.validate_evidence(plan, report, lock, identity, plan['suite_fingerprint'],
                                              plan['packages'][publish.PACKAGE])

    def test_private_package_material_rejected_and_helper_itself_is_publishable(self):
        allowed = Path(publish.__file__).read_bytes()
        publish.validate_package_member('crate/ci/publish.py', allowed)
        publish.validate_package_member('crate/src/ndi_lib.rs', b'include!(concat!(env!("OUT_DIR"), "/ndi_lib.rs"));')
        forbidden = [('crate/target/debug/out/ndi_lib.rs', b''),
                     ('crate/Processing.NDI.Lib.h', b''), ('crate/vendor/libndi.so.6', b''),
                     ('crate/receipt/provision.json', b'{}'), ('crate/source.rs', publish.BINDGEN_MARKER),
                     ('crate/data', b'\x7fELFbinary'), ('crate/source.rs', b'/workspace/' + b'private-audit/notes')]
        for name, data in forbidden:
            with self.subTest(name=name), self.assertRaises(ValueError):
                publish.validate_package_member(name, data)

    def test_archive_inspection_rejects_renamed_native_binary(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / 'test.crate'
            with tarfile.open(archive, 'w:gz') as stream:
                body = b'\x7fELFpayload'; member = tarfile.TarInfo('crate/data.txt'); member.size = len(body)
                stream.addfile(member, io.BytesIO(body))
            with self.assertRaises(ValueError): publish.inspect_package(archive)

    def test_staging_allows_empty_source_directory_but_no_source_files_or_links(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); source = root / 'doc'
            (source / 'grafton_ndi').mkdir(parents=True); (source / 'src/grafton_ndi').mkdir(parents=True)
            index = source / 'grafton_ndi/index.html'
            index.write_text('<a href="https://doc.rust-lang.org/src/core/lib.rs.html">std source</a>')
            (source / '.lock').write_text('')
            self.assertEqual(publish.stage_docs(source, root / 'public'), 2)
            self.assertFalse((root / 'public/src').exists())
            (source / 'src/grafton_ndi/lib.rs.html').write_text('private source')
            with self.assertRaises(ValueError): publish.stage_docs(source, root / 'bad-source')
            (source / 'src/grafton_ndi/lib.rs.html').unlink()
            index.write_text('<a href="../src/grafton_ndi/lib.rs.html">source</a>')
            with self.assertRaises(ValueError): publish.stage_docs(source, root / 'bad-link')


if __name__ == '__main__':
    unittest.main()
