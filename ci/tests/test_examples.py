"""Examples with unit tests must opt in to Cargo's test harness."""
from pathlib import Path
import re
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]


def example_test_violations(root):
    manifest = tomllib.loads((root / 'Cargo.toml').read_text(encoding='utf-8'))
    settings = {example['name']: example for example in manifest.get('example', [])}
    found = []

    def contains_tests(path, seen):
        path = path.resolve()
        if path in seen:
            return False
        seen.add(path)
        text = path.read_text(encoding='utf-8')
        if re.search(r'#\s*\[\s*test\s*\]', text):
            return True
        return any(contains_tests(path.parent / child, seen) for child in
                   re.findall(r'#\s*\[\s*path\s*=\s*"([^"]+)"\s*\]\s*mod\s+\w+\s*;', text))

    for path in sorted((root / 'examples').glob('*.rs')):
        if contains_tests(path, set()) and settings.get(path.stem, {}).get('test') is not True:
            found.append(f'{path.relative_to(root)}: example contains unit tests but has no test = true')
    return found


class ExampleTests(unittest.TestCase):
    def test_repository_examples_run_their_unit_tests(self):
        self.assertEqual(example_test_violations(ROOT), [])

    def test_direct_and_path_module_tests_require_the_harness(self):
        for source, child in [('#[test] fn checks() {}', None),
                              ('#[path = "common/checks.rs"] mod checks;', '#[test] fn checks() {}')]:
            with self.subTest(source=source), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / 'examples/common').mkdir(parents=True)
                (root / 'examples/demo.rs').write_text(source)
                if child:
                    (root / 'examples/common/checks.rs').write_text(child)
                (root / 'Cargo.toml').write_text('[package]\nname = "demo"\nversion = "1.0.0"\n')
                self.assertEqual(len(example_test_violations(root)), 1)
                with (root / 'Cargo.toml').open('a') as manifest:
                    manifest.write('[[example]]\nname = "demo"\ntest = true\n')
                self.assertEqual(example_test_violations(root), [])


if __name__ == '__main__':
    unittest.main()
