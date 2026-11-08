"""The connected-app proof refuses plausible but wrong room-disk evidence."""

import copy
import json
import contextlib
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import room_disk_app as proof


PHYSICAL = {'capacity': 10 * 1024**3, 'allocated': 3_400_000_000, 'mounted': True,
            'used': 1_200_000_000, 'free': 8_800_000_000}
SETUP = {'vault': {'tier': 'file'}, 'room_disk': {'state': 'open', 'off': None,
         'cap': PHYSICAL['capacity'], 'used': 1_200_000_000, 'free': 8_800_000_000, 'on_disk': 3_400_000_000}}
TEXT = 'Your agent’s computer\nIt holds up to 10.7 GB, 1.2 GB of it in use, and takes 3.4 GB on this machine.\nDeleting files inside it doesn’t make it smaller.\n' + proof.FILE_WARNING


class RoomDiskAppTests(unittest.TestCase):
    def test_only_a_setup_answer_is_read_and_other_frames_are_not_retained(self):
        self.assertEqual(proof.setup_from_frame(json.dumps({'type': 'action_result', 'result': {'result': 'setup', 'setup': SETUP}})), SETUP)
        for frame in ['bad json', '[]', 'null', '{"type":"welcome","token":"canary"}', '{"type":"action_result","result":{"result":"refused"}}']:
            self.assertIsNone(proof.setup_from_frame(frame))

    def test_a_mounted_file_is_measured_without_changing_it_and_links_are_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'mnt').mkdir()
            image = root / 'disk.img'
            image.write_bytes(b'not an encrypted fixture; snapshot only reads filesystem metadata')
            before = image.read_bytes()
            with patch('room_disk_app.os.path.ismount', return_value=True):
                result = proof.snapshot(root)
                self.assertEqual(result['capacity'], len(before))
                self.assertGreater(result['allocated'], 0)
            self.assertEqual(image.read_bytes(), before)
            with patch('room_disk_app.os.path.ismount', return_value=False), self.assertRaises(ValueError):
                proof.snapshot(root)
            other = root / 'other-file'
            other.write_bytes(b'must not be inspected through the image name')
            image.unlink()
            image.symlink_to(other)
            with patch('room_disk_app.os.path.ismount', return_value=True), self.assertRaises(ValueError):
                proof.snapshot(root)
            self.assertEqual(other.read_bytes(), b'must not be inspected through the image name')

    def test_real_open_counters_match_the_rendered_line(self):
        self.assertEqual(proof.validate_view(PHYSICAL, SETUP, TEXT)['used'], 1_200_000_000)
        small = copy.deepcopy(SETUP)
        small['room_disk']['used'] = 12_288_000
        self.assertEqual(proof.validate_view({**PHYSICAL, 'used': 12_288_000}, small, TEXT.replace('1.2 GB of it', '12 MB of it'))['used'], 12_288_000)

    def test_lazy_initialization_changes_are_bounded_by_independent_observations(self):
        before = {**PHYSICAL, 'allocated': 3_300_000_000, 'used': 1_100_000_000, 'free': 8_900_000_000}
        after = {**PHYSICAL, 'allocated': 3_500_000_000, 'used': 1_300_000_000, 'free': 8_700_000_000}
        proof.validate_view(before, SETUP, TEXT, after)
        for field, value in [('on_disk', 3_000_000_000), ('used', 900_000_000), ('free', 9_000_000_000)]:
            wrong = copy.deepcopy(SETUP)
            wrong['room_disk'][field] = value
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, 'independent filesystem'):
                proof.validate_view(before, wrong, TEXT, after)
        wrong = copy.deepcopy(SETUP)
        wrong['room_disk']['on_disk'] = 3_000_000_000
        with self.assertRaisesRegex(ValueError, 'independent filesystem'):
            proof.validate_view(PHYSICAL, wrong, TEXT.replace('takes 3.4 GB', 'takes 3.0 GB'))

    def test_a_heading_and_capacity_cannot_hide_an_off_mapped_or_missing_disk(self):
        for state in ['absent', 'closed', 'mapped', 'off']:
            setup = copy.deepcopy(SETUP)
            setup['room_disk']['state'] = state
            with self.subTest(state=state), self.assertRaises(ValueError):
                proof.validate_view(PHYSICAL, setup, TEXT)
        setup = copy.deepcopy(SETUP)
        setup['room_disk']['off'] = 'not_opened'
        with self.assertRaises(ValueError):
            proof.validate_view(PHYSICAL, setup, TEXT)

    def test_stale_fake_or_omitted_counters_fail(self):
        for field, value in [('cap', 100), ('used', None), ('used', True), ('free', 0), ('on_disk', 3_000_000_000)]:
            setup = copy.deepcopy(SETUP)
            setup['room_disk'][field] = value
            with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                proof.validate_view(PHYSICAL, setup, TEXT)
        with self.assertRaises(ValueError):
            proof.validate_view({**PHYSICAL, 'mounted': False}, SETUP, TEXT)

    def test_missing_limits_wrong_tier_or_failure_copy_fail(self):
        for text in [TEXT.replace(proof.FILE_WARNING, ''), TEXT.replace('Deleting files inside it doesn’t make it smaller.', ''), TEXT.replace('of it in use,', ''), TEXT + '\nIts storage is still unlocked', TEXT + '\nIt’s off']:
            with self.subTest(text=text), self.assertRaises(ValueError):
                proof.validate_view(PHYSICAL, SETUP, text)
        with self.assertRaises(ValueError):
            proof.validate_view(PHYSICAL, {**SETUP, 'vault': {'tier': 'hardware_key'}}, TEXT)

    def test_errors_do_not_print_a_bearer_address_from_a_browser_exception(self):
        bearer = 'http://127.0.0.1:7340/#hatchling-ui=' + 'a' * 64
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'physical.json'
            path.write_text(json.dumps(PHYSICAL))
            error = io.StringIO()
            output = io.StringIO()
            with patch('room_disk_app.subprocess.check_output', return_value=bearer), patch('room_disk_app.check', side_effect=RuntimeError(bearer)), contextlib.redirect_stderr(error), contextlib.redirect_stdout(output):
                self.assertEqual(proof.main(['check', str(path), directory]), 1)
            self.assertNotIn(bearer, error.getvalue() + output.getvalue())
            self.assertNotIn('a' * 64, error.getvalue() + output.getvalue())

    def test_the_second_snapshot_cannot_prompt_or_run_without_a_deadline(self):
        with patch('room_disk_app.subprocess.check_output', return_value=json.dumps(PHYSICAL)) as command:
            self.assertEqual(proof.snapshot_as_owner(), PHYSICAL)
        args, kwargs = command.call_args
        self.assertEqual(args[0][:2], ['sudo', '-n'])
        self.assertEqual(args[0][-1], 'snapshot')
        self.assertEqual(kwargs['timeout'], 30)


if __name__ == '__main__':
    unittest.main()
