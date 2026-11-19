import contextlib
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import release  # noqa: E402

# Versions here are fixtures. The crate's own version is only ever read from its
# manifest, so these tests do not change when the crate is released.
MANIFEST = '[workspace]\nmembers = ["xtask"]\n\n[package]\nname = "x"\nversion = "{version}"\n'


class Release(unittest.TestCase):
    def test_tag_must_be_v_plus_the_crate_version(self):
        self.assertIsNone(release.mismatch('v7.8.9', '7.8.9'))
        for tag in ('7.8.9', 'v7.8', 'v7.8.9-rc.1', 'v7.8.10', 'v8.0.0', 'release-7.8.9', 'V7.8.9', ''):
            with self.subTest(tag=tag):
                self.assertIn('expected v7.8.9', release.mismatch(tag, '7.8.9'))
        self.assertIsNone(release.mismatch('v7.8.9-rc.1', '7.8.9-rc.1'))

    def test_version_is_read_from_the_package_table(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / 'Cargo.toml'
            manifest.write_text(MANIFEST.format(version='7.8.9'))
            self.assertEqual(release.crate_version(manifest), '7.8.9')

    def test_main_checks_the_tag_against_the_checked_in_manifest(self):
        version = release.crate_version()
        output = io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(release.main(['v' + version]), 0)
            self.assertEqual(release.main([version]), 1)
            self.assertEqual(release.main(['v' + version + '.1']), 1)
            self.assertEqual(release.main([]), 2)
        self.assertIn(f'::error::tag {version} does not release crate version {version}; expected v{version}',
                      output.getvalue())


class TaggedRelease(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.enterContext(contextlib.chdir(temporary.name))
        self.git('init', '--initial-branch=main')
        self.git('config', 'user.name', 'Release Test')
        self.git('config', 'user.email', 'release@example.invalid')
        self.git('config', 'commit.gpgsign', 'false')
        self.git('config', 'tag.gpgsign', 'false')
        self.git('config', 'core.hooksPath', temporary.name)
        self.commit_version('7.8.9')
        self.tagged = self.git('rev-parse', 'HEAD')
        self.git('tag', '-a', 'v7.8.9', '-m', 'Release')
        self.commit_version('7.9.0')
        self.git('update-ref', 'refs/remotes/origin/main', 'HEAD')

    def git(self, *arguments):
        return subprocess.check_output(['git', *arguments], text=True, stderr=subprocess.PIPE).strip()

    def commit_version(self, version):
        Path('Cargo.toml').write_text(MANIFEST.format(version=version), encoding='utf-8')
        self.git('add', 'Cargo.toml')
        self.git('commit', '-m', version)

    def test_retry_from_main_uses_the_existing_tag_and_its_manifest(self):
        self.assertEqual(release.release_commit('v7.8.9', 'workflow_dispatch', 'refs/heads/main'),
                         (self.tagged, '7.8.9'))
        with self.assertRaisesRegex(ValueError, 'retry it from main'):
            release.release_commit('v7.8.9', 'workflow_dispatch', 'refs/heads/staging')

    def test_tag_push_must_match_the_checked_out_commit(self):
        with self.assertRaisesRegex(ValueError, 'does not match the pushed commit'):
            release.release_commit('v7.8.9', 'push', 'refs/tags/v7.8.9')
        self.git('checkout', '--detach', self.tagged)
        self.assertEqual(release.release_commit('v7.8.9', 'push', 'refs/tags/v7.8.9'),
                         (self.tagged, '7.8.9'))

    def test_invalid_missing_and_mismatched_tags_are_rejected(self):
        self.git('tag', 'v7.8.0', self.tagged)
        for tag, reason in (
            ('refs/tags/v7.8.9', 'expected a v<version> release tag'),
            ('v7.8.9\ncommit=other', 'expected a v<version> release tag'),
            ('v7.8.10', 'does not exist'),
            ('v7.8.0', 'expected v7.8.9'),
        ):
            with self.subTest(tag=tag), self.assertRaisesRegex(ValueError, reason):
                release.release_commit(tag, 'workflow_dispatch', 'refs/heads/main')

    def test_tag_outside_main_is_rejected(self):
        self.git('switch', '-c', 'other')
        self.commit_version('7.10.0')
        self.git('tag', 'v7.10.0')
        with self.assertRaisesRegex(ValueError, 'points to a commit of main'):
            release.release_commit('v7.10.0', 'workflow_dispatch', 'refs/heads/main')

    def test_preparation_records_only_verified_reuse(self):
        output = Path('output')
        env = {
            'RELEASE_TAG': 'v7.8.9', 'GITHUB_EVENT_NAME': 'workflow_dispatch',
            'GITHUB_REF': 'refs/heads/main', 'GITHUB_OUTPUT': str(output),
        }
        for run in (None, 1234):
            with self.subTest(run=run), patch.object(release, 'find_run', return_value=run) as lookup, \
                    contextlib.redirect_stdout(io.StringIO()):
                output.write_text('')
                release.prepare(env)
                lookup.assert_called_once_with(env, self.tagged)
                lines = [f'commit={self.tagged}', 'version=7.8.9']
                if run:
                    lines.append(f'run_id={run}')
                self.assertEqual(output.read_text().splitlines(), lines)


if __name__ == '__main__':
    unittest.main()
