"""Offline mutation tests using independently authored C/Rust fixtures, no NDI SDK."""
import importlib.util
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import audit

HEADER = '''#pragma once
#include <stdint.h>
#define NDI_FIXTURE_MODE 7
typedef enum NDIlib_kind_e { NDIlib_kind_zero, NDIlib_kind_negative = -2, NDIlib_kind_next } NDIlib_kind_e;
typedef struct NDIlib_fixture_t {
    uint32_t first;
    uint32_t second;
    union { uint32_t a; float b; };
} NDIlib_fixture_t;
int NDIlib_fixture_call(const NDIlib_fixture_t* value);
'''


class AttributeFingerprints(unittest.TestCase):
    def test_default_visibility_is_stable_across_clang_json_versions(self):
        legacy = {'kind': 'FunctionDecl', 'signature': 'int (void)',
                  'attributes': [{'kind': 'VisibilityAttr'}]}
        explicit = json.loads(json.dumps(legacy))
        explicit['attributes'][0]['visibility'] = 'default'
        self.assertEqual(audit.semantic(legacy), audit.semantic(explicit))
        self.assertEqual(explicit['attributes'][0]['visibility'], 'default')

    def test_nondefault_and_unknown_attribute_payloads_still_change_fingerprint(self):
        baseline = {'kind': 'FunctionDecl', 'signature': 'int (void)',
                    'attributes': [{'kind': 'VisibilityAttr'}]}
        for visibility in ('hidden', 'protected', 'future-unknown'):
            changed = json.loads(json.dumps(baseline))
            changed['attributes'][0]['visibility'] = visibility
            with self.subTest(visibility=visibility):
                self.assertNotEqual(audit.digest(audit.semantic(baseline)),
                                    audit.digest(audit.semantic(changed)))
        changed = json.loads(json.dumps(baseline))
        changed['attributes'][0].update(visibility='default', future_payload=True)
        self.assertNotEqual(audit.semantic(baseline), audit.semantic(changed))
        other = {'attributes': [{'kind': 'AnotherAttr', 'visibility': 'default'}]}
        self.assertEqual(audit.semantic(other), other)


class DependencyPaths(unittest.TestCase):
    def test_windows_native_separators_drive_colons_and_unc_survive(self):
        emitted = ('probe: C:\\Users\\runner\\probe.c \\\r\n '
                   r'C:\Program\ Files\LLVM\include\stddef.h '
                   r'C:\folder\\#hash.h C:\$$cash\header.h \\server\share\header.h' + '\r\n')
        expected = [r'C:\Users\runner\probe.c', r'C:\Program Files\LLVM\include\stddef.h',
                    r'C:\folder\#hash.h', r'C:\$cash\header.h', r'\\server\share\header.h']
        self.assertEqual(audit.make_dependencies(emitted, target='probe'), expected)
        self.assertEqual(audit.make_dependencies(r'probe: C:\\\ space\header.h', target='probe'),
                         [r'C:\ space\header.h'])

    def test_make_escaping_and_quotes_are_not_shell_escaping(self):
        emitted = ('audit: /sdk/a\\ b.h /sdk/hash\\#.h /sdk/$$value.h '
                   '/sdk/quote\'s.h /sdk/"literal".h \\\n /sdk/last.h\n')
        self.assertEqual(audit.make_dependencies(emitted, target='audit'),
            ['/sdk/a b.h', '/sdk/hash#.h', '/sdk/$value.h', "/sdk/quote's.h",
             '/sdk/"literal".h', '/sdk/last.h'])
        self.assertEqual(audit.make_dependencies('audit:\n', target='audit'), [])
        for malformed in ('other: /file.h\n', 'audit: /file.h\nother: /second.h\n',
                          'audit: /unescaped$var.h\n'):
            with self.subTest(malformed=malformed), self.assertRaises(ValueError):
                audit.make_dependencies(malformed, target='audit')

    @unittest.skipUnless(shutil.which('clang'), 'Clang required for emitted dependency paths')
    def test_real_clang_dependencies_resolve_exotic_posix_filenames(self):
        if os.name == 'nt':
            self.skipTest('POSIX filename fixture includes a literal double quote')
        with tempfile.TemporaryDirectory(prefix='ndi-dependency-paths-') as temporary:
            root = pathlib.Path(temporary) / 'space\'s "quotes" #$'
            root.mkdir()
            header = root/'literal #$.h'; header.write_text('#define FIXTURE 1\n')
            source = root/'source file.c'; source.write_text('#include "literal #$.h"\n')
            output = audit.run(['clang', '-M', '-MT', 'probe', str(source)]).stdout
            paths = [pathlib.Path(p).resolve(strict=True)
                     for p in audit.make_dependencies(output, target='probe')]
            self.assertEqual(set(paths), {source.resolve(), header.resolve()})
            stdin_output = audit.run(['clang', '-x', 'c', '-M', '-MT', 'audit', '-I', str(root), '-'],
                                    stdin='#include "literal #$.h"\n').stdout
            self.assertEqual(audit.make_dependencies(stdin_output, target='audit'), [str(header)])


