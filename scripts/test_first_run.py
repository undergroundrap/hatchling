import importlib.util
from pathlib import Path
import sys
import subprocess
import unittest

examples = Path(__file__).resolve().parents[1] / 'docs/examples'
sys.path.insert(0, str(examples))
path = examples / 'first-run.py'
spec = importlib.util.spec_from_file_location('first_run', path)
example = importlib.util.module_from_spec(spec)
spec.loader.exec_module(example)

class DestinationTests(unittest.TestCase):
    def test_local_port_compatibility_and_hosted_grants(self):
        for value, expected in [('18089', 'localhost:18089'),
                                ('localhost:18090', 'localhost:18090'),
                                ('openrouter.ai:443', 'openrouter.ai:443'),
                                ('api.example.com:8443', 'api.example.com:8443')]:
            with self.subTest(value=value):
                self.assertEqual(example.parse_destination(value), expected)

    def test_refuses_ambiguous_or_out_of_range_destinations(self):
        for value in ('', ':443', 'api.example.com:', '0', '65536',
                      'api.example.com:not-a-port', 'a:b:443', '１２３'):
            with self.subTest(value=value), self.assertRaises(ValueError):
                example.parse_destination(value)

    def test_invalid_destination_exits_usage_before_connecting(self):
        result = subprocess.run([sys.executable, str(path), '/no-such-broker.sock', 'api.example:0'],
                                capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 64)
        self.assertIn('port must be between', result.stderr)
        self.assertNotIn('Traceback', result.stderr)

if __name__ == '__main__':
    unittest.main()
