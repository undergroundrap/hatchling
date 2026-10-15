import hashlib
from pathlib import Path
import tempfile
import unittest

from fetch_hosted_model import download


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
                target.unlink()
                with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                    download(dict(pin, **{field: value}), target)


if __name__ == '__main__':
    unittest.main()
