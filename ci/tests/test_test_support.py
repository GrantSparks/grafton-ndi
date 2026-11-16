"""Keep native-test skips visible and prerequisites enforceable."""
from pathlib import Path
import re
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from docs import rust_tokens  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]


def skip_violations(root):
    found = []
    for directory in (root / 'src', root / 'tests'):
        for path in sorted(directory.rglob('*.rs')):
            text = path.read_text(encoding='utf-8')
            tokens = rust_tokens(text)
            words = [word for word, _ in tokens]

            def report(index, message):
                line = text.count('\n', 0, tokens[index][1]) + 1
                found.append(f'{path.relative_to(root)}:{line}: {message}')

            for index, word in enumerate(words):
                if (words[index:index + 3] == ['eprintln', '!', '(']
                        and index + 3 < len(words)
                        and re.match(r'(?:r#*)?"skipped', words[index + 3])):
                    report(index, 'use skip or unsupported so the skip remains visible')
                if word != 'if':
                    continue
                cursor, parentheses = index + 1, 0
                while cursor < len(words):
                    current = words[cursor]
                    if current == '{' and parentheses == 0:
                        break
                    if current in {'(', '['}:
                        parentheses += 1
                    elif current in {')', ']'}:
                        parentheses -= 1
                    cursor += 1
                condition = words[index + 1:cursor]
                failed = ('is_err' in condition
                          or condition[:2] == ['let', 'Err'])
                if 'require_feature' not in condition or not failed:
                    continue
                end, depth = cursor + 1, 1
                while end < len(words) and depth:
                    if words[end] == '{':
                        depth += 1
                    elif words[end] == '}':
                        depth -= 1
                    end += 1
                body = words[cursor + 1:end - 1]
                reports = any(body[position] in {'skip', 'unsupported', 'require_feature'}
                              and body[position + 1] == '('
                              for position in range(len(body) - 1))
                asserts_refusal = ('UnsupportedCapability' in body
                                   and any(word in body for word in {'assert', 'assert_eq', 'assert_ne'}))
                if 'return' in body and not reports and not asserts_refusal:
                    report(index, 'a failed require_feature returns without a visible skip')
    return found


class VisibleSkips(unittest.TestCase):
    def test_repository_native_tests_report_their_skips(self):
        self.assertEqual(skip_violations(ROOT), [])

    def test_captured_and_silent_skips_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'src').mkdir()
            source = root / 'src/test.rs'
            for pattern in [
                'eprintln!("skipped: unavailable");',
                'if ndi.require_feature(feature).is_err() { return; }',
                'if let Err(error) = ndi.require_feature(feature) { return; }',
                'if ndi\n.require_feature(feature)\n.is_err() { let text = "}"; return; }',
            ]:
                with self.subTest(pattern=pattern):
                    source.write_text(pattern)
                    self.assertEqual(len(skip_violations(root)), 1)

    def test_assertions_helpers_comments_and_strings_are_allowed(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'tests').mkdir()
            (root / 'tests/check.rs').write_text('''
                // eprintln!("skipped: not code");
                const EXAMPLE: &str = r#"if ndi.require_feature(x).is_err() { return; }"#;
                if let Err(error) = ndi.require_feature(feature) { assert!(error.is_expected()); }
                if ndi.require_feature(feature).is_ok() { return; }
                if ndi.require_feature(feature).is_err() {
                    assert!(matches!(operation(), Err(Error::UnsupportedCapability { .. })));
                    return;
                }
                if ndi.require_feature(feature).is_err() { common::skip(Advanced, "missing"); return; }
                if let Err(error) = ndi.require_feature(feature) { unsupported(&error.to_string()); return; }
                if !common::require_feature(ndi.require_feature(feature)) { return; }
            ''')
            self.assertEqual(skip_violations(root), [])


if __name__ == '__main__':
    unittest.main()
