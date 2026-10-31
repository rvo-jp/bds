"""Failure-injection tests using real ZIP/tar extraction and filesystem swaps."""
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import unittest
import zipfile

import test_backup as backup

MOCK = backup.MOCK.replace('import fcntl, json, os, pathlib, signal, subprocess, sys, time',
                           'import fcntl, json, os, pathlib, shutil, signal, subprocess, sys, time')
a = MOCK.index("if name == 'jq':"); b = MOCK.index("if name == 'journalctl':", a)
MOCK = MOCK[:a] + r'''
if name == 'curl':
    if '--data-binary' in args:
        record('notify '+args[args.index('--data-binary')+1])
        pathlib.Path(args[args.index('--output')+1]).write_text('{}')
        pathlib.Path(args[args.index('--dump-header')+1]).write_text('')
        print('204',end=''); sys.exit(0)
    if '--output' in args:
        record('download')
        if mode=='download-fail': sys.exit(22)
        shutil.copyfile(root/'release.zip',args[args.index('--output')+1]); sys.exit(0)
    record('api')
    print(json.dumps({'result':{'links':[{'downloadType':'serverBedrockLinux',
          'downloadUrl':'https://example.invalid/bedrock-server-1.1.0.0.zip'}]}})); sys.exit(0)
if name == 'python3':
    if mode=='extract-fail' and args[:2]==['-','extract']:
        source=sys.stdin.read().replace('shutil.copyfileobj(src, dst)',
            "shutil.copyfileobj(src, dst); raise OSError('injected partial extraction failure')")
        sys.exit(subprocess.run([sys.executable]+args,input=source,text=True).returncode)
    os.execv(sys.executable,[sys.executable]+args)
if name == 'mv':
    if mode=='swap-fail' and args[-2].endswith('/candidate') and pathlib.Path(args[-1]).resolve()==(root/'bedrock-server').resolve():
        record('swap-failed'); sys.exit(1)
    sys.exit(subprocess.call([os.environ['REAL_MV']]+args))
''' + MOCK[b:]
a=MOCK.index("    if cmd == 'start':"); b=MOCK.index("if name == 'cp':",a)
MOCK=MOCK[:a]+r'''
    if cmd in ('start','restart'):
        d['active']='active'; d['invocation']='new-start'; d['starts']=d.get('starts',0)+1; save(d)
        fifo=root/'bedrock-server/.server.stdin'
        if not fifo.exists(): os.mkfifo(fifo)
        if mode=='candidate-start-fail' and d['starts']==1:
            (root/'bedrock-server/worlds/Test/db/data').write_text('converted by new server')
            sys.exit(1)
    sys.exit(0)
''' + MOCK[b:]

