import contextlib
import io
from pathlib import Path
import sys
import tempfile
import unittest

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


if __name__ == '__main__':
    unittest.main()
