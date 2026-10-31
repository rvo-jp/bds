import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('control_result', Path(__file__).resolve().parents[1]/'discord-control/result.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ResultTests(unittest.TestCase):
    def test_running_and_finished_share_invocation_and_preserve_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            environment = {'INVOCATION_ID': 'a'*32}
            module.write_result(directory, 'restart', 'running', environment, 'boot-a')
            path = Path(directory)/'result.json'
            self.assertEqual(json.loads(path.read_text())['phase'], 'running')
            environment.update(SERVICE_RESULT='exit-code', EXIT_CODE='exited', EXIT_STATUS='1')
            module.write_result(directory, 'restart', 'finished', environment, 'boot-a')
            record = json.loads(path.read_text())
            self.assertEqual(record['invocation_id'], 'a'*32)
            self.assertEqual(record['service_result'], 'exit-code')
            self.assertEqual(record['exit_status'], '1')
            self.assertEqual(path.stat().st_mode & 0o777, 0o644)
            self.assertEqual(len(list(Path(directory).iterdir())), 1)

    def test_missing_main_exit_status_does_not_turn_into_success(self):
        with tempfile.TemporaryDirectory() as directory:
            module.write_result(directory, 'restart', 'finished', {'INVOCATION_ID': 'a'*32}, 'boot-a')
            record = json.loads((Path(directory)/'result.json').read_text())
            self.assertEqual(record['service_result'], 'unknown')
            self.assertEqual(record['exit_status'], '')

    def test_missing_invocation_is_rejected_before_overwriting_result(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(ValueError):
                module.write_result(directory, 'restart', 'running', {}, 'boot-a')
            self.assertEqual(list(Path(directory).iterdir()), [])