class DeploymentTests(unittest.TestCase):
    setUpBase=backup.BackupTests.setUp
    set_state=backup.BackupTests.set_state
    events=backup.BackupTests.events

    def setUp(self):
        self.setUpBase()
        (self.bin/'jq').unlink(missing_ok=True)  # Exercise the actual production API JSON parser.
        (self.bin/'mock').write_text('#!'+sys.executable+'\n'+MOCK)
        (self.bin/'mv').symlink_to(self.bin/'mock')
        (self.bin/'python3').symlink_to(self.bin/'mock')
        self.env['REAL_MV']=shutil.which('mv')
        with (self.root/'bds.conf').open('a') as f: f.write('UPDATE_NOTICE_SECONDS=0\n')
        (self.server/'behavior_packs/vanilla').mkdir()
        (self.server/'behavior_packs/vanilla/manifest.json').write_text('old default')
        self.release()

    def release(self,entries=None):
        with zipfile.ZipFile(self.root/'release.zip','w') as z:
            for name,content in (entries or {
                'bedrock_server':'new binary', 'server.properties':'server-name=Default\n',
                'behavior_packs/vanilla/manifest.json':'new default',
                'behavior_packs/new-default/manifest.json':'new pack',
                'permissions.json':'[]', 'libexample.so':'new library',
            }).items(): z.writestr(name,content)

    def run_script(self,command='update',*args,mode=''):
        return subprocess.run(['bash',str(self.script),command,*map(str,args)],
                              env=dict(self.env,TEST_MODE=mode),text=True,capture_output=True,timeout=15)

    def original_is_intact(self):
        self.assertEqual((self.server/'bedrock_server').read_text(),'original binary')
        self.assertEqual((self.server/'worlds/Test/db/data').read_text(),'snapshot')
        self.assertEqual((self.server/'.installed-version').read_text(),'1.0.0.0')
        self.assertEqual((self.server/'server.properties').read_text(),'server-name=Custom\n')

    def clean(self): self.assertEqual(list(self.root.glob('.bds-transaction.*')),[])

    def restored_archive(self,full=True,extra=None):
        archive=self.root/'restore.tar.gz'
        entries={'server/worlds/Test/db/data':'restored world','server/bedrock_server':'restored binary',
                 'server/server.properties':'server-name=Restored\n','server/.installed-version':'0.9.0.0',
                 'metadata.json':json.dumps({'format':'bds-full','schema':1,'bds_version':'0.9.0.0'})} if full else {'worlds/Test/db/data':'restored world'}
        if extra: entries.update(extra)
        with tarfile.open(archive,'w:gz') as t:
            dirs=set()
            for name,content in entries.items():
                for parent in reversed(Path(name).parents):
                    if str(parent)=='.' or str(parent) in dirs: continue
                    info=tarfile.TarInfo(str(parent)); info.type=tarfile.DIRTYPE; info.mode=0o755; t.addfile(info); dirs.add(str(parent))
                info=tarfile.TarInfo(name); data=content.encode(); info.size=len(data)
                info.mode=0o755 if name.endswith('bedrock_server') else 0o644
                t.addfile(info,io.BytesIO(data))
        return archive

    def test_update_prepares_before_stop_and_preserves_world_config_custom_packs(self):
        r=self.run_script(); self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertLess(self.events().index('download'),self.events().index('systemctl stop'))
        self.assertEqual((self.server/'bedrock_server').read_text(),'new binary')
        self.assertEqual((self.server/'worlds/Test/db/data').read_text(),'snapshot')
        self.assertEqual((self.server/'server.properties').read_text(),'server-name=Custom\n')
        self.assertTrue((self.server/'behavior_packs/custom/manifest.json').exists())
        self.assertEqual((self.server/'behavior_packs/vanilla/manifest.json').read_text(),'new default')
        self.assertTrue((self.server/'behavior_packs/new-default/manifest.json').exists())
        previous=list(self.root.glob('bedrock-server.pre-update-*')); self.assertEqual(len(previous),1)
        self.assertEqual((previous[0]/'bedrock_server').read_text(),'original binary'); self.clean()

    def test_download_failure_leaves_live_server_untouched(self):
        r=self.run_script(mode='download-fail'); self.assertNotEqual(r.returncode,0)
        self.assertNotIn('systemctl stop',self.events()); self.original_is_intact(); self.clean()

    def test_invalid_and_traversing_zip_fail_before_stop(self):
        for entries in ({'bedrock_server':'broken'}, {'bedrock_server':'x','server.properties':'x','../escape':'x'}):
            self.release(entries); r=self.run_script(); self.assertNotEqual(r.returncode,0,r.stdout+r.stderr)
            self.assertNotIn('systemctl stop',self.events()); self.original_is_intact(); self.clean()
        self.assertFalse((self.root/'escape').exists())

    def test_zip_corruption_fails_before_stop(self):
        (self.root/'release.zip').write_bytes(b'broken zip')
        r=self.run_script(); self.assertNotEqual(r.returncode,0)
        self.assertNotIn('systemctl stop',self.events()); self.original_is_intact(); self.clean()

    def test_start_failure_restores_original_binary_and_unconverted_world(self):
        r=self.run_script(mode='candidate-start-fail'); self.assertNotEqual(r.returncode,0,r.stdout+r.stderr)
        self.original_is_intact()
        self.assertEqual(json.loads((self.root/'state').read_text())['active'],'active')
        failed=list(self.root.glob('bedrock-server.failed-update-*')); self.assertEqual(len(failed),1)
        self.assertEqual((failed[0]/'worlds/Test/db/data').read_text(),'converted by new server')
        self.assertIn('元の実行ファイル',r.stderr); self.clean()

    def test_swap_failure_recovers_original_directory(self):
        r=self.run_script(mode='swap-fail'); self.assertNotEqual(r.returncode,0,r.stdout+r.stderr)
        self.original_is_intact(); self.assertEqual(json.loads((self.root/'state').read_text())['active'],'active'); self.clean()

    def test_copy_failure_restarts_original_server(self):
        r=self.run_script(mode='copy-fail'); self.assertNotEqual(r.returncode,0)
        self.original_is_intact(); self.assertIn('systemctl start',self.events()); self.clean()

    def test_absolute_runtime_link_cannot_modify_original_during_update(self):
        target=self.root/'outside-version'; target.write_text('1.0.0.0')
        (self.server/'.installed-version').unlink()
        (self.server/'.installed-version').symlink_to(target)
        r=self.run_script(); self.assertNotEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertEqual(target.read_text(),'1.0.0.0'); self.original_is_intact()
        self.assertIn('systemctl start',self.events()); self.clean()

    def test_stopped_server_remains_stopped_after_update(self):
        self.set_state('inactive'); r=self.run_script(); self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertNotIn('systemctl start',self.events()); self.assertNotIn('systemctl stop',self.events()); self.clean()

    def test_same_version_does_not_stop_or_download(self):
        (self.server/'.installed-version').write_text('1.1.0.0')
        r=self.run_script(); self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertNotIn('download',self.events()); self.assertNotIn('systemctl stop',self.events()); self.clean()

    def test_start_restart_and_configure_do_not_download_or_replace_runtime(self):
        for command in ('start','restart','configure'):
            r=self.run_script(command); self.assertEqual(r.returncode,0,r.stdout+r.stderr)
            self.original_is_intact()
        self.assertNotIn('api',self.events()); self.assertNotIn('download',self.events())

    def test_install_refuses_existing_server_without_network(self):
        r=self.run_script('install'); self.assertNotEqual(r.returncode,0)
        self.assertNotIn('api',self.events()); self.original_is_intact(); self.clean()

    def test_initial_install_needs_no_service_and_does_not_start(self):
        self.set_state('inactive'); shutil.rmtree(self.server)
        (self.root/'state').write_text(json.dumps({'active':'inactive','load':'not-found'}))
        r=self.run_script('install'); self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertTrue(os.access(self.server/'bedrock_server',os.X_OK))
        self.assertNotIn('systemctl start',self.events()); self.clean()

    def test_full_restore_replaces_binary_settings_and_world_together(self):
        r=self.run_script('restore',self.restored_archive()); self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertEqual((self.server/'worlds/Test/db/data').read_text(),'restored world')
        self.assertEqual((self.server/'bedrock_server').read_text(),'restored binary')
        self.assertEqual((self.server/'server.properties').read_text(),'server-name=Restored\n')
        self.assertEqual(len(list(self.root.glob('bedrock-server.pre-restore-*'))),1); self.clean()

    def test_worlds_archive_preserves_current_binary_and_settings(self):
        r=self.run_script('restore',self.restored_archive(False)); self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertEqual((self.server/'bedrock_server').read_text(),'original binary')
        self.assertEqual((self.server/'server.properties').read_text(),'server-name=Custom\n')
        self.assertEqual((self.server/'worlds/Test/db/data').read_text(),'restored world'); self.clean()

    def test_restore_start_failure_rolls_back_entire_runtime(self):
        r=self.run_script('restore',self.restored_archive(),mode='candidate-start-fail')
        self.assertNotEqual(r.returncode,0); self.original_is_intact()
        self.assertEqual(json.loads((self.root/'state').read_text())['active'],'active'); self.clean()

    def test_partial_extract_failure_does_not_stop_or_change_live_server(self):
        r=self.run_script('restore',self.restored_archive(),mode='extract-fail')
        self.assertNotEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertIn('injected partial extraction failure',r.stderr)
        self.assertNotIn('systemctl stop',self.events()); self.original_is_intact(); self.clean()

    def test_update_space_shortage_does_not_stop_live_server(self):
        r=self.run_script(mode='no-space'); self.assertNotEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertNotIn('systemctl stop',self.events()); self.original_is_intact(); self.clean()

    def test_truncated_backup_fails_before_server_stop(self):
        archive=self.restored_archive(); archive.write_bytes(archive.read_bytes()[:-8])
        r=self.run_script('restore',archive); self.assertNotEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertNotIn('systemctl stop',self.events()); self.original_is_intact(); self.clean()

    def test_unsafe_archive_path_fails_before_server_stop(self):
        r=self.run_script('restore',self.restored_archive(extra={'server/../../escape':'bad'}))
        self.assertNotEqual(r.returncode,0); self.assertNotIn('systemctl stop',self.events())
        self.original_is_intact(); self.assertFalse((self.root/'escape').exists()); self.clean()

    def test_archive_symlink_escape_fails_before_server_stop(self):
        archive=self.root/'malicious.tar.gz'
        with tarfile.open(archive,'w:gz') as t:
            d=tarfile.TarInfo('worlds'); d.type=tarfile.DIRTYPE; t.addfile(d)
            link=tarfile.TarInfo('worlds/escape'); link.type=tarfile.SYMTYPE; link.linkname='../../escape'; t.addfile(link)
        r=self.run_script('restore',archive); self.assertNotEqual(r.returncode,0)
        self.assertNotIn('systemctl stop',self.events()); self.original_is_intact(); self.clean()

    def test_backup_and_restore_round_trip(self):
        self.set_state('inactive')
        r=self.run_script('backup'); self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        archive=next(self.backups.glob('bds-server-*.tar.gz'))
        (self.server/'bedrock_server').write_text('modified binary')
        (self.server/'worlds/Test/db/data').write_text('modified world')
        r=self.run_script('restore',archive); self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.original_is_intact(); self.clean()

if __name__=='__main__': unittest.main()
