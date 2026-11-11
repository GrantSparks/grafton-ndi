"""Platform path and checkout settings cannot change an exact source identity."""
from pathlib import Path, PureWindowsPath
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from common import ROOT, suite_fingerprint


class SourceIdentityTests(unittest.TestCase):
    def source(self, root):
        root.mkdir()
        (root / 'ci').mkdir()
        (root / 'tools/sdk').mkdir(parents=True)
        (root / 'build_profiles.tsv').write_bytes(b'id\ttarget\nfixture\tfixture-target\n')
        (root / 'ci/probe.py').write_bytes(b'first_line = 1\nsecond_line = 2\n')
        (root / 'ci/README.md').write_bytes(b'Documentation also contributes to the suite identity.\n')
        (root / 'tools/sdk/probe.py').write_bytes(b'other_source = 3\n')
        return root

    def test_windows_path_serialization_matches_posix_source_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            root = self.source(Path(directory) / 'source')
            expected = suite_fingerprint(root)
            original = Path.relative_to
            def windows_relative(path, *args, **kwargs):
                return PureWindowsPath(original(path, *args, **kwargs).as_posix())
            def windows_order(left, right):
                return PureWindowsPath(left.as_posix()) < PureWindowsPath(right.as_posix())
            with patch.object(Path, 'relative_to', windows_relative), patch.object(Path, '__lt__', windows_order):
                self.assertEqual(suite_fingerprint(root), expected)
            (root / 'ci/probe.py').write_bytes(b'changed_source = 4\n')
            self.assertNotEqual(suite_fingerprint(root), expected)

    def test_autocrlf_checkout_preserves_reviewed_lf_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            source = self.source(directory / 'source')
            (source / '.gitattributes').write_bytes((ROOT / '.gitattributes').read_bytes())
            def git(*args, cwd=source):
                subprocess.run(['git', *args], cwd=cwd, check=True,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            git('init', '--quiet')
            git('-c', 'core.autocrlf=false', 'add', '.')
            git('-c', 'user.name=CI fixture', '-c', 'user.email=ci@example.invalid',
                'commit', '--quiet', '-m', 'Fixture source')
            checkout = directory / 'autocrlf-checkout'
            git('-c', 'core.autocrlf=true', 'clone', '--no-local', '--quiet', str(source), str(checkout))
            self.assertEqual((checkout / 'ci/probe.py').read_bytes(), (source / 'ci/probe.py').read_bytes())
            self.assertEqual(suite_fingerprint(checkout), suite_fingerprint(source))


if __name__ == '__main__':
    unittest.main()
