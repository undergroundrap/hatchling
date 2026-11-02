from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

script = Path(__file__).resolve().parent / 'release-notes.sh'

CHANGELOG = '''# Changelog

Intro.

## [Unreleased]

### Added

- Not released yet.

## [1.2.0] - 2026-11-01

### Added

- One.

### Fixed

- Two.

## [1.1.0] - 2026-10-01

- Older.

[Unreleased]: https://example.com/compare/v1.2.0...HEAD
[1.2.0]: https://example.com/v1.2.0
'''


def notes(changelog, *args):
    with tempfile.TemporaryDirectory() as root:
        (Path(root) / 'scripts').mkdir()
        shutil.copy(script, Path(root) / 'scripts' / 'release-notes.sh')
        (Path(root) / 'CHANGELOG.md').write_text(changelog, encoding='utf-8')
        return subprocess.run(['bash', str(Path(root) / 'scripts' / 'release-notes.sh'), *args],
                              capture_output=True, text=True, timeout=10)


class ReleaseNotesTests(unittest.TestCase):
    def test_a_versions_notes_are_its_section_alone(self):
        result = notes(CHANGELOG, '1.2.0')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, '### Added\n\n- One.\n\n### Fixed\n\n- Two.\n')

    def test_the_last_section_stops_at_the_compare_links(self):
        result = notes(CHANGELOG, '1.1.0')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, '- Older.\n')

    def test_latest_is_the_newest_dated_version(self):
        result = notes(CHANGELOG, '--latest')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(result.stdout.startswith('### Added\n\n- One.'))

    def test_a_version_without_its_dated_section_is_refused(self):
        undated = CHANGELOG.replace('## [1.2.0] - 2026-11-01', '## [1.2.0]')
        empty = CHANGELOG.replace('- Older.\n', '')
        twice = CHANGELOG + '\n## [1.2.0] - 2026-11-02\n\n- Again.\n'
        for changelog, version, reason in (
            (CHANGELOG, '1.3.0', 'sections for 1.3.0'),
            (undated, '1.2.0', 'heading'),
            (empty, '1.1.0', 'empty'),
            (twice, '1.2.0', '2 sections'),
            # A dot in the version is a dot, not any character.
            (CHANGELOG.replace('[1.1.0]', '[1x1.0]'), '1.1.0', 'sections for 1.1.0'),
        ):
            with self.subTest(reason=reason):
                result = notes(changelog, version)
                self.assertEqual(result.returncode, 1)
                self.assertEqual(result.stdout, '')
                self.assertIn(reason, result.stderr)

    def test_unreleased_or_no_argument_is_a_usage_error(self):
        for args in ([], ['Unreleased'], ['v1.2.0']):
            with self.subTest(args=args):
                result = notes(CHANGELOG, *args)
                self.assertEqual(result.returncode, 64)
                self.assertIn('usage', result.stderr)


if __name__ == '__main__':
    unittest.main()
