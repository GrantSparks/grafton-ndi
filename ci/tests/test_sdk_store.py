"""Offline SDK store tests with small independent installer fixtures, no vendor SDK."""
import io
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import sdk_store
import provision
from common import digest, read_json, write_json


def fixture_archive(root, name, version='6.4.0.2', edition='standard', target_platform='linux', machine=62, profile='x86_64-linux-gnu'):
    """Not an NDI runtime: just a minimal ELF identity plus independently authored C."""
    inner = io.BytesIO()
    core = 'libndi_advanced' if edition == 'advanced' else 'libndi'
    elf = bytearray(64); elf[:6] = b'\x7fELF\x02\x01'; elf[18:20] = machine.to_bytes(2, 'little')
    files = {'SDK/include/Processing.NDI.Lib.h': b'int independent_fixture(void);\n',
             'SDK/Version.txt': ('NDI independent fixture v'+version+'\n').encode(),
             'SDK/lib/'+profile+'/'+core+'.so.6.4.0': bytes(elf),
             'SDK/docs/fixture.txt': b'Independently authored fixture documentation.\n'}
    with tarfile.open(fileobj=inner, mode='w:gz') as archive:
        for filename, content in files.items():
            member = tarfile.TarInfo(filename); member.size = len(content); member.mode = 0o644
            archive.addfile(member, io.BytesIO(content))
    content = b'#!/bin/sh\nexit 99\n__NDI_ARCHIVE_BEGIN__\n' + inner.getvalue()
    path = root/name
    with tarfile.open(path, 'w:gz') as archive:
        member = tarfile.TarInfo('Install_NDI_Fixture.sh'); member.size = len(content)
        archive.addfile(member, io.BytesIO(content))
    package = {'version': version, 'edition': edition, 'platform': target_platform,
               'sha256': digest(path), 'archive_format': 'embedded-tar', 'version_evidence': 'version.txt'}
    package = provision.resolved_package('fixture', {'fixture': package})
    return path, package


