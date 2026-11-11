"""The reviewed real-SDK inventory must remain opt-in and exactly executable."""
from collections import Counter
import importlib.util
from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('core_tests', ROOT / 'ci/core_tests.py')
POLICY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(POLICY)


def marked_functions(source):
    return re.findall(
        r'#\[test\]\s*#\[ignore = "' + re.escape(POLICY.CORE_IGNORE_REASON)
        + r'"\]\s*fn\s+(\w+)\s*\(', source)


class CoreRegistryTests(unittest.TestCase):
    def test_reviewed_inventory_has_no_duplicate_or_missing_source_tests(self):
        self.assertEqual(len(POLICY.CORE_TESTS), 6)
        self.assertEqual(sum(map(len, POLICY.CORE_TESTS.values())), 39)
        expected = {}
        for suite, names in POLICY.CORE_TESTS.items():
            self.assertEqual(len(names), len(set(names)), suite)
            for name in names:
                parts = name.split('::')
                if suite == 'grafton_ndi':
                    self.assertEqual(parts[1:-1], ['runtime_tests'] if parts[0] == 'tests' else ['tests'])
                    path = ROOT / 'src' / (parts[0] + '.rs')
                else:
                    self.assertEqual(len(parts), 1)
                    path = ROOT / 'tests' / (suite + '.rs')
                expected.setdefault(path, Counter())[parts[-1]] += 2 if suite == 'windows_dll_check' else 1
        observed = {}
        for directory in ('src', 'tests'):
            for path in (ROOT / directory).rglob('*.rs'):
                names = marked_functions(path.read_text())
                if names:
                    observed[path] = Counter(names)
        self.assertEqual(observed, expected)

    def test_pass_fixtures_type_check_without_starting_the_sdk(self):
        for name in ('receiver_refs_in_scope', 'framesync_refs_in_scope'):
            source = (ROOT / 'tests/ui/pass' / (name + '.rs')).read_text()
            self.assertRegex(source, r'fn main\(\)\s*\{\s*\}\s*$')
            self.assertEqual(source.count('fn compile_contract() -> Result<(), grafton_ndi::Error>'), 1)
            self.assertIn('let ndi = NDI::new()?;', source)
            self.assertIn('.to_owned()', source)

    def test_runtime_lifecycle_cannot_treat_initialization_failure_as_success(self):
        source = (ROOT / 'src/tests.rs').read_text().split('mod runtime_tests {', 1)[1]
        self.assertNotIn('if let Ok', source)
        self.assertNotIn('Err(_) => return', source)
        self.assertIn('assert_eq!(success_count.load(Ordering::Relaxed), 500)', source)


if __name__ == '__main__':
    unittest.main()
