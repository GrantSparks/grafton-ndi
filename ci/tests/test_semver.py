import contextlib
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import semver  # noqa: E402


def index(*versions):
    """An index file: one JSON line per version; a trailing `!` marks a yanked one."""
    return ''.join(json.dumps({'name': 'some-crate', 'vers': version.rstrip('!'), 'yanked': version.endswith('!')})
                   + '\n' for version in versions)


class Response(io.BytesIO):
    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


class Semver(unittest.TestCase):
    def test_the_check_takes_no_options(self):
        self.assertEqual(semver.CHECK, ['cargo', 'semver-checks'])
        self.assertEqual(semver.INSTALL[:3], ['cargo', 'install', 'cargo-semver-checks'])
        self.assertIn('--locked', semver.INSTALL)
        self.assertRegex(semver.INSTALL[semver.INSTALL.index('--version') + 1], r'^\d+\.\d+\.\d+$')

    def test_index_location(self):
        self.assertEqual(semver.index_path('Some-Crate'), 'so/me/some-crate')

    def test_newest_release_ignores_yanked_versions_and_pre_releases(self):
        self.assertEqual(semver.newest_release(index('1.9.0', '1.10.0', '1.2.3')), (1, 10, 0))
        self.assertEqual(semver.newest_release(index('1.0.0', '2.0.0!', '2.0.0-rc.1', '1.0.1')), (1, 0, 1))
        self.assertIsNone(semver.newest_release(index('1.0.0!', '2.0.0-beta.1')))
        self.assertIsNone(semver.newest_release(''))

    def test_only_a_new_major_version_has_nothing_to_stay_compatible_with(self):
        for version, newest, expected in (('3.0.0', (2, 9, 9), False), ('3.0.0', None, False),
                                          ('3.0.0', (3, 0, 0), True), ('3.1.0', (3, 0, 5), True),
                                          ('3.0.1-rc.1', (3, 0, 0), True), ('2.5.0', (3, 0, 0), True)):
            with self.subTest(version=version, newest=newest):
                self.assertEqual(semver.applies(version, newest), expected)

    def run_main(self, version, published, statuses=(0, 0)):
        """Run the script for a crate at `version`; return its status, output, the commands run and the URL read."""
        commands, requested = [], []

        def run(command):
            commands.append(command)
            return mock.Mock(returncode=statuses[len(commands) - 1])

        def urlopen(url, timeout):
            requested.append(url)
            return Response(index(*published).encode())

        with tempfile.TemporaryDirectory() as directory, contextlib.redirect_stdout(io.StringIO()) as output:
            manifest = Path(directory) / 'Cargo.toml'
            manifest.write_text(f'[package]\nname = "some-crate"\nversion = "{version}"\n')
            with mock.patch.object(semver, 'MANIFEST', manifest), \
                    mock.patch.object(semver.subprocess, 'run', run), \
                    mock.patch.object(semver.urllib.request, 'urlopen', urlopen):
                status = semver.main()
        return status, output.getvalue(), commands, requested

    def test_a_new_major_version_is_skipped_and_says_so(self):
        status, output, commands, requested = self.run_main('8.0.0', ['7.3.0', '7.4.0'])
        self.assertEqual((status, commands), (0, []))
        self.assertEqual(requested, ['https://index.crates.io/so/me/some-crate'])
        self.assertEqual(output, 'semver check skipped: 8.0.0 is a new major version (the newest release is 7.4.0), '
                                 'so every API change is permitted\n')
        self.assertIn('nothing is published', self.run_main('8.0.0', [])[1])

    def test_otherwise_the_tool_is_installed_and_run(self):
        self.assertEqual(self.run_main('7.5.0', ['7.3.0', '7.4.0'])[::2], (0, [semver.INSTALL, semver.CHECK]))

    def test_a_failure_is_the_result(self):
        self.assertEqual(self.run_main('7.5.0', ['7.4.0'], statuses=(0, 100))[::2], (100, [semver.INSTALL, semver.CHECK]))
        self.assertEqual(self.run_main('7.5.0', ['7.4.0'], statuses=(101, 0))[::2], (101, [semver.INSTALL]))


if __name__ == '__main__':
    unittest.main()