class StoreTests(unittest.TestCase):
    def setUp(self):
        # These are independently authored fixtures, not hosted SDK acquisition.
        environment = mock.patch.dict(os.environ, {'GITHUB_ACTIONS': 'false'})
        environment.start(); self.addCleanup(environment.stop)
        self.temporary = tempfile.TemporaryDirectory(prefix='ndi-store-tests-')
        self.root = Path(self.temporary.name)
        self.store = self.root/'private store'
        self.archive, self.package = fixture_archive(self.root, 'fixture.tar.gz')
        self.catalog = {'current-standard-linux': self.package}
        self.key = 'current-standard-linux'

    def tearDown(self):
        self.temporary.cleanup()

    def populate(self):
        return sdk_store.populate(self.store, self.key, archive=self.archive, catalog=self.catalog)

    def helper(self, name):
        index = sdk_store.load_index(self.store)
        return self.store/index.get('tool_generation','tools')/name

    def candidate_helpers(self):
        candidate = self.root/'candidate-helpers'; candidate.mkdir()
        for name in sdk_store.HELPERS:
            shutil.copyfile(sdk_store.HERE/name,candidate/name)
        with (candidate/'common.py').open('a') as output:
            output.write('\n# Independent harmless candidate generation.\n')
        return candidate

    def assert_portable_verify(self):
        result = subprocess.run([sys.executable,str(self.store/'tools/sdk_store.py'),
                                 '--store',str(self.store),'verify'],capture_output=True,text=True)
        self.assertEqual(result.returncode,0,result.stdout+result.stderr)

    def test_offline_retention_keeps_exact_original_and_needs_no_extractor(self):
        # The bytes are deliberately not a Windows installer. Retention verifies
        # identity only, and must not attempt host installation/execution.
        binary = self.root/'reviewed.exe'; binary.write_bytes(b'Independent retained binary fixture')
        package = {'version':'6.4.0.2','edition':'standard','platform':'windows',
                   'archive_format':'windows-installer','version_evidence':'archive-sha256','sha256':digest(binary)}
        with mock.patch.object(provision, 'extract_sdk', side_effect=AssertionError('must not execute')):
            sdk_store.retain(self.store, 'windows', archive=binary, catalog={'windows':package})
        index = sdk_store.load_index(self.store)
        self.assertEqual(len(index['archives']), 1)
        self.assertEqual(index['artifacts'], {})
        self.assertEqual(sdk_store.verify(self.store, 'windows'), [])
        retained = next(iter(index['archives'].values()))
        self.assertEqual((self.store/retained['path']).read_bytes(), binary.read_bytes())

    def test_retained_archive_survives_source_deletion_and_store_relocation(self):
        sdk_store.retain(self.store, self.key, archive=self.archive, catalog=self.catalog)
        self.archive.unlink()
        sdk_store.populate(self.store, self.key, catalog=self.catalog)
        relocated = self.root/'relocated mounted store'
        shutil.move(self.store, relocated)
        before = {str(p.relative_to(relocated)): p.stat().st_mtime_ns for p in relocated.rglob('*')}
        result = subprocess.run([sys.executable,str(relocated/'tools/sdk_store.py'), '--store',str(relocated),'verify'],capture_output=True,text=True)
        self.assertEqual(result.returncode,0,result.stderr)
        after = {str(p.relative_to(relocated)): p.stat().st_mtime_ns for p in relocated.rglob('*')}
        self.assertEqual(before, after, 'portable verification wrote to the mounted store')
        for path in [relocated/'index.json', *relocated.glob('artifacts/*/provision.json')]:
            text = path.read_text()
            self.assertNotIn(str(self.root), text)
            self.assertNotIn('https://', text)
        self.assertFalse(list(relocated.rglob('__pycache__')))

    def test_wrong_archive_and_unverified_baseline_fail_closed(self):
        self.archive.write_bytes(b'not the reviewed archive')
        with self.assertRaisesRegex(ValueError, 'SHA256'):
            self.populate()
        self.assertFalse((self.store/'index.json').exists())
        unverified = dict(self.package, sha256=None, artifact_id=None)
        with self.assertRaisesRegex(ValueError, 'verified SHA256'):
            sdk_store.retain(self.store, self.key, archive=self.archive, catalog={self.key:unverified})

    def test_private_source_map_never_enters_retained_store(self):
        private = self.root/'private-sources.json'
        write_json(private, {'schema':1,'packages':{self.key:{'archive':str(self.archive)}}})
        sdk_store.populate(self.store, self.key, sources=private, catalog=self.catalog)
        for path in [self.store/'index.json',self.helper('sdk-packages.json')]:
            self.assertNotIn(str(self.archive),path.read_text())
            self.assertNotIn('private-sources',path.read_text())
        url_only = {'schema':1,'packages':{self.key:{'url_env':'PRIVATE_TEST_SOURCE'}}}
        with mock.patch.object(provision, 'download', side_effect=AssertionError('network forbidden')):
            with self.assertRaisesRegex(ValueError,'offline'):
                sdk_store.retain(self.root/'other-store', self.key, sources=url_only,catalog=self.catalog)

    def test_content_receipt_helpers_and_archive_tampering_each_fail(self):
        self.populate()
        index = sdk_store.load_index(self.store)
        entry = next(iter(index['artifacts'].values()))
        targets = [self.store/entry['root']/'include/Processing.NDI.Lib.h',
                   self.store/entry['root']/'docs/fixture.txt',
                   self.store/entry['directory']/'provision.json',
                   self.helper('common.py'),
                   self.store/next(iter(index['archives'].values()))['path']]
        for target in targets:
            original = target.read_bytes()
            target.write_bytes(original+b'changed')
            with self.subTest(target=target.name), self.assertRaises((ValueError,json.JSONDecodeError)):
                sdk_store.verify(self.store)
            target.write_bytes(original)
        self.assertEqual(len(sdk_store.verify(self.store)),1)

    def test_unreviewed_generation_file_blocks_bootstrap_before_loading_helpers(self):
        self.populate()
        extra = self.helper('common.py').parent/'unexpected.py'
        extra.write_text('raise RuntimeError("must not be imported")\n')
        with self.assertRaisesRegex(ValueError,'unreviewed files'):
            sdk_store.verify(self.store)
        result = subprocess.run([sys.executable,str(self.store/'tools/sdk_store.py'),
                                 '--store',str(self.store),'verify'],capture_output=True,text=True)
        self.assertEqual(result.returncode,1)
        self.assertIn('unreviewed files',result.stderr)

    def test_side_by_side_pairs_isolate_builds_and_disable_inherited_wrappers(self):
        self.populate()
        old, old_package = fixture_archive(self.root,'old.tar.gz','6.3.2')
        catalog = {**self.catalog, 'old-standard-linux': old_package}
        sdk_store.populate(self.store,'old-standard-linux',archive=old,catalog=catalog)
        unrelated = self.root/'unrelated-libs'; unrelated.mkdir()
        wrong = self.root/'old-ndi-runtime'; wrong.mkdir(); (wrong/'libndi.so.6').write_bytes(b'old')
        inherited = {'LD_LIBRARY_PATH':f'{wrong}:{unrelated}', 'LD_PRELOAD':'bad.so',
                     'NDI_SDK_DIR':str(wrong),'NDI_RUNTIME_DIR_V6':str(wrong),
                     'RUSTC_WRAPPER':'sccache','RUSTC_WORKSPACE_WRAPPER':'private-wrapper',
                     'CARGO_BUILD_RUSTC_WRAPPER':'sccache', 'GMS_CACHE_MODE':'remote', 'PATH':os.environ['PATH']}
        identity = ('x86_64-linux-gnu','x86_64-unknown-linux-gnu',62,2)
        with mock.patch.object(sdk_store,'native_linux_identity',return_value=identity):
            current = sdk_store.command_environment(self.store,self.key,self.key,build_root=self.root/'builds',environment=inherited)
            mixed = sdk_store.command_environment(self.store,self.key,'old-standard-linux',build_root=self.root/'builds',environment=inherited)
        self.assertNotEqual(current['CARGO_TARGET_DIR'],mixed['CARGO_TARGET_DIR'])
        self.assertNotEqual(current['NDI_RUNTIME_DIR_V6'],mixed['NDI_RUNTIME_DIR_V6'])
        self.assertEqual(current['NDI_SDK_DIR'],mixed['NDI_SDK_DIR'])
        self.assertEqual(mixed['LD_LIBRARY_PATH'].split(':')[0],mixed['NDI_RUNTIME_DIR_V6'])
        self.assertIn(str(unrelated),mixed['LD_LIBRARY_PATH'])
        self.assertNotIn(str(wrong),mixed['LD_LIBRARY_PATH'])
        for key in ('RUSTC_WRAPPER','RUSTC_WORKSPACE_WRAPPER','CARGO_BUILD_RUSTC_WRAPPER'):
            self.assertEqual(mixed[key],'')
        self.assertNotIn('LD_PRELOAD',mixed)
        self.assertEqual(mixed['GMS_CACHE_MODE'],'off')
        self.assertEqual(mixed['SCCACHE_DISABLE'],'1')
        self.assertEqual(stat.S_IMODE(Path(mixed['CARGO_TARGET_DIR']).stat().st_mode),0o700)

    def test_incompatible_edition_platform_and_elf_rejected_before_command(self):
        self.populate()
        advanced, advanced_package = fixture_archive(self.root,'advanced.tar.gz',edition='advanced')
        catalog={**self.catalog,'advanced':advanced_package}
        sdk_store.populate(self.store,'advanced',archive=advanced,catalog=catalog)
        with self.assertRaisesRegex(ValueError,'same edition'):
            sdk_store.command_environment(self.store,self.key,'advanced',build_root=self.root/'builds')
        android, android_package = fixture_archive(self.root,'android.tar.gz',target_platform='android')
        catalog={**catalog,'android':android_package}
        sdk_store.populate(self.store,'android',archive=android,catalog=catalog)
        with self.assertRaisesRegex(ValueError,'Linux headers/runtime'):
            sdk_store.command_environment(self.store,'android','android',build_root=self.root/'builds')
        with mock.patch.object(sdk_store,'native_linux_identity',return_value=('x86_64-linux-gnu','x86_64-unknown-linux-gnu',183,2)):
            with self.assertRaisesRegex(ValueError,'ELF identity'):
                sdk_store.command_environment(self.store,self.key,self.key,build_root=self.root/'builds')

    def test_exec_verifies_only_selected_pair_while_full_verify_detects_unselected_tamper(self):
        self.populate()
        old, package = fixture_archive(self.root,'unused.tar.gz','6.3.2')
        sdk_store.populate(self.store,'unused',archive=old,catalog={**self.catalog,'unused':package})
        index = sdk_store.load_index(self.store)
        unused = index['artifacts'][index['packages']['unused']]
        unused_archive = self.store/index['archives'][index['packages']['unused']]['path']
        # Both original archive and installed payload are corrupt, but neither is
        # activated by this command. Full-store verification must still reject.
        unused_archive.write_bytes(b'unselected archive changed')
        (self.store/unused['root']/'include/Processing.NDI.Lib.h').write_bytes(b'unselected header changed')
        identity = ('x86_64-linux-gnu','x86_64-unknown-linux-gnu',62,2)
        with mock.patch.object(sdk_store,'native_linux_identity',return_value=identity), \
             mock.patch.object(sdk_store,'verify_archive',wraps=sdk_store.verify_archive) as archives, \
             mock.patch.object(sdk_store,'verify_artifact',wraps=sdk_store.verify_artifact) as payloads:
            env = sdk_store.command_environment(self.store,self.key,self.key,build_root=self.root/'builds',environment={})
        selected = index['packages'][self.key]
        self.assertEqual([call.args[1] for call in archives.call_args_list],[selected])
        self.assertEqual([call.args[1] for call in payloads.call_args_list],[selected])
        self.assertEqual(env['NDI_SDK_DIR'],str(self.store/index['artifacts'][selected]['root']))
        with self.assertRaisesRegex(ValueError,'retained archive changed'):
            sdk_store.verify(self.store)
        with self.assertRaisesRegex(ValueError,'retained archive changed'):
            sdk_store.command_environment(self.store,self.key,'unused',build_root=self.root/'builds',environment={})

    def test_selected_archive_payload_receipt_and_shared_catalog_tamper_still_block_exec(self):
        self.populate()
        index = sdk_store.load_index(self.store)
        selected = index['packages'][self.key]
        entry = index['artifacts'][selected]
        targets = [self.store/index['archives'][selected]['path'],
                   self.store/entry['root']/'include/Processing.NDI.Lib.h',
                   self.store/entry['directory']/'provision.json',
                   self.helper('sdk-packages.json')]
        for target in targets:
            original = target.read_bytes(); target.write_bytes(original+b'changed')
            with self.subTest(target=target.name), self.assertRaises(ValueError):
                sdk_store.command_environment(self.store,self.key,self.key,build_root=self.root/'builds',environment={})
            target.write_bytes(original)
        index['packages']['broken-alias'] = 'not-retained'
        write_json(self.store/'index.json',index)
        with self.assertRaisesRegex(ValueError,'alias references a missing artifact'):
            sdk_store.command_environment(self.store,self.key,self.key,build_root=self.root/'builds',environment={})

    def test_interrupted_generation_copy_keeps_active_store_and_retry_usable(self):
        self.populate()
        previous = (self.store/'index.json').read_bytes()
        launcher = (self.store/'tools/sdk_store.py').read_bytes()
        candidate = self.candidate_helpers()
        real_copy = shutil.copyfile
        def interrupted_copy(source,destination,*args,**kwargs):
            if Path(source) == candidate/'provision.py':
                raise OSError('synthetic third-helper copy failure')
            return real_copy(source,destination,*args,**kwargs)
        with mock.patch.object(sdk_store,'HERE',candidate), mock.patch.object(shutil,'copyfile',side_effect=interrupted_copy):
            with self.assertRaisesRegex(OSError,'third-helper'):
                sdk_store.retain(self.store,self.key,catalog=self.catalog)
        self.assertEqual((self.store/'index.json').read_bytes(),previous)
        self.assertEqual((self.store/'tools/sdk_store.py').read_bytes(),launcher)
        sdk_store.verify(self.store); self.assert_portable_verify()
        with mock.patch.object(sdk_store,'HERE',candidate):
            sdk_store.retain(self.store,self.key,catalog=self.catalog)
        self.assertNotEqual((self.store/'index.json').read_bytes(),previous)
        self.assertEqual((self.store/'tools/sdk_store.py').read_bytes(),launcher)
        self.assert_portable_verify()

    def test_failed_index_publication_keeps_old_generation_and_retry_usable(self):
        self.populate()
        previous = (self.store/'index.json').read_bytes()
        candidate = self.candidate_helpers()
        real_replace = Path.replace
        def interrupted_publish(path,target):
            if Path(target) == self.store/'index.json':
                raise OSError('synthetic index publication failure')
            return real_replace(path,target)
        with mock.patch.object(sdk_store,'HERE',candidate), mock.patch.object(Path,'replace',interrupted_publish):
            with self.assertRaisesRegex(OSError,'index publication'):
                sdk_store.retain(self.store,self.key,catalog=self.catalog)
        self.assertEqual((self.store/'index.json').read_bytes(),previous)
        sdk_store.verify(self.store); self.assert_portable_verify()
        with mock.patch.object(sdk_store,'HERE',candidate):
            sdk_store.retain(self.store,self.key,catalog=self.catalog)
        self.assertNotEqual((self.store/'index.json').read_bytes(),previous)
        self.assert_portable_verify()

    def test_interrupted_legacy_handoff_preserves_read_only_verification_and_retry(self):
        self.populate()
        index = sdk_store.load_index(self.store)
        generation = self.store/index.pop('tool_generation')
        index.pop('bootstrap_sha256')
        # Reconstruct the original flat-helper layout with its original index.
        for name in sdk_store.HELPERS:
            shutil.copyfile(generation/name,self.store/'tools'/name)
        write_json(self.store/'index.json',index)
        previous = (self.store/'index.json').read_bytes()
        self.assert_portable_verify()
        candidate = self.candidate_helpers()
        real_replace = Path.replace
        def interrupted_publish(path,target):
            if Path(target) == self.store/'index.json':
                raise OSError('synthetic legacy index publication failure')
            return real_replace(path,target)
        with mock.patch.object(sdk_store,'HERE',candidate), mock.patch.object(Path,'replace',interrupted_publish):
            with self.assertRaisesRegex(OSError,'legacy index'):
                sdk_store.retain(self.store,self.key,catalog=self.catalog)
        self.assertEqual((self.store/'index.json').read_bytes(),previous)
        self.assertEqual((self.store/'tools/sdk_store.py').read_text(),sdk_store.BOOTSTRAP)
        sdk_store.verify(self.store); self.assert_portable_verify()
        with mock.patch.object(sdk_store,'HERE',candidate):
            sdk_store.retain(self.store,self.key,catalog=self.catalog)
        self.assertIn('tool_generation',sdk_store.load_index(self.store))
        self.assert_portable_verify()

    def test_interrupted_new_install_retry_adopts_only_exact_reverified_payload(self):
        for tamper in (False, True):
            with self.subTest(tamper=tamper):
                store = self.root/('tampered-orphan' if tamper else 'complete-orphan')
                real_save = sdk_store.save_index
                calls = []
                def interrupted_save(*args,**kwargs):
                    calls.append(True)
                    if len(calls) == 2:
                        raise OSError('synthetic new-install index failure')
                    return real_save(*args,**kwargs)
                with mock.patch.object(sdk_store,'save_index',side_effect=interrupted_save):
                    with self.assertRaisesRegex(OSError,'new-install index'):
                        sdk_store.populate(store,self.key,archive=self.archive,catalog=self.catalog)
                self.assertEqual(sdk_store.verify(store),[])
                self.assertEqual(sdk_store.load_index(store)['artifacts'],{})
                orphan = store/'artifacts'/self.package['artifact_id']
                self.assertTrue(orphan.is_dir())
                header = next(orphan.rglob('Processing.NDI.Lib.h'))
                if tamper:
                    header.write_bytes(b'unreviewed orphan content')
                    with self.assertRaisesRegex(ValueError,'content changed'):
                        sdk_store.populate(store,self.key,catalog=self.catalog)
                    self.assertEqual(header.read_bytes(),b'unreviewed orphan content')
                    self.assertEqual(sdk_store.load_index(store)['artifacts'],{})
                else:
                    sdk_store.populate(store,self.key,catalog=self.catalog)
                    self.assertEqual(len(sdk_store.verify(store)),1)

    def test_aarch64_uses_reviewed_native_profile_without_bsp_fallback(self):
        import common
        canonical = common.profiles()['aarch64-rpi4-linux-gnueabi']
        arm, package = fixture_archive(self.root,'arm.tar.gz',machine=183,profile=canonical['subdir'])
        sdk_store.populate(self.store,self.key,archive=arm,catalog={self.key:package})
        for spelling in ('aarch64','arm64'):
            with self.subTest(spelling=spelling), mock.patch.object(sdk_store.sys,'platform','linux'), \
                 mock.patch.object(sdk_store.platform,'machine',return_value=spelling):
                identity = sdk_store.native_linux_identity()
                self.assertEqual(identity[:2],(canonical['id'],canonical['target']))
                env = sdk_store.command_environment(self.store,self.key,self.key,build_root=self.root/'builds',environment={})
                self.assertEqual(env['NDI_SDK_PROFILE'],canonical['id'])
                self.assertEqual(Path(env['NDI_RUNTIME_DIR_V6']).name,canonical['subdir'])
        # A different vendor BSP directory must not be silently substituted for
        # the reviewed native profile, even if its ELF architecture is identical.
        bsp, bsp_package = fixture_archive(self.root,'bsp.tar.gz',machine=183,profile='aarch64-rockchip-linux-gnu')
        sdk_store.populate(self.root/'bsp-store',self.key,archive=bsp,catalog={self.key:bsp_package})
        with mock.patch.object(sdk_store.sys,'platform','linux'), mock.patch.object(sdk_store.platform,'machine',return_value='aarch64'):
            with self.assertRaisesRegex(ValueError,'no native runtime for aarch64-rpi4-linux-gnueabi'):
                sdk_store.command_environment(self.root/'bsp-store',self.key,self.key,build_root=self.root/'builds',environment={})

    @unittest.skipUnless(sys.platform == 'linux' and sdk_store.platform.machine().lower() in ('x86_64','amd64'), 'fixture ELF is Linux x64')
    def test_exec_child_does_not_receive_custom_source_tokens_or_urls(self):
        self.populate()
        sources = {'schema':1,'packages':{
            'asset-source':{'github_release_asset':{'repo':'fixture/private-archive','asset_id':123,'token_env':'CUSTOM_ARCHIVE_READ_TOKEN'}},
            'url-source':{'url_env':'CUSTOM_VENDOR_URL'}}}
        env = dict(os.environ, NDI_SDK_SOURCES_JSON=json.dumps(sources),
                   CUSTOM_ARCHIVE_READ_TOKEN='independent-fixture-token',
                   CUSTOM_VENDOR_URL='https://example.invalid/private-fixture',
                   NDI_SDK_ARCHIVE_TOKEN='independent-default-token', UNRELATED_SETTING='preserved')
        script = ('import json,os; print(json.dumps({key:os.environ.get(key) for key in '
                  '["CUSTOM_ARCHIVE_READ_TOKEN","CUSTOM_VENDOR_URL","NDI_SDK_ARCHIVE_TOKEN",'
                  '"NDI_SDK_SOURCES_JSON","UNRELATED_SETTING"]}))')
        result = subprocess.run([sys.executable,str(self.store/'tools/sdk_store.py'),'--store',str(self.store),'exec',
                                 '--headers',self.key,'--runtime',self.key,'--build-root',str(self.root/'builds'),
                                 '--',sys.executable,'-c',script],env=env,capture_output=True,text=True)
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertEqual(json.loads(result.stdout),{
            'CUSTOM_ARCHIVE_READ_TOKEN':None,'CUSTOM_VENDOR_URL':None,'NDI_SDK_ARCHIVE_TOKEN':None,
            'NDI_SDK_SOURCES_JSON':None,'UNRELATED_SETTING':'preserved'})

    @unittest.skipUnless(sys.platform == 'linux' and sdk_store.platform.machine().lower() in ('x86_64','amd64'), 'fixture ELF is Linux x64')
    def test_portable_exec_preserves_literal_arguments_exit_code_and_store_read_only(self):
        self.populate()
        helper = self.store/'tools/sdk_store.py'
        argument = 'spaces $() `literal` ; untouched'
        command = [sys.executable,str(helper),'--store',str(self.store),'exec',
                   '--headers',self.key,'--runtime',self.key,'--build-root',str(self.root/'private builds'),
                   '--',sys.executable,'-c','import json,os,sys; print(json.dumps([sys.argv[1], os.environ["NDI_SDK_EDITION"],os.environ["RUSTC_WRAPPER"]])); sys.exit(7)',argument]
        before = {str(p.relative_to(self.store)):p.stat().st_mtime_ns for p in self.store.rglob('*')}
        result = subprocess.run(command,capture_output=True,text=True)
        self.assertEqual(result.returncode,7,result.stderr)
        self.assertEqual(json.loads(result.stdout),[argument,'standard',''])
        after = {str(p.relative_to(self.store)):p.stat().st_mtime_ns for p in self.store.rglob('*')}
        self.assertEqual(before,after)

    @unittest.skipUnless(shutil.which('cargo') and shutil.which('rustc') and sys.platform == 'linux' and sdk_store.platform.machine().lower() in ('x86_64','amd64'), 'native Cargo required for config precedence check')
    def test_empty_wrapper_environment_overrides_real_cargo_configuration(self):
        self.populate()
        project = self.root/'independent-rust-fixture'; (project/'src').mkdir(parents=True)
        (project/'Cargo.toml').write_text('[package]\nname="store_wrapper_fixture"\nversion="0.0.0"\nedition="2021"\n')
        (project/'src/lib.rs').write_text('pub fn fixture() -> u8 { 3 }\n')
        (project/'.cargo').mkdir()
        wrapper = self.root/'must-not-execute.sh'; wrapper.write_text('#!/bin/sh\nexit 93\n'); wrapper.chmod(0o700)
        (project/'.cargo/config.toml').write_text('[build]\nrustc-wrapper='+json.dumps(str(wrapper))+'\nrustc-workspace-wrapper='+json.dumps(str(wrapper))+'\n')
        # Freeze the already installed compiler so rustup cannot select/download
        # a channel while this offline fixture verifies Cargo's real precedence.
        rustc = subprocess.check_output(['rustup','which','rustc'],text=True).strip() if shutil.which('rustup') else shutil.which('rustc')
        env = dict(os.environ, RUSTC=rustc)
        result = subprocess.run([sys.executable,str(self.store/'tools/sdk_store.py'),'--store',str(self.store),'exec',
                                 '--headers',self.key,'--runtime',self.key,'--build-root',str(self.root/'private-builds'),
                                 '--','cargo','check','--offline'],cwd=project,env=env,capture_output=True,text=True)
        self.assertEqual(result.returncode,0,result.stdout+result.stderr)


if __name__ == '__main__':
    unittest.main()
