from pathlib import Path
import unittest
from unit_fixture import isolated_unit


class UnitFixture(unittest.TestCase):
    def test_private_directory_and_failure_hook(self):
        text = 'RuntimeDirectory="bds-test-fixture/runtime"\nRuntimeDirectoryMode=0750\nOnFailure=bds-alert@%n.service\n'
        result = isolated_unit(text, Path('/run/bds-test-fixture'))
        self.assertIn('RuntimeDirectory="bds-test-fixture/runtime"', result)
        self.assertNotIn('OnFailure=', result)

    def test_default_and_escaping_directories_are_rejected(self):
        for line in ('RuntimeDirectory=bds', ' RuntimeDirectory = bds',
                     'StateDirectory=bds-supervisor',
                     'RuntimeDirectory=../../var/lib/bds', 'RuntimeDirectory=/run/bds',
                     'RuntimeDirectory=bds-test-fixture/runtime:outside'):
            with self.subTest(line=line):
                with self.assertRaises(RuntimeError):
                    isolated_unit(line, Path('/run/bds-test-fixture'))