@unittest.skipUnless(shutil.which('clang'), 'Clang required for real header-mutation tests')
class AuditMutations(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='ndi-audit-test-')
        self.root = pathlib.Path(self.tmp.name)
        (self.root / 'include').mkdir()
        self.header = self.root / 'include/Processing.NDI.Lib.h'
        self.header.write_text(HEADER)
        self.target = audit.run(['clang', '-dumpmachine']).stdout.strip()
        self.before = self.snapshot('before')
        self.registry = self.root / 'registry.toml'
        self.seed_fixture_registry(self.before)

    def tearDown(self):
        self.tmp.cleanup()

    def cli(self, *args, expected=0):
        result = subprocess.run([sys.executable, str(HERE / 'audit.py'), *map(str, args)], capture_output=True, text=True)
        self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
        return result

    def snapshot(self, label):
        path = self.root / (label + '.json')
        self.cli('snapshot', '--sdk-root', self.root, '--edition', 'standard', '--target', self.target, '--output', path)
        return path

    def seed_fixture_registry(self, snapshot):
        # This only approves the known test fixture. There is intentionally no
        # automatic acceptance command for production/vendor snapshots.
        m = audit.read_snapshot(snapshot)
        lines = ['schema = 1', '']
        categories = {
            'declaration': {k: audit.digest(audit.semantic(v)) for k,v in m['api']['declarations'].items()},
            'header': {pathlib.PurePath(k).name: v['sha256'] for k,v in m['headers'].items()},
            'macro': {k: audit.digest(v) for k,v in m['api']['macros'].items()},
            'value': {v['name']: audit.digest({'enum':d['name'], **v}) for d in m['api']['declarations'].values()
                      if d['kind']=='EnumDecl' for v in d['constants']},
        }
        for category, observations in categories.items():
            for name, fingerprint in observations.items():
                lines += ['[['+category+']]', 'id = '+json.dumps(name), 'sha256 = '+json.dumps([fingerprint]), 'evidence = ["independently authored test fixture"]']
                if category == 'declaration':
                    lines += ['earliest_observed = "fixture"', 'disposition = "planned"', 'wrapper = "fixture"', 'rationale = "explicit test fixture review"']
        self.registry.write_text('\n'.join(lines)+'\n')

    def review(self, snapshot, expected=0, extra=()):
        output = self.root / 'review.json'
        self.cli('classify', '--manifest', snapshot, '--registry', self.registry, '--output', output, *extra, expected=expected)
        return json.loads(output.read_text())

    def test_nondefault_visibility_requires_review_even_with_legacy_clang(self):
        default = HEADER.replace('int NDIlib_fixture_call',
                                 '__attribute__((visibility("default"))) int NDIlib_fixture_call')
        self.header.write_text(default)
        self.seed_fixture_registry(self.snapshot('default-visibility'))
        for visibility in ('hidden', 'protected'):
            with self.subTest(visibility=visibility):
                self.header.write_text(default.replace('visibility("default")',
                                                       'visibility("' + visibility + '")'))
                result = self.review(self.snapshot(visibility + '-visibility'), expected=1)
                self.assertIn({'category': 'header', 'id': 'Processing.NDI.Lib.h',
                               'reason': 'changed_requires_review'}, result['classification_failures'])

    def test_snapshot_deterministic_and_registration_is_not_implementation(self):
        repeated = self.snapshot('repeat')
        self.assertEqual(self.before.read_bytes(), repeated.read_bytes())
        result = self.review(self.before)
        self.assertTrue(result['classification_passed'])
        self.assertFalse(result['implementation_coverage_passed'])
        self.review(self.before, expected=1, extra=('--require-implemented',))

    def test_added_function_requires_classification(self):
        self.header.write_text(HEADER+'void NDIlib_new_function(void);\n')
        result = self.review(self.snapshot('new'), expected=1)
        self.assertIn({'category':'declaration','id':'FunctionDecl:NDIlib_new_function','reason':'unclassified'}, result['classification_failures'])

    def test_reviewed_unavailable_is_explicit_and_never_complete_surface(self):
        text = self.registry.read_text().replace('disposition = "planned"', 'disposition = "implemented"')
        text = text.replace('id = "FunctionDecl:NDIlib_fixture_call"',
                            'id = "FunctionDecl:NDIlib_fixture_call"\nscope_decision = "Test-only explicit unavailable scope"')
        sections = re.split(r'(?=\[\[declaration\]\])', text)
        for i, section in enumerate(sections):
            if 'id = "FunctionDecl:NDIlib_fixture_call"' in section:
                sections[i] = section.replace('disposition = "implemented"', 'disposition = "reviewed_unavailable"', 1)
        self.registry.write_text(''.join(sections))
        result = self.review(self.before, extra=('--require-implemented',))
        self.assertTrue(result['implementation_coverage_passed'])
        self.assertFalse(result['complete_sdk_surface'])
        self.assertEqual([row['id'] for row in result['approved_unavailable']], ['FunctionDecl:NDIlib_fixture_call'])
        self.review(self.before, expected=1, extra=('--require-complete-surface',))
        self.registry.write_text(self.registry.read_text().replace('scope_decision = "Test-only explicit unavailable scope"\n', ''))
        self.cli('classify', '--manifest', self.before, '--registry', self.registry,
                 '--output', self.root/'invalid.json', expected=1)

    def test_removed_baseline_function_fails_even_when_remaining_shapes_known(self):
        self.header.write_text(HEADER.replace('int NDIlib_fixture_call(const NDIlib_fixture_t* value);', ''))
        after = self.snapshot('removed')
        self.seed_fixture_registry(after)
        result = self.review(after, expected=1, extra=('--baseline', self.before))
        self.assertIn({'category':'declaration','id':'FunctionDecl:NDIlib_fixture_call','reason':'baseline_declaration_removed'}, result['classification_failures'])

    def test_signature_enum_and_same_size_field_reorder_detected(self):
        self.header.write_text(HEADER.replace('int NDIlib_fixture_call', 'long NDIlib_fixture_call')
                               .replace('NDIlib_kind_negative = -2', 'NDIlib_kind_negative = -9')
                               .replace('uint32_t first;\n    uint32_t second;', 'uint32_t second;\n    uint32_t first;'))
        after = self.snapshot('changed')
        result = self.review(after, expected=1)
        failed = {x['id'] for x in result['classification_failures'] if x['category']=='declaration'}
        self.assertTrue({'FunctionDecl:NDIlib_fixture_call','EnumDecl:NDIlib_kind_e','RecordDecl:NDIlib_fixture_t'} <= failed)
        output = self.root / 'diff.json'
        self.cli('diff', '--old', self.before, '--new', after, '--output', output)
        delta = json.loads(output.read_text())
        self.assertIn('NDIlib_fixture_t', delta['record_layouts']['changed'])
        old, new = audit.read_snapshot(self.before), audit.read_snapshot(after)
        self.assertEqual(old['api']['record_layouts']['NDIlib_fixture_t'][-1], new['api']['record_layouts']['NDIlib_fixture_t'][-1])

    def test_comment_only_change_is_reviewed_without_false_abi_change(self):
        self.header.write_text('// Contract changed; please review.\n'+HEADER)
        after = self.snapshot('comments')
        output = self.root / 'diff.json'
        self.cli('diff','--old',self.before,'--new',after,'--output',output)
        delta=json.loads(output.read_text())
        self.assertFalse(delta['api']['changed'])
        self.assertTrue(delta['headers']['changed'])
        result=self.review(after,expected=1)
        self.assertTrue(any(x['category']=='header' for x in result['classification_failures']))

    def test_configuration_mismatch_and_manifest_tampering_rejected(self):
        value=json.loads(self.before.read_text())
        value['api']['target']='aarch64-unknown-linux-gnu'
        path=self.root/'tampered.json';audit.write_json(path,value)
        self.cli('diff','--old',self.before,'--new',path,'--output',self.root/'diff.json',expected=1)
        value['identity'].pop('snapshot_sha256')
        value['identity']['snapshot_sha256']=audit.digest(value)
        audit.write_json(path,value)
        result=self.cli('diff','--old',self.before,'--new',path,'--output',self.root/'diff.json',expected=1)
        self.assertIn('Incomparable', result.stderr)

    def test_sopcinfo_is_not_a_library(self):
        (self.root/'ndi_hps.sopcinfo').write_text('<system/>')
        manifest=audit.read_snapshot(self.snapshot('names'))
        self.assertEqual(manifest['binaries'],{})
        self.assertTrue(audit.is_library_name('libndi.so.6.4.0'))
        self.assertFalse(audit.is_library_name('ndi_hps.sopcinfo'))

    @unittest.skipUnless(shutil.which('cc') and shutil.which('readelf'), 'ELF fixture tools unavailable')
    def test_duplicate_soname_is_not_silently_overwritten(self):
        source=self.root/'lib.c';source.write_text('int NDIlib_fixture_export(void) { return 3; }\n')
        for name in ['libndi_a.so.1','libndi_b.so.1']:
            audit.run(['cc','-shared','-fPIC','-Wl,-soname,libndi.so.1',str(source),'-o',str(self.root/name)])
        after=self.snapshot('libraries')
        result=self.cli('diff','--old',after,'--new',after,'--output',self.root/'diff.json',expected=1)
        self.assertIn('Ambiguous runtime identity',result.stderr)

    @unittest.skipUnless(shutil.which('cc') and shutil.which('readelf'), 'ELF fixture tools unavailable')
    def test_undocumented_export_requires_its_own_classification(self):
        source=self.root/'lib.c';source.write_text('int NDIlib_surprise(void) { return 3; }\n')
        audit.run(['cc','-shared','-fPIC',str(source),'-o',str(self.root/'libndi.so')])
        result=self.review(self.snapshot('exports'),expected=1)
        self.assertIn({'category':'binary_export','id':'NDIlib_surprise','reason':'unclassified_native_symbol'}, result['classification_failures'])


