"""Resolve CI section references and preserve evidence for native exceptions."""
import ast
from pathlib import Path
import re
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
CITATION = re.compile(r'ci/README\.md(?:,\s*"([^"]+)"|#([a-z0-9-]+))?')
STALE = re.compile(r"README's known issues|README lists it as a known issue|see docs/troubleshooting\.md")


def reference_violations(root):
    readme = root / 'ci/README.md'
    headings = set(re.findall(r'^#{1,6}\s+(.+?)\s*$', readme.read_text(encoding='utf-8'), re.MULTILINE))
    anchors = {re.sub(r'[^a-z0-9 -]', '', title.lower()).replace(' ', '-') for title in headings}
    found = []
    for directory in (root / '.github', root / 'ci'):
        for path in sorted(directory.rglob('*')):
            if not path.is_file() or path.suffix not in {'.md', '.py', '.sh', '.toml', '.yml', '.yaml'}:
                continue
            lines = path.read_text(encoding='utf-8').splitlines()
            # CI citations occur in comments; masking code avoids interpreting
            # fixtures that deliberately name a missing section as references.
            documentation = [line if path.suffix == '.md' else
                             re.sub(r'^\s*#\s?', '', line) if line.lstrip().startswith('#') else ''
                             for line in lines]
            if path.suffix == '.py':
                for node in ast.walk(ast.parse('\n'.join(lines))):
                    body = getattr(node, 'body', None)
                    if (isinstance(body, list) and body and isinstance(body[0], ast.Expr)
                            and isinstance(body[0].value, ast.Constant)
                            and isinstance(body[0].value.value, str)):
                        for index in range(body[0].lineno - 1, body[0].end_lineno):
                            documentation[index] = lines[index]
            text = '\n'.join(documentation)
            for citation in CITATION.finditer(text):
                heading, anchor = citation.groups()
                if heading in headings or anchor in anchors:
                    continue
                line = text.count('\n', 0, citation.start()) + 1
                found.append(f'{path.relative_to(root)}:{line}: missing CI documentation section: '
                             + (heading or anchor or '(no section named)'))
    for directory in (root / 'src', root / 'tests'):
        for path in sorted(directory.rglob('*.rs')):
            for line, text in enumerate(path.read_text(encoding='utf-8').splitlines(), 1):
                if text.lstrip().startswith('//') and STALE.search(text):
                    found.append(f'{path.relative_to(root)}:{line}: native exception cites removed documentation')
    return found


class References(unittest.TestCase):
    def test_repository_citations_name_existing_sections(self):
        self.assertEqual(reference_violations(ROOT), [])

    def test_missing_sections_and_removed_native_evidence_fail(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'ci').mkdir()
            (root / 'src').mkdir()
            (root / '.github').mkdir()
            (root / 'ci/README.md').write_text('# Build\n\n## Release controls\n\n### Loopback-only test runs\n')
            workflow = root / '.github/check.yml'
            workflow.write_text('# See (ci/README.md, "Release controls").\n'
                                '# Windows uses (ci/README.md,\n# "Loopback-only test runs").\n')
            self.assertEqual(reference_violations(root), [])
            workflow.write_text('# See (ci/README.md, "Missing").\n# see ci/README.md\n')
            (root / 'src/test.rs').write_text('// The README lists it as a known issue.\n')
            errors = reference_violations(root)
            self.assertEqual(len(errors), 3)
            self.assertIn('Missing', errors[0])
            self.assertIn('no section named', errors[1])
            self.assertIn('removed documentation', errors[2])
            workflow.write_text('# See ci/README.md#release-controls\n')
            (root / 'src/test.rs').write_text('// See an immutable historical commit for the measurement.\n')
            self.assertEqual(reference_violations(root), [])


if __name__ == '__main__':
    unittest.main()
