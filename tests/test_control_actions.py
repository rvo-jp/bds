"""Privileged command failure injection. All systemctl/reboot calls are mocked."""
import fcntl
import json
import os
import subprocess
import sys
import unittest
import test_backup as backup


class ControlActionTests(unittest.TestCase):
    set_state = backup.BackupTests.set_state
    events = backup.BackupTests.events

    def setUp(self):
        backup.BackupTests.setUp(self)
        self.marker = self.root/'reboot-pending'
        with (self.root/'bds.conf').open('a') as f:
            f.write('CONTROL_NOTICE_SECONDS=60\nCONTROL_REBOOT_MARKER='+str(self.marker)+'\n')
        mock = self.bin/'mock'
        source = mock.read_text()
        source = source.replace("if name == 'id':", "if name == 'sleep':\n    record('sleep '+args[0]); sys.exit(0)\nif name == 'id':")
        source = source.replace("if name == 'timeout':", "if name == 'timeout':\n    record('fifo '+args[-2])\n    if mode=='notice-fail': sys.exit(124)")
        source = source.replace("    cmd = args[0]; d = state()", """    cmd = args[0]; d = state()
    if args == ['--no-block','reboot']:
        record('OS REBOOT')
        assert d['active']=='inactive', 'BDS MUST BE STOPPED'
        assert (root/'reboot-pending').exists(), 'MISSING MAINTENANCE GUARD'
        sys.exit(1 if mode=='reboot-rejected' else 0)
    if any(x.endswith('.timer') for x in args):
        record('timer '+' '.join(args)); sys.exit(0)
""")
        mock.write_text(source)
        (self.bin/'sleep').symlink_to(mock)

    def run_control(self, operation='restart', mode=''):
        return subprocess.run(['bash',str(self.script),'control-'+operation],
            env=dict(self.env,TEST_MODE=mode),capture_output=True,text=True,timeout=10)

    def test_restart_announces_60_and_10_seconds_before_normal_stop(self):
        result=self.run_control(); self.assertEqual(result.returncode,0,result.stdout+result.stderr)
        events=self.events()
        self.assertIn('fifo say Minecraftサーバーを60秒後',events)
        self.assertIn('fifo say Minecraftサーバーを10秒後',events)
        self.assertLess(events.index('sleep 50'),events.index('sleep 10'))
        self.assertLess(events.index('sleep 10'),events.index('systemctl stop bds.service'))
        self.assertLess(events.index('systemctl stop bds.service'),events.index('systemctl start bds.service'))
        self.assertNotIn('OS REBOOT',events)

    def test_stop_or_notice_failure_never_reboots_os(self):
        for mode in ('stop-fail','abnormal-stop','notice-fail','old-unit'):
            with self.subTest(mode=mode):
                self.set_state('active')
                result=self.run_control('reboot',mode)
                self.assertNotEqual(result.returncode,0,result.stdout+result.stderr)
                self.assertNotIn('OS REBOOT',self.events())
                self.assertFalse(self.marker.exists())

    def test_reboot_stops_bds_and_retains_guard_until_boot(self):
        result=self.run_control('reboot'); self.assertEqual(result.returncode,0,result.stdout+result.stderr)
        events=self.events()
        self.assertLess(events.index('timer stop'),events.index('systemctl stop bds.service'))
        self.assertLess(events.index('systemctl stop bds.service'),events.index('OS REBOOT'))
        self.assertNotIn('systemctl start bds.service',events)
        self.assertTrue(self.marker.exists())
        self.assertEqual(json.loads((self.root/'state').read_text())['active'],'inactive')

    def test_rejected_reboot_restores_running_server_and_timers(self):
        result=self.run_control('reboot','reboot-rejected')
        self.assertNotEqual(result.returncode,0,result.stdout+result.stderr)
        self.assertFalse(self.marker.exists())
        self.assertIn('systemctl start bds.service',self.events())
        self.assertIn('timer start bds-update.timer',self.events())
        self.assertIn('timer start bds-backup.timer',self.events())

    def test_failed_start_is_not_reported_as_success(self):
        result=self.run_control('restart','start-fail')
        self.assertNotEqual(result.returncode,0)
        self.assertNotIn('再起動が完了しました',self.events())

    def test_stopped_server_can_be_started_without_sending_chat(self):
        self.set_state('inactive')
        result=self.run_control(); self.assertEqual(result.returncode,0,result.stdout+result.stderr)
        self.assertNotIn('fifo say',self.events())
        self.assertIn('systemctl start bds.service',self.events())

    def test_maintenance_contention_refuses_both_operations_before_notice(self):
        with (self.root/'.maintenance.lock').open('w') as lock:
            fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
            for operation in ('restart','reboot'):
                result=self.run_control(operation)
                self.assertNotEqual(result.returncode,0)
        self.assertNotIn('systemctl stop',self.events())
        self.assertNotIn('notify ',self.events())

    def test_pending_reboot_blocks_new_maintenance(self):
        self.marker.write_text('pending')
        for operation in ('restart','reboot'):
            result=self.run_control(operation)
            self.assertNotEqual(result.returncode,0)
        result=subprocess.run(['bash',str(self.script),'backup'],env=self.env,capture_output=True,text=True,timeout=10)
        self.assertNotEqual(result.returncode,0)
        self.assertEqual(self.events(),'')

    def test_notice_seconds_with_leading_zero_use_decimal(self):
        with (self.root/'bds.conf').open('a') as f: f.write('CONTROL_NOTICE_SECONDS=060\n')
        result=self.run_control(); self.assertEqual(result.returncode,0,result.stdout+result.stderr)
        self.assertIn('sleep 50',self.events())

    def test_restart_abnormal_stop_recovers_once_but_reports_failure(self):
        result = self.run_control('restart', 'abnormal-stop')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.events().count('systemctl start bds.service'), 1)
        self.assertEqual(json.loads((self.root/'state').read_text())['active'], 'active')
        self.assertNotIn('再起動が完了しました', self.events())

    def test_restart_stop_failure_with_live_process_does_not_start(self):
        result = self.run_control('restart', 'stop-fail')
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn('systemctl start bds.service', self.events())

    def test_failed_start_is_never_retried_by_cleanup(self):
        result = self.run_control('restart', 'start-fail')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.events().count('systemctl start bds.service'), 1)
