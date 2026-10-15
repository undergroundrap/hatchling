from pathlib import Path
import hashlib
import tempfile
import unittest

from hosted_model_support import checked_file, readonly_mount, sanitize_logs, snapshot


class SnapshotTests(unittest.TestCase):
    def test_complete_snapshot_rejects_environment_file_and_symlink_exposure(self):
        key = b'fixture-only-credential'
        with tempfile.TemporaryDirectory() as directory:
            proc = Path(directory)
            root = proc / 'root'
            root.mkdir()
            (root / 'agent').write_bytes(b'agent program fixture')
            (root / 'library').write_bytes(b'library fixture')
            (root / 'endpoint').write_bytes(b'endpoint fixture')
            (proc / 'environ').write_bytes(b'HATCHLING_CELL_ID=run-fixture\0')
            (proc / 'cmdline').write_bytes(b'/agent\0run\0task\0')
            (proc / 'mountinfo').write_text('1 0 0:1 / / ro - tmpfs tmpfs ro\n')
            digest = hashlib.sha256((root / 'agent').read_bytes()).hexdigest()
            result = snapshot(proc, key, '/agent', digest, [Path('/vault')])
            self.assertEqual(len(result['files']), 3)
            (proc / 'environ').write_bytes(b'KEY=' + key + b'\0')
            with self.assertRaisesRegex(ValueError, 'environment or argv'):
                snapshot(proc, key, '/agent', digest, [])
            (proc / 'environ').write_bytes(b'')
            (root / 'library').write_bytes(key)
            with self.assertRaisesRegex(ValueError, 'cell-view file'):
                snapshot(proc, key, '/agent', digest, [])
            (root / 'library').write_bytes(b'clean')
            (root / 'link').symlink_to('library')
            with self.assertRaisesRegex(ValueError, 'file link'):
                snapshot(proc, key, '/agent', digest, [])

    def test_checks_all_bytes_and_boundary_spanning_credentials(self):
        key = b'fixture-credential'
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'file'
            for offset in (0, 3, 16, 31, 5000):
                path.write_bytes(b'x' * offset + key + b'z' * 17)
                with self.subTest(offset=offset), self.assertRaises(ValueError):
                    checked_file(path, key, chunk_size=16)
            data = b'clean' * 1000
            path.write_bytes(data)
            self.assertEqual(checked_file(path, key, 16),
                             {'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest()})
            with self.assertRaises(FileNotFoundError):
                checked_file(Path(directory) / 'missing', key)

    def test_writable_submount_cannot_hide_under_readonly_root(self):
        root = '1 0 0:1 / / ro - tmpfs tmpfs ro\n'
        readonly_mount('agent', root)
        with self.assertRaises(ValueError):
            readonly_mount('lib/program', root + '2 1 0:2 / /lib rw - tmpfs tmpfs rw\n')
        with self.assertRaises(ValueError):
            readonly_mount('agent', root + '4 1 0:3 / / rw - tmpfs tmpfs rw\n')
        readonly_mount('a space/file', root + '3 1 0:3 / /a\\040space ro - tmpfs tmpfs ro\n')

    def test_redaction_preserves_failure_and_labels_sanitized_logs(self):
        key = b'only-a-generated-fixture-key'
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            log = root / 'model.log'
            log.write_bytes(b'before ' + key + b' after\n')
            with self.assertRaises(ValueError):
                sanitize_logs(root, key)
            self.assertNotIn(key, log.read_bytes())
            self.assertIn(b'<redacted-test-credential>', log.read_bytes())
            self.assertIn('FAILED', (root / 'SANITIZED.txt').read_text())


if __name__ == '__main__':
    unittest.main()
