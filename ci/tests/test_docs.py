"""Failure cases for documentation links, anchors, examples and snippets."""
import contextlib
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import docs  # noqa: E402


class Documentation(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.internal_entry_points = dict(docs.INTERNAL_SDK_ENTRY_POINTS)
        exemptions = mock.patch.dict(docs.INTERNAL_SDK_ENTRY_POINTS, {}, clear=True)
        exemptions.start()
        self.addCleanup(exemptions.stop)
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

    def test_prose_api_names_resolve_against_items_and_members(self):
        self.write('target/doc/grafton_ndi/index.html', '<a href="struct.Foo.html">Foo</a>')
        self.write('target/doc/grafton_ndi/struct.Foo.html',
                   '<section id="method.capture_any"></section><section id="structfield.line_stride"></section>')
        self.write('README.md', 'Use `Foo::capture_any()` with `capture_any` and `line_stride`.\n'
                   'Then call `Foo::removed()` or `removed_method(`.\n')
        errors = docs.guide_api_name_violations(self.root, self.root / 'target/doc')
        self.assertEqual(len(errors), 2)
        self.assertIn('README.md:2: unknown documented API name: Foo::removed()', errors[0])
        self.assertIn('removed_method(', errors[1])

    def test_prose_name_check_skips_history_fences_and_non_api_vocabulary(self):
        self.write('target/doc/grafton_ndi/index.html', '<html></html>')
        self.write('examples/old_example.rs', 'fn main() {}')
        self.write('README.md', '`old_example` `ndi_color_info` `x86_64-unknown-linux-gnu` `*_frames_sent` '
                   '`src/old_api.rs` `grafton_ndi::image_encoding`\n'
                   '```rust\n`removed_method`\n```\n')
        self.write('docs/migration/2.0-to-3.0.md', 'Replace `removed_method` and `Foo::old()`.')
        self.write('docs/api-conventions.md', 'Use `<kind>` and `new_<kind>`.')
        self.write('CHANGELOG.md', 'Removed `old_method`.')
        self.assertEqual(docs.guide_api_name_violations(self.root, self.root / 'target/doc'), [])
        self.write('docs/usage.md', 'The `removed_method` is gone.')
        self.assertIn('docs/usage.md:1:', docs.guide_api_name_violations(self.root, self.root / 'target/doc')[0])

    def test_rustdoc_cli_rejects_a_missing_prose_method(self):
        self.write('target/doc/grafton_ndi/index.html', '<a href="struct.Foo.html">Foo</a>')
        self.write('target/doc/grafton_ndi/struct.Foo.html', '<section id="method.new"></section>')
        self.write('docs/usage.md', 'Use `Foo::removed()`.')
        output = io.StringIO()
        with mock.patch.object(docs, 'ROOT', self.root), \
                mock.patch.object(sys, 'argv', ['docs.py', '--rustdoc', str(self.root / 'target/doc')]), \
                contextlib.redirect_stderr(output):
            self.assertEqual(docs.main(), 1)
        self.assertIn('docs/usage.md:1: unknown documented API name: Foo::removed()', output.getvalue())

    def test_prose_name_check_distinguishes_qualified_owners(self):
        self.write('target/doc/grafton_ndi/index.html', '<a href="struct.Foo.html">Foo</a><a href="struct.Bar.html">Bar</a>')
        self.write('target/doc/grafton_ndi/struct.Foo.html', '<section id="method.capture_any"></section>')
        self.write('target/doc/grafton_ndi/struct.Bar.html', '<section id="method.new"></section>')
        self.write('README.md', '`Bar::capture_any()`')
        self.assertEqual(len(docs.guide_api_name_violations(self.root, self.root / 'target/doc')), 1)

    def test_prose_name_check_ignores_stale_unlinked_rustdoc_pages(self):
        self.write('target/doc/grafton_ndi/index.html', '<a href="nested/index.html">nested</a>')
        self.write('target/doc/grafton_ndi/nested/index.html', '<a href="struct.Current.html">Current</a>')
        self.write('target/doc/grafton_ndi/nested/struct.Current.html', '<section id="method.current_name"></section>')
        self.write('target/doc/grafton_ndi/struct.Removed.html', '<section id="method.removed_name"></section>')
        self.write('README.md', '`Current::current_name()` `Removed::removed_name()` `removed_name`')
        errors = docs.guide_api_name_violations(self.root, self.root / 'target/doc')
        self.assertEqual(len(errors), 2)
        self.assertIn('Removed::removed_name()', errors[0])
        self.assertIn('removed_name', errors[1])

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

    def test_native_alias_coverage_follows_every_registry_and_new_files(self):
        forms = ['native_table', 'optional_tables', 'advanced_tables', 'file_families']
        for index, form in enumerate(forms):
            self.write(f'src/sys/new_registry_{index}.rs',
                       f'{form}! {{ struct Table {{ fn NDIlib_call_{index}(); }} }}')
        self.write('src/sys/api.rs', '// fn NDIlib_comment();\n'
                   '/* fn NDIlib_block(); /* nested */ */\n'
                   'const EXAMPLE: &str = r#"native_table! { fn NDIlib_string(); }"#;\n'
                   'fn NDIlib_unregistered() {}')
        errors = docs.sdk_alias_violations(self.root)
        self.assertEqual(len(errors), 4)
        for index in range(4):
            self.assertIn(f'NDIlib_call_{index}', errors[index])
        self.write('src/public.rs', '\n'.join(
            f'#[doc(alias = "NDIlib_call_{index}")]\npub fn call_{index}() {{}}'
            for index in range(4)))
        self.assertEqual(docs.sdk_alias_violations(self.root), [])

    def test_alias_examples_comments_and_internal_code_cannot_cover_public_calls(self):
        self.write('src/sys/api.rs', 'native_table! { fn NDIlib_public(); }')
        self.write('src/sys/hidden.rs', '#[doc(alias = "NDIlib_public")] fn internal() {}')
        self.write('src/public.rs', '// #[doc(alias = "NDIlib_public")]\n'
                   '/* #[doc(alias = "NDIlib_public")] */\n'
                   'const EXAMPLE: &str = r##"#[doc(alias = "NDIlib_public")]"##;\n'
                   'const OTHER: &str = "#[doc(alias = \\"NDIlib_public\\")]";')
        self.write('src/example_tests.rs', '#[doc(alias = "NDIlib_public")] pub fn test_only() {}')
        self.assertEqual(len(docs.sdk_alias_violations(self.root)), 1)
        for declaration in ('fn private() {}', 'pub(crate) fn internal() {}', 'pub struct NotAFunction;'):
            with self.subTest(declaration=declaration):
                self.write('src/public.rs', '#[doc(alias = "NDIlib_public")]\n' + declaration)
                self.assertEqual(len(docs.sdk_alias_violations(self.root)), 1)
        self.write('src/public.rs', '#[cfg(test)] mod tests { '
                   '#[doc(alias = "NDIlib_public")] pub fn only_in_tests() {} }')
        self.assertEqual(len(docs.sdk_alias_violations(self.root)), 1)
        self.write('src/public.rs', '#[doc(alias = "NDIlib_public")]\n'
                   '#[must_use]\npub fn public() {}')
        self.assertEqual(docs.sdk_alias_violations(self.root), [])

    def test_macro_aliases_must_be_forwarded_to_the_matching_public_method(self):
        self.write('src/sys/api.rs', 'native_table! { fn NDIlib_public(); }')
        template = ('macro_rules! methods { '
                    '($sample:ty, $(#[$attr:meta])* $method:ident) => { '
                    '%s }; }\n'
                    'methods!(u8, #[doc(alias = "NDIlib_public")] generated);')
        for expansion in ('fn $method() {}', '$(#[$attr])* fn $method() {}',
                          '$(#[$other])* pub fn $method() {}',
                          '$(#[$attr])* pub fn $other() {}'):
            with self.subTest(expansion=expansion):
                self.write('src/public.rs', template % expansion)
                self.assertEqual(len(docs.sdk_alias_violations(self.root)), 1)
        self.write('src/public.rs', template % '$(#[$attr])* pub fn $method() {}')
        self.assertEqual(docs.sdk_alias_violations(self.root), [])
        self.write('src/public.rs', 'macro_rules! literal { () => { '
                   '#[doc(alias = "NDIlib_public")] pub fn generated() {} }; }')
        self.assertEqual(docs.sdk_alias_violations(self.root), [])

    def test_internal_native_entry_points_have_explicit_reasons(self):
        self.assertTrue(self.internal_entry_points)
        self.assertTrue(all(reason.strip() for reason in self.internal_entry_points.values()))
        docs.INTERNAL_SDK_ENTRY_POINTS['NDIlib_recv_free_video_v2'] = self.internal_entry_points['NDIlib_recv_free_video_v2']
        self.write('src/sys/api.rs', 'native_table! { fn NDIlib_recv_free_video_v2(); '
                   'fn NDIlib_recv_free_new_resource(); }')
        errors = docs.sdk_alias_violations(self.root)
        self.assertEqual(len(errors), 1)
        self.assertIn('NDIlib_recv_free_new_resource', errors[0])

    def test_public_alias_must_remove_its_internal_exemption(self):
        docs.INTERNAL_SDK_ENTRY_POINTS['NDIlib_example'] = 'fixture internal call'
        self.write('src/sys/api.rs', 'native_table! { fn NDIlib_example(); }')
        self.write('src/public.rs', '\n#[doc(alias = "NDIlib_example")]\npub fn example() {}')
        errors = docs.sdk_alias_violations(self.root)
        self.assertEqual(len(errors), 1)
        self.assertIn('src/public.rs:2:', errors[0])
        self.assertIn('is aliased publicly; remove its internal exemption', errors[0])

    def test_unregistered_internal_exemption_is_reported(self):
        docs.INTERNAL_SDK_ENTRY_POINTS['NDIlib_removed'] = 'fixture removed call'
        errors = docs.sdk_alias_violations(self.root)
        self.assertEqual(len(errors), 1)
        self.assertIn('NDIlib_removed names no registered SDK entry point', errors[0])

    def test_generated_signatures_reject_underscored_parameters_only(self):
        self.write('target/doc/grafton_ndi/struct.Example.html',
                   '<pre class="rust item-decl"><code>pub struct Example;</code></pre>'
                   '<h4 class="code-header">pub fn new(<span>_options</span>: &amp;Options)</h4>'
                   '<h4 class="code-header">pub async fn capture(&amp;self,<wbr> _timeout: Duration)</h4>'
                   '<h4 class="code-header">fn fmt(&amp;self, __formatter: &amp;mut Formatter)</h4>'
                   '<h4 class="code-header">fn hash&lt;__H: Hasher&gt;(&amp;self, state: &amp;mut __H)</h4>'
                   '<pre class="rust rust-example-rendered">fn example(_ignored: u8) {}</pre>'
                   '<p>Text mentions _not_a_parameter: something.</p>')
        self.write('target/doc/dependency/fn.other.html',
                   '<pre class="rust item-decl">pub fn other(_foreign: u8)</pre>')
        errors = docs.parameter_name_violations(self.root, self.root / 'target/doc')
        self.assertEqual(len(errors), 2)
        self.assertIn('_options', errors[0])
        self.assertIn('_timeout', errors[1])
        self.write('target/doc/grafton_ndi/struct.Example.html',
                   '<h4 class="code-header">pub fn new(options: &amp;Options)</h4>')
        self.assertEqual(docs.parameter_name_violations(self.root, self.root / 'target/doc'), [])

    def test_public_trait_required_and_default_method_parameters_are_checked(self):
        self.write('target/doc/grafton_ndi/trait.Example.html',
                   '<pre class="rust item-decl"><code>pub trait Example { '
                   'fn required(_input: u8); fn defaulted(_other: u8) { ... } }</code></pre>'
                   '<h4 class="code-header">fn defaulted(_other: u8)</h4>')
        errors = docs.parameter_name_violations(self.root, self.root / 'target/doc')
        self.assertEqual(len(errors), 2)
        self.assertIn('_input', errors[0])
        self.assertIn('_other', errors[1])


if __name__ == '__main__':
    unittest.main()
