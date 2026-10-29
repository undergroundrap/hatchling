import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
import unittest
from unittest.mock import Mock, patch

import hosted_model_smoke as proof


class InspectTests(unittest.TestCase):
    def test_an_unreadable_process_that_is_not_the_agent_is_skipped(self):
        stop = threading.Event()
        observations, errors = [], []
        process = Mock()
        process.name = '4242'

        def readlink(path):
            stop.set()
            raise PermissionError(13, 'Permission denied', str(path))

        with patch.object(proof.Path, 'iterdir', return_value=[process]),              patch.object(proof.os, 'open', return_value=99),              patch.object(proof.os, 'close') as close,              patch.object(proof.os, 'readlink', side_effect=readlink):
            proof.inspect_agent(1000, b'fixture-key', '/agent', 'hash', 'endpoint', 'model', [], 'task',
                                stop, observations, errors, Mock())
        self.assertEqual(errors, [])
        self.assertEqual(observations, [])
        close.assert_called_once_with(99)


class CleanupTests(unittest.TestCase):
    def test_sanitation_io_error_and_failed_unlink_cannot_publish_unchecked_logs(self):
        with tempfile.TemporaryDirectory() as directory:
            pending = Path(directory, 'private')
            artifacts = Path(directory, 'upload')
            pending.mkdir()
            artifacts.mkdir()
            (pending / 'broker.log').write_bytes(b'fixture-key')
            with patch.object(proof, 'reap_namespace'), \
                 patch.object(proof, 'sanitize_logs', side_effect=OSError('fixture read error')):
                with self.assertRaisesRegex(ValueError, 'could not complete'):
                    proof.finish_logs(pending, artifacts, b'fixture-key', [], [], None, None, threading.Event())
            self.assertEqual([p.name for p in artifacts.iterdir()], ['WITHHELD.txt'])
            with patch.object(proof, 'reap_namespace', side_effect=OSError), \
                 patch.object(Path, 'unlink', side_effect=PermissionError):
                with self.assertRaises(PermissionError):
                    proof.finish_logs(pending, artifacts, b'fixture-key', [], [], None, None, threading.Event())
            self.assertEqual([p.name for p in artifacts.iterdir()], ['WITHHELD.txt'])

    def test_early_cleanup_failure_still_reaps_and_withholds_logs(self):
        with tempfile.TemporaryDirectory() as directory:
            artifacts = Path(directory)
            (artifacts / 'broker.log').write_bytes(b'not yet scanned')
            child = Mock()
            child.poll.return_value = None
            writer = Mock()
            observer = Mock()
            observer.is_alive.return_value = False
            with patch.object(proof.signal, 'pidfd_send_signal', side_effect=PermissionError), \
                 patch.object(proof.os, 'close') as close, \
                 patch.object(proof, 'reap_namespace') as reap, \
                 patch.object(proof, 'sanitize_logs') as sanitize:
                with self.assertRaisesRegex(ValueError, 'logs withheld'):
                    proof.finalize(artifacts, b'fixture-key', [child], [writer], observer, 42, threading.Event())
                child.terminate.assert_called_once()
                child.wait.assert_called_once()
                reap.assert_called_once()
                close.assert_called_once_with(42)
                writer.close.assert_called_once()
                sanitize.assert_not_called()
            self.assertFalse((artifacts / 'broker.log').exists())
            self.assertIn('resume model', (artifacts / 'WITHHELD.txt').read_text())

    def test_live_observer_withholds_logs_and_successful_cleanup_cannot_hide_a_leak(self):
        with tempfile.TemporaryDirectory() as directory:
            artifacts = Path(directory)
            key = b'fixture-only-test-key'
            observer = Mock()
            observer.is_alive.return_value = True
            (artifacts / 'broker.log').write_bytes(key)
            with patch.object(proof, 'reap_namespace') as reap:
                with self.assertRaisesRegex(ValueError, 'logs withheld'):
                    proof.finalize(artifacts, key, [], [], observer, None, threading.Event())
                reap.assert_called_once()
            self.assertFalse((artifacts / 'broker.log').exists())
            (artifacts / 'broker.log').write_bytes(key)
            observer.is_alive.return_value = False
            with patch.object(proof, 'reap_namespace'):
                with self.assertRaises(proof.CredentialLeak):
                    proof.finalize(artifacts, key, [], [], observer, None, threading.Event())
            self.assertNotIn(key, (artifacts / 'broker.log').read_bytes())
            self.assertIn('FAILED', (artifacts / 'SANITIZED.txt').read_text())


class ProcessTests(unittest.TestCase):
    def test_a_process_runs_as_its_user_alone(self):
        dropped = {'Uid': '\t994\t994\t994\t994', 'Gid': '\t993\t993\t993\t993', 'Groups': '\t',
                   'CapPrm': '\t0000000000000000', 'CapEff': '\t0000000000000000'}
        with patch.object(proof.pwd, 'getpwnam', return_value=Mock(pw_uid=994, pw_gid=993)) as getpwnam:
            self.assertTrue(proof.runs_as(dropped, 'hatchling-keeper'))
            getpwnam.assert_called_with('hatchling-keeper')
            for field, value in (('Uid', '\t994\t0\t994\t994'), ('Gid', '\t993\t0\t993\t993'),
                                 ('Groups', '\t0'), ('CapPrm', '\t0000000000000400'),
                                 ('CapEff', '\t0000000000000001')):
                self.assertFalse(proof.runs_as(dict(dropped, **{field: value}), 'hatchling-keeper'), field)

    def test_only_a_childs_own_program_and_subcommand_are_found(self):
        program = shutil.which('sleep')
        child = subprocess.Popen([program, '30'])
        try:
            self.assertEqual(proof.children_running(os.getpid(), program, '30'), [child.pid])
            self.assertEqual(proof.children_running(os.getpid(), program, '31'), [])
            self.assertEqual(proof.children_running(os.getppid(), program, '30'), [])
        finally:
            child.kill()
            child.wait()


class CanaryTests(unittest.TestCase):
    def test_a_fetched_canary_is_told_from_a_leaked_or_unfetched_one(self):
        import https_model_fixture as fixture
        canary = 'hatchling-canary-fixture'
        def request(content):
            body = {'messages': [{'role': 'user', 'content': content}]}
            return body, json.dumps(body).encode()
        fetched = '<tool_result tool="vault_fetch" status="ok">{"secret":"[redacted secret]"}</tool_result>'
        self.assertEqual(fixture.canary_seen(*request(fetched), canary), 'fetched-redacted')
        leaked = fetched.replace('[redacted secret]', canary)
        self.assertEqual(fixture.canary_seen(*request(leaked), canary), 'plain')
        denied = '<tool_result tool="vault_fetch" status="error">denied</tool_result>'
        self.assertEqual(fixture.canary_seen(*request(denied), canary), 'not-fetched')
        self.assertEqual(fixture.canary_seen(*request('[redacted secret]'), canary), 'not-fetched')


if __name__ == '__main__':
    unittest.main()
