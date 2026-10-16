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

class UsageTests(unittest.TestCase):
    def test_the_task_is_one_line(self):
        self.assertTrue(example.TASK and '\n' not in example.TASK)

    def test_wrong_arguments_exit_usage_before_connecting(self):
        for args in ([], ['/no-such-broker.sock', '18089']):
            with self.subTest(args=args):
                result = subprocess.run([sys.executable, str(path), *args],
                                        capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode, 64)
                self.assertIn('Usage: python3 docs/examples/first-run.py', result.stderr)
                self.assertNotIn('Traceback', result.stderr)

if __name__ == '__main__':
    unittest.main()
