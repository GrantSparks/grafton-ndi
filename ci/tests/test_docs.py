"""Failure cases for documentation links, anchors, examples and snippets."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import docs  # noqa: E402


class Documentation(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.write('Cargo.toml', '[package]\nname = "example"\nversion = "3.0.0"\n')
        self.write('README.md', '# Example\n')
        self.write('examples/README.md', '# Examples\n')

    def write(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding='utf-8')
        return path

    def check(self, text):
        self.write('README.md', text)
        return docs.check(self.root)

    def test_relative_links_and_same_document_anchors(self):
        self.write('docs/setup.md', '# Setup\n\n## SDK (`advanced`)\n\n## SDK (`advanced`)\n')
        errors, _ = self.check('[setup](docs/setup.md#sdk-advanced) [again](docs/setup.md#sdk-advanced-1)\n'
                               '# Example\n[back](#example)')
        self.assertEqual(errors, [])
        errors, _ = self.check('[missing](docs/setup.md#typo) [absent](docs/missing.md) [up](../secret)')
        self.assertEqual(len(errors), 3)
        self.assertIn('missing anchor', errors[0])
        self.assertIn('missing target', errors[1])
        self.assertIn('escapes the repository', errors[2])

    def test_directory_links_need_readme_only_for_fragments(self):
        self.write('docs/guide.md', '# Guide')
        errors, _ = self.check('[directory](docs/)')
        self.assertEqual(errors, [])
        errors, _ = self.check('[directory](docs/#guide)')
        self.assertIn('directory has no README', errors[0])
        self.write('docs/README.md', '# Guide')
        self.assertEqual(self.check('[directory](docs/#guide)')[0], [])

    def test_release_links_require_a_tag_and_check_its_actual_content(self):
        self.write('docs/setup.md', '# Setup')
        prefix = docs.REPOSITORY + '/blob/v3.0.0/'
        errors, _ = self.check(f'[setup]({prefix}docs/setup.md#setup)')
        self.assertEqual(len(errors), 1)
        self.assertIn('unpublished release tag', errors[0])
        future = docs.REPOSITORY + '/blob/v9.0.0/docs/setup.md#setup'
        self.assertIn('unpublished release tag', self.check(f'[future]({future})')[0][0])
        subprocess.run(['git', 'init', '-q', str(self.root)], check=True)
        subprocess.run(['git', '-C', str(self.root), 'add', '.'], check=True)
        subprocess.run(['git', '-C', str(self.root), '-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
                        'commit', '-qm', 'published fixture'], check=True)
        subprocess.run(['git', '-C', str(self.root), 'tag', 'v3.0.0'], check=True)
        self.write('docs/setup.md', '# Changed after release')
        self.assertEqual(self.check(f'[setup]({prefix}docs/setup.md#setup)')[0], [])
        self.assertIn('historical target is absent', self.check(f'[bad]({prefix}docs/absent.md)')[0][0])
        self.assertIn('missing anchor', self.check(f'[bad]({prefix}docs/setup.md#changed-after-release)')[0][0])

    def test_packaged_readme_requires_absolute_links_and_images(self):
        self.write('README.md', '[guide](docs/setup.md) ![image](logo.svg) [section](#install)')
        errors = docs.readme_link_violations(self.root)
        self.assertEqual(len(errors), 2)
        self.assertTrue(all('must use absolute URLs' in error for error in errors))
        self.write('README.md', '[guide](' + docs.REPOSITORY + '/blob/main/docs/setup.md) '
                   '![image](https://img.shields.io/crates/v/grafton-ndi.svg) [section](#install)')
        self.assertEqual(docs.readme_link_violations(self.root), [])

    def test_dependency_examples_reject_removed_features_including_renamed_dependencies(self):
        self.write('Cargo.toml', '[package]\nname = "grafton-ndi"\nversion = "3.0.0"\n'
                   '[features]\ndefault = []\nadvanced = []\nimage-encoding = []\n')
        for dependency in ('grafton-ndi = { features = ["removed-feature"] }',
                           'ndi = { package = "grafton-ndi", features = ["removed-feature"] }'):
            with self.subTest(dependency=dependency):
                errors, _ = self.check(f'```toml\n[dependencies]\n{dependency}\n```')
                self.assertEqual(len(errors), 1)
                self.assertIn('unknown grafton-ndi dependency feature: removed-feature', errors[0])
        valid = ('```toml\n[dependencies]\n'
                 'grafton-ndi = { version = "3.0", features = ["advanced", "image-encoding"] }\n'
                 'other = { version = "1", features = ["unrelated-feature"] }\n```')
        self.assertEqual(self.check(valid)[0], [])
        self.assertIn('invalid TOML example', self.check('```toml\n[dependencies\n```')[0][0])

    def test_readme_feature_table_rejects_removed_names_and_missing_features(self):
        self.write('Cargo.toml', '[package]\nname = "grafton-ndi"\nversion = "3.0.0"\n'
                   '[features]\ndefault = []\nadvanced = []\ntokio = []\nsmol = []\n')
        table = '| Feature | Adds |\n| --- | --- |\n| `advanced` | SDK |\n| `tokio`, `smol` | Async |\n'
        self.assertEqual(self.check(table)[0], [])
        errors, _ = self.check(table.replace('`advanced`', '`removed-feature`'))
        self.assertEqual(len(errors), 2)
        self.assertIn('unknown public feature: removed-feature', errors[0])
        self.assertIn('feature table is missing: advanced', errors[1])

    def test_historical_commit_links_use_historical_content(self):
        self.write('docs/old.md', '# Historical')
        subprocess.run(['git', 'init', '-q', str(self.root)], check=True)
        subprocess.run(['git', '-C', str(self.root), 'add', '.'], check=True)
        subprocess.run(['git', '-C', str(self.root), '-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
                        'commit', '-qm', 'fixture'], check=True)
        ref = subprocess.check_output(['git', '-C', str(self.root), 'rev-parse', 'HEAD'], text=True).strip()
        self.write('docs/old.md', '# Current')
        prefix = docs.REPOSITORY + '/blob/' + ref + '/'
        self.assertEqual(self.check(f'[old]({prefix}docs/old.md#historical)')[0], [])
        self.assertIn('missing anchor', self.check(f'[bad]({prefix}docs/old.md#current)')[0][0])
        self.assertIn('historical target is absent', self.check(f'[bad]({prefix}docs/absent.md)')[0][0])

    def test_parentheses_escaped_paths_reference_links_and_code(self):
        self.write('docs/a (b).md', '# One & Two')
        errors, _ = self.check('[one](docs/a%20(b).md#one--two) [two](<docs/a (b).md>)\n'
                               '[three]: docs/a%20(b).md\n'
                               '`[ignored](missing.md)`\n```sh\n[ignored](missing.md)\n```\n')
        self.assertEqual(errors, [])

    def test_source_line_anchors_have_valid_bounds(self):
        self.write('src/lib.rs', '// one\n// two\n')
        self.assertEqual(self.check('[source](src/lib.rs#L1-L2)')[0], [])
        self.assertIn('unsupported source anchor', self.check('[source](src/lib.rs#L3)')[0][0])

    def test_rustdoc_guide_entry_uses_the_same_link_checks(self):
        prefix = docs.REPOSITORY + '/blob/main/'
        self.write('src/lib.rs', f'//! See [guides]({prefix}docs/missing.md).\n')
        errors, _ = self.check('# Example')
        self.assertIn('src/lib.rs:1: missing target', errors[0])

    def test_doc_comment_extraction_preserves_line_numbers(self):
        text = 'mod nested {\n    //! Inner docs\n    // ordinary\n    /// Outer docs\n    //// ordinary\n}\n'
        self.assertEqual(docs.doc_comment_text(text), '\nInner docs\n\nOuter docs\n\n')

    def test_nested_module_guides_checked_without_treating_intradoc_names_as_paths(self):
        prefix = docs.REPOSITORY + '/blob/main/'
        self.write('src/nested/mod.rs', '//! [Frame](VideoFrame)\n\npub struct X;\n'
                   f'/// Read [guide]({prefix}docs/missing.md).\n')
        errors, _ = self.check('# Example')
        self.assertEqual(len(errors), 1)
        self.assertIn('src/nested/mod.rs:4: missing target', errors[0])
        self.write('src/nested/mod.rs', '//! [Frame](VideoFrame)\n/// [Module](crate::frames)\n')
        self.assertEqual(self.check('# Example')[0], [])

    def test_main_guides_check_paths_and_anchors_in_markdown_and_module_docs(self):
        self.write('docs/setup.md', '# Setup')
        url = docs.REPOSITORY + '/blob/main/docs/setup.md#setup'
        self.write('src/nested/mod.rs', f'// ordinary\n\n/// Read [setup]({url}).\n')
        self.assertEqual(self.check(f'[Setup]({url})')[0], [])
        self.write('docs/setup.md', '# Changed')
        errors, _ = self.check(f'[Setup]({url})')
        self.assertEqual(len(errors), 2)
        self.assertTrue(all('missing anchor' in error for error in errors))
        self.assertTrue(any('src/nested/mod.rs:3:' in error for error in errors))

    def test_latest_api_link_fails_but_external_pages_are_not_fetched(self):
        errors, _ = self.check('[API](https://docs.rs/grafton-ndi/latest/grafton_ndi/)')
        self.assertIn('must name version 3.0.0', errors[0])
        self.assertEqual(self.check('[API](https://docs.rs/grafton-ndi/3.0.0/grafton_ndi/) '
                                    '[external](https://example.invalid/absent)')[0], [])

    def test_generated_rustdoc_pages_and_real_html_anchors(self):
        self.write('target/doc/grafton_ndi/index.html', '<html><main id="modules"></main></html>')
        self.write('target/doc/grafton_ndi/finder/struct.Finder.html',
                   '<html><section id="method.new"></section><a name="legacy"></a></html>')
        for prefix in (docs.API_SITE, 'https://docs.rs/grafton-ndi/3.0.0/'):
            with self.subTest(prefix=prefix):
                self.write('README.md', f'[root]({prefix}) [index]({prefix}grafton_ndi/#modules) '
                           f'[constructor]({prefix}grafton_ndi/finder/struct.Finder.html#method.new) '
                           f'[old anchor]({prefix}grafton_ndi/finder/struct.Finder.html#legacy)')
                self.assertEqual(docs.rustdoc_violations(self.root, self.root / 'target/doc'), [])
                self.write('README.md', f'[bad page]({prefix}grafton_ndi/struct.Finder.html) '
                           f'[bad method]({prefix}grafton_ndi/finder/struct.Finder.html#method.wrong)')
                errors = docs.rustdoc_violations(self.root, self.root / 'target/doc')
                self.assertEqual(len(errors), 2)
                self.assertIn('missing generated rustdoc page', errors[0])
                self.assertIn('missing generated rustdoc anchor', errors[1])

    def test_api_links_cannot_escape_rustdoc_or_match_another_pages_site(self):
        for prefix in (docs.API_SITE, 'https://docs.rs/grafton-ndi/3.0.0/'):
            with self.subTest(prefix=prefix):
                self.write('README.md', f'[escape]({prefix}%2e%2e/secret.html)')
                errors = docs.rustdoc_violations(self.root, self.root / 'target/doc')
                self.assertEqual(len(errors), 1)
                self.assertIn('escapes the generated directory', errors[0])
        self.write('README.md', '[other project](https://grantsparks.github.io/another-project/missing.html) '
                   '[similar name](https://grantsparks.github.io/grafton-ndi-other/missing.html)')
        self.assertEqual(docs.rustdoc_violations(self.root, self.root / 'target/doc'), [])

    def test_rustdoc_check_ignores_other_versions_and_repository_links(self):
        self.write('README.md', '[old release](https://docs.rs/grafton-ndi/2.0.0/grafton_ndi/) '
                   '[guide](' + docs.REPOSITORY + '/blob/v3.0.0/README.md)')
        self.assertEqual(docs.rustdoc_violations(self.root, self.root / 'target/doc'), [])
        self.write('src/lib.rs', '//! [API](https://docs.rs/grafton-ndi/3.0.0/grafton_ndi/absent.html)')
        errors = docs.rustdoc_violations(self.root, self.root / 'target/doc')
        self.assertEqual(len(errors), 1)
        self.assertIn('src/lib.rs:1: missing generated rustdoc page', errors[0])

    def test_nested_module_api_urls_check_pages_and_anchors_at_source_line(self):
        url = docs.API_SITE + 'grafton_ndi/frames/struct.VideoFrame.html#method.new'
        self.write('src/nested/mod.rs', f'// ordinary\n\n/// Read [frame]({url}).\n')
        errors = docs.rustdoc_violations(self.root, self.root / 'target/doc')
        self.assertIn('src/nested/mod.rs:3: missing generated rustdoc page', errors[0])
        self.write('target/doc/grafton_ndi/frames/struct.VideoFrame.html', '<html><div id="method.other"></div></html>')
        errors = docs.rustdoc_violations(self.root, self.root / 'target/doc')
        self.assertIn('src/nested/mod.rs:3: missing generated rustdoc anchor', errors[0])
        self.write('target/doc/grafton_ndi/frames/struct.VideoFrame.html', '<html><div id="method.new"></div></html>')
        self.assertEqual(docs.rustdoc_violations(self.root, self.root / 'target/doc'), [])

    def test_rust_fences_need_standalone_main_or_linked_example(self):
        errors, snippets = self.check('```rust\nfn main() { println!("hello"); }\n```\n')
        self.assertEqual(errors, [])
        self.assertEqual(len(snippets), 1)
        self.assertEqual(snippets[0].line, 1)
        self.assertIn('must be standalone', self.check('```rust\nlet x = 1;\n```')[0][0])
        self.assertIn('needs a nearby link', self.check('```rust,ignore\nlet x = 1;\n```')[0][0])
        self.write('examples/one.rs', 'fn main() {}')
        self.write('examples/README.md', '[One](one.rs)')
        self.assertEqual(self.check('[Runnable](examples/one.rs)\n```rust,ignore\nlet x = 1;\n```')[0], [])
        self.write('src/lib.rs', '// Not a runnable example')
        self.assertIn('needs a nearby link',
                      self.check('[Source](src/lib.rs)\n```rust,ignore\nlet x = 1;\n```')[0][0])

    def test_hidden_rustdoc_lines_and_unclosed_fences_fail(self):
        errors, _ = self.check('```rust\n# fn main() {}\n```')
        self.assertTrue(any('hidden lines' in error for error in errors))
        self.assertIn('unclosed code fence', self.check('```sh\necho hello')[0][0])

    def test_example_index_requires_every_top_level_example(self):
        self.write('examples/one.rs', 'fn main() {}')
        self.write('examples/support/helper.rs', '// helper, not an example')
        errors, _ = self.check('# Example')
        self.assertEqual(len(errors), 1)
        self.assertIn('missing example: one.rs', errors[0])
        self.write('examples/README.md', '[One](one.rs)')
        self.assertEqual(self.check('# Example')[0], [])

    def test_changelog_keeps_historical_code_but_links_are_checked(self):
        self.write('CHANGELOG.md', '```rust\nold_api();\n```\n[missing](missing.md)')
        errors, snippets = self.check('# Example')
        self.assertEqual(len(errors), 1)
        self.assertIn('missing target', errors[0])
        self.assertEqual(snippets, [])


if __name__ == '__main__':
    unittest.main()
