import hashlib
import io
from pathlib import Path
import tempfile
import unittest
import urllib.error

from fetch_hosted_model import ATTEMPTS, PAUSE_SECONDS, download


class DownloadTests(unittest.TestCase):
    def test_checks_every_downloaded_byte_and_refuses_existing_output(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary, 'source')
            source.write_bytes(b'pinned test fixture')
            pin = {'url': source.as_uri(), 'bytes': source.stat().st_size,
                   'sha256': hashlib.sha256(source.read_bytes()).hexdigest()}
            target = Path(temporary, 'download')
            download(pin, target)
            self.assertEqual(target.read_bytes(), source.read_bytes())
            with self.assertRaises(FileExistsError):
                download(pin, target)
            for field, value in [('bytes', pin['bytes'] - 1), ('bytes', pin['bytes'] + 1),
                                 ('sha256', '0' * 64)]:
                target.unlink(missing_ok=True)
                with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                    download(dict(pin, **{field: value}), target, sleep=lambda seconds: None)
                # A refused download leaves nothing behind.
                self.assertFalse(target.exists())


WHOLE = b'pinned test fixture'
PIN = {'url': 'https://fixtures.invalid/model.gguf', 'bytes': len(WHOLE),
       'sha256': hashlib.sha256(WHOLE).hexdigest()}


class Served:
    """An opener that serves each scripted reply in turn: bytes as the
    response's body, or an exception raised as the connection's failure."""

    def __init__(self, *replies):
        self.replies = list(replies)
        self.opened = 0

    def __call__(self, url, timeout):
        self.opened += 1
        reply = self.replies.pop(0)
        if isinstance(reply, Exception):
            raise reply
        return io.BytesIO(reply)


class RetryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.target = Path(self.temporary.name, 'download')
        self.paused = []

    def tearDown(self):
        self.temporary.cleanup()

    def fetch(self, served):
        download(PIN, self.target, opener=served, sleep=self.paused.append)

    def test_a_short_download_is_fetched_again_from_an_empty_file(self):
        served = Served(WHOLE[:5], WHOLE)
        self.fetch(served)
        self.assertEqual(self.target.read_bytes(), WHOLE)
        self.assertEqual((served.opened, self.paused), (2, [PAUSE_SECONDS]))

    def test_a_garbled_download_or_a_failed_connection_is_fetched_again(self):
        garbled = b'X' + WHOLE[1:]
        served = Served(garbled, urllib.error.URLError('connection reset'), WHOLE)
        self.fetch(served)
        self.assertEqual(self.target.read_bytes(), WHOLE)
        self.assertEqual(served.opened, 3)

    def test_one_never_right_is_refused_as_before_and_nothing_is_left(self):
        served = Served(*[WHOLE[:5]] * ATTEMPTS)
        with self.assertRaisesRegex(ValueError, 'does not match its pinned size and SHA-256'):
            self.fetch(served)
        self.assertEqual((served.opened, len(self.paused)), (ATTEMPTS, ATTEMPTS - 1))
        self.assertFalse(self.target.exists())

    def test_one_bigger_than_its_pin_is_refused_at_once(self):
        served = Served(WHOLE + b'more', WHOLE)
        with self.assertRaisesRegex(ValueError, 'exceeds pinned size'):
            self.fetch(served)
        self.assertEqual((served.opened, self.paused), (1, []))
        self.assertFalse(self.target.exists())

    def test_an_existing_output_is_refused_and_left_as_it_was(self):
        self.target.write_bytes(b'already here')
        served = Served(WHOLE)
        with self.assertRaises(FileExistsError):
            self.fetch(served)
        self.assertEqual((served.opened, self.target.read_bytes()), (0, b'already here'))

if __name__ == '__main__':
    unittest.main()
