"""Regression coverage for review findings, failed recovery and operational status."""
import json
import shlex
import tarfile
import unittest
import test_backup as backup
import test_deployment as deployment

class SafetyAndStatusTests(unittest.TestCase):
    setUpBase = backup.BackupTests.setUp
    set_state = backup.BackupTests.set_state
    events = backup.BackupTests.events
    release = deployment.DeploymentTests.release
    run_script = deployment.DeploymentTests.run_script
    original_is_intact = deployment.DeploymentTests.original_is_intact
    restored_archive = deployment.DeploymentTests.restored_archive
    clean = deployment.DeploymentTests.clean

    def setUp(self):
        deployment.DeploymentTests.setUp(self)
        mock = self.bin/'mock'
        source = mock.read_text()
        source = source.replace("if mode == 'stop-fail': sys.exit(1)",
            "if mode == 'stop-fail' or (mode=='rollback-stop-fail' and d.get('starts',0)>0): sys.exit(1)")
        source = source.replace("mode=='candidate-start-fail' and d['starts']==1",
            "mode in ('candidate-start-fail','rollback-stop-fail','rollback-mv-fail') and d['starts']==1")
        source = source.replace("if name == 'mv':", "if name == 'mv':\n    if mode=='rollback-mv-fail' and '.pre-update-' in args[-2]: sys.exit(1)")
        source = source.replace("        values = {'ActiveState':", "        if prop=='Type': d['ready_seen']=True; save(d)\n        values = {'ActiveState':")
        source = source.replace("        values = {'ActiveState':", """        if prop=='InvocationID' and d.get('ready_seen'):
            record('stable-check')
            if mode=='post-ready-crash' and d.get('starts')==1:
                d['invocation']='crash-restart'; d['active']='failed'; save(d)
        values = {'ActiveState':""")
        mock.write_text(source)

    def config(self,text):
        with (self.root/'bds.conf').open('a') as f: f.write(text+'\n')

    def test_worlds_only_chained_link_escape_is_rejected_before_stopping(self):
        (self.root/'outside/db').mkdir(parents=True)
        (self.root/'outside/db/data').write_text('outside')
        archive=self.root/'links.tar.gz'
        with tarfile.open(archive,'w:gz') as t:
            info=tarfile.TarInfo('worlds'); info.type=tarfile.DIRTYPE; info.mode=0o755; t.addfile(info)
            for name,target in [('worlds/a','.'),('worlds/Test','a/a/../../outside')]:
                info=tarfile.TarInfo(name); info.type=tarfile.SYMTYPE; info.linkname=target; t.addfile(info)
        r=self.run_script('restore',archive)
        self.assertNotEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertNotIn('systemctl stop',self.events()); self.original_is_intact(); self.clean()
        self.assertEqual((self.root/'outside/db/data').read_text(),'outside')

    def test_safe_relative_world_link_can_be_restored(self):
        self.set_state('inactive'); archive=self.root/'safe-link.tar.gz'
        with tarfile.open(archive,'w:gz') as t:
            t.add(self.server/'worlds',arcname='worlds')
            link=tarfile.TarInfo('worlds/alias'); link.type=tarfile.SYMTYPE; link.linkname='Test'; t.addfile(link)
        r=self.run_script('restore',archive); self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertEqual((self.server/'worlds/alias/db/data').read_text(),'snapshot')

    def test_disable_failure_preserves_service_definitions(self):
        r=self.run_script('configure'); self.assertEqual(r.returncode,0,r.stderr)
        units={p.name:p.read_bytes() for p in self.units.iterdir()}
        r=self.run_script('uninstall',mode='disable-fail'); self.assertNotEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertEqual(units,{p.name:p.read_bytes() for p in self.units.iterdir()})
        self.assertEqual(json.loads((self.root/'state').read_text())['active'],'active')
        self.assertNotIn('unit を削除しました',r.stdout)

    def test_disable_success_with_remaining_bds_preserves_definitions(self):
        r=self.run_script('configure'); self.assertEqual(r.returncode,0,r.stderr)
        r=self.run_script('uninstall',mode='disable-still-running'); self.assertNotEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertTrue((self.units/'bds.service').exists())

    def test_uninstall_after_complete_stop_removes_definitions(self):
        r=self.run_script('configure'); self.assertEqual(r.returncode,0,r.stderr)
        r=self.run_script('uninstall'); self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertFalse((self.units/'bds.service').exists())
        self.assertEqual(json.loads((self.root/'state').read_text())['active'],'inactive')
        self.assertTrue((self.server/'worlds/Test/db/data').exists())

    def test_trailing_slashes_are_normalized_before_update(self):
        self.config('BEDROCK_DIR='+shlex.quote(str(self.server)+'///'))
        r=self.run_script(); self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertEqual((self.server/'bedrock_server').read_text(),'new binary')
        self.assertEqual(len(list(self.root.glob('bedrock-server.pre-update-*'))),1)
        self.assertEqual(list(self.server.glob('.pre-*')),[]); self.clean()

    def test_dot_components_are_rejected_before_stopping(self):
        self.config('BEDROCK_DIR='+shlex.quote(str(self.server)+'/./'))
        r=self.run_script(); self.assertNotEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertNotIn('systemctl stop',self.events()); self.original_is_intact()

    def test_rollback_stop_failure_preserves_both_versions_and_staging(self):
        r=self.run_script(mode='rollback-stop-fail'); self.assertNotEqual(r.returncode,0,r.stdout+r.stderr)
        previous=list(self.root.glob('bedrock-server.pre-update-*')); self.assertEqual(len(previous),1)
        self.assertEqual((previous[0]/'worlds/Test/db/data').read_text(),'snapshot')
        self.assertEqual((self.server/'worlds/Test/db/data').read_text(),'converted by new server')
        self.assertEqual(len(list(self.root.glob('.bds-transaction.*'))),1)
        self.assertIn('復旧に失敗',self.events())
        self.assertEqual(self.events().count('systemctl start bds.service'),1)

    def test_rollback_move_failure_preserves_recovery_data_without_starting(self):
        r=self.run_script(mode='rollback-mv-fail'); self.assertNotEqual(r.returncode,0,r.stdout+r.stderr)
        previous=list(self.root.glob('bedrock-server.pre-update-*')); self.assertEqual(len(previous),1)
        self.assertEqual((previous[0]/'worlds/Test/db/data').read_text(),'snapshot')
        self.assertFalse(self.server.exists())
        self.assertEqual(len(list(self.root.glob('bedrock-server.failed-update-*'))),1)
        self.assertEqual(len(list(self.root.glob('.bds-transaction.*'))),1)
        self.assertEqual(self.events().count('systemctl start bds.service'),1)

    def test_crash_after_ready_log_rolls_back_update(self):
        self.config('SERVER_STABLE_SECONDS=2')
        r=self.run_script(mode='post-ready-crash'); self.assertNotEqual(r.returncode,0,r.stdout+r.stderr)
        self.original_is_intact(); self.assertIn('stable-check',self.events())
        self.assertEqual(json.loads((self.root/'state').read_text())['starts'],2); self.clean()

    def test_crash_after_ready_log_rolls_back_restore(self):
        self.config('SERVER_STABLE_SECONDS=2')
        r=self.run_script('restore',self.restored_archive(),mode='post-ready-crash')
        self.assertNotEqual(r.returncode,0,r.stdout+r.stderr); self.original_is_intact(); self.clean()

    def test_stability_window_can_exceed_startup_timeout(self):
        self.config('SERVER_START_TIMEOUT_SECONDS=2\nSERVER_STABLE_SECONDS=3')
        r=self.run_script(); self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertGreaterEqual(self.events().count('stable-check'),3); self.clean()

    def test_status_shows_sizes_counts_and_latest_completed_backup(self):
        for name in ['bedrock-server.pre-update-one','bedrock-server.failed-restore-two']:
            p=self.root/name; p.mkdir(); (p/'data').write_bytes(b'x'*8192)
        latest=self.backups/'bds-server-latest.tar.gz'; latest.write_bytes(b'x'*8192)
        (self.backups/'bds-server-later.tar.gz.partial').write_text('partial')
        (self.backups/'bds-server-link.tar.gz').symlink_to(latest)
        (self.root/'unrelated.pre-update-one').mkdir()
        r=self.run_script('status'); self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        for expected in ['サーバー:', 'バックアップ: 2件', '退避データ: 2件', 'bds-server-latest.tar.gz',
                         'ファイル更新日時:', 'バックアップ保存先の空き:', 'MiB']:
            self.assertIn(expected,r.stdout)
        self.assertNotIn('.partial',r.stdout)

    def test_stopped_status_preserves_systemctl_exit_code_and_shows_storage(self):
        self.set_state('inactive'); r=self.run_script('status')
        self.assertEqual(r.returncode,3,r.stdout+r.stderr); self.assertIn('運用データ',r.stdout)

    def test_nonserver_status_does_not_scan_storage(self):
        r=self.run_script('status','backup'); self.assertNotIn('運用データ',r.stdout)

if __name__=='__main__': unittest.main()