@unittest.skipUnless(shutil.which('cc') and shutil.which('rustc') and audit.find_tool('llvm-readobj') and audit.find_tool('llvm-objcopy'), 'C/Rust/LLVM probe tools unavailable')
class IndependentAbiProbe(unittest.TestCase):
    def test_actual_compiler_failures_report_the_fixed_stage(self):
        with tempfile.TemporaryDirectory(prefix='ndi-abi-stage-test-') as temporary:
            root = pathlib.Path(temporary)
            (root/'include').mkdir()
            (root/'include/Processing.NDI.Lib.h').write_text('/* independent fixture */\n')
            bindings = root/'bindings.rs'; bindings.write_text('')
            spec = root/'spec.toml'; spec.write_text('schema=1\n')
            target = re.search(r'^host: (\S+)', audit.run(['rustc','-vV']).stdout, re.M)[1]
            command = [sys.executable, str(HERE/'abi_probe.py'), '--sdk-root', str(root),
                       '--rust-bindings', str(bindings), '--target', target, '--spec', str(spec),
                       '--output', str(root/'probe.json')]
            for option, stage in (('--cc', 'c-compile'), ('--rustc', 'rust-compile')):
                with self.subTest(stage=stage):
                    result = subprocess.run(command + [option, str(root/'private-missing-compiler')],
                                            capture_output=True, text=True)
                    self.assertEqual(result.returncode, 1)
                    self.assertIn('ABI_PROBE_STAGE=' + stage + '\n', result.stderr)
                    self.assertFalse((root/'probe.json').exists())

    def test_field_extent_detects_array_change_hidden_by_padding(self):
        with tempfile.TemporaryDirectory(prefix='ndi-abi-field-test-') as temporary:
            root = pathlib.Path(temporary); (root/'include').mkdir()
            (root/'include/Processing.NDI.Lib.h').write_text('typedef struct NDIlib_fixture_t { unsigned char buffer[3]; unsigned int next; } NDIlib_fixture_t;\n')
            bindings = root/'bindings.rs'
            bindings.write_text('#[repr(C)] pub struct NDIlib_fixture_t { pub buffer: [u8; 4], pub next: u32 }\n')
            spec = root/'spec.toml'
            spec.write_text('schema=1\n[[record]]\nc="NDIlib_fixture_t"\nfield_sizes=true\nfields=[{c="buffer"},{c="next"}]\n')
            target = re.search(r'^host: (\S+)', audit.run(['rustc','-vV']).stdout, re.M)[1]
            output = root/'probe.json'
            result = subprocess.run([sys.executable,str(HERE/'abi_probe.py'),'--sdk-root',str(root),'--rust-bindings',str(bindings),'--target',target,'--spec',str(spec),'--output',str(output)], capture_output=True,text=True)
            self.assertEqual(result.returncode, 1, result.stdout+result.stderr)
            self.assertIn('ABI_PROBE_STAGE=comparison\n', result.stderr)
            self.assertEqual(json.loads(output.read_text())['mismatches'], ['NDIlib_fixture_t.buffer.size'])

    def test_same_size_rust_field_reorder_fails_real_compiler_probe(self):
        with tempfile.TemporaryDirectory(prefix='ndi-abi-test-') as temporary:
            root=pathlib.Path(temporary);(root/'include').mkdir()
            (root/'include/Processing.NDI.Lib.h').write_text('typedef struct NDIlib_fixture_t { unsigned int first; unsigned int second; } NDIlib_fixture_t;\n')
            bindings=root/'bindings.rs'
            bindings.write_text('#[repr(C)] pub struct NDIlib_fixture_t { pub first: u32, pub second: u32 }\n')
            spec=root/'spec.toml';spec.write_text('schema=1\n[[record]]\nc="NDIlib_fixture_t"\nfields=[{c="first"},{c="second"}]\n')
            verbose=audit.run(['rustc','-vV']).stdout
            target=re.search(r'^host: (\S+)',verbose,re.M)[1]
            output=root/'probe.json'
            command=[sys.executable,str(HERE/'abi_probe.py'),'--sdk-root',str(root),'--rust-bindings',str(bindings),'--target',target,'--spec',str(spec),'--output',str(output)]
            good=subprocess.run(command,capture_output=True,text=True)
            self.assertEqual(good.returncode,0,good.stderr)
            self.assertTrue(json.loads(output.read_text())['passed'])
            bindings.write_text('#[repr(C)] pub struct NDIlib_fixture_t { pub second: u32, pub first: u32 }\n')
            bad=subprocess.run(command,capture_output=True,text=True)
            self.assertEqual(bad.returncode,1,bad.stdout+bad.stderr)
            self.assertIn('ABI_PROBE_STAGE=comparison\n', bad.stderr)
            result=json.loads(output.read_text())
            self.assertEqual(set(result['mismatches']),{'NDIlib_fixture_t.first','NDIlib_fixture_t.second'})
            self.assertNotIn('NDIlib_fixture_t.size',result['mismatches'])


if __name__ == '__main__':
    unittest.main()
