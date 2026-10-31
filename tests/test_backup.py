"""Offline backup integration tests; no real systemd, network, or BDS required."""
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import subprocess
import sys
import tarfile
import tempfile
import time
import threading
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'bds.sh'

MOCK = r'''
import fcntl, json, os, pathlib, signal, subprocess, sys, time
root = pathlib.Path(os.environ['TEST_ROOT'])
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
mode = os.environ.get('TEST_MODE', '')
def record(s):
    with (root/'events').open('a') as f: f.write(s+'\n')
def state(): return json.loads((root/'state').read_text())
def save(d): (root/'state').write_text(json.dumps(d))
if name == 'id':
    print('0' if args == ['-u'] else 'tester'); sys.exit(0)
if name == 'flock':
    limit = float(args[args.index('-w')+1]) if '-w' in args else 0
    deadline = time.monotonic()+limit
    waited = False
    while True:
        try:
            fcntl.flock(int(args[-1]), fcntl.LOCK_EX | fcntl.LOCK_NB)
            sys.exit(0)
        except BlockingIOError:
            if not waited: record('lock-wait'); waited=True
            if time.monotonic()>=deadline: sys.exit(1)
            time.sleep(.02)
if name == 'chown': sys.exit(0)
if name == 'timeout':
    try: sys.exit(subprocess.run(args[1:],timeout=float(args[0].rstrip('s'))).returncode)
    except subprocess.TimeoutExpired: sys.exit(124)
if name == 'jq':
    print(json.dumps({'content': args[args.index('--arg')+2]},ensure_ascii=False)); sys.exit(0)
if name == 'curl':
    record('notify '+args[args.index('--data-binary')+1])
    pathlib.Path(args[args.index('--output')+1]).write_text('{}')
    pathlib.Path(args[args.index('--dump-header')+1]).write_text('')
    print('204',end=''); sys.exit(0)
if name == 'journalctl':
    if mode != 'not-ready': print('[INFO] Server started.')
    sys.exit(0)
if name == 'systemctl':
    cmd = args[0]; d = state()
    if cmd == 'show':
        prop = next(x.split('=',1)[1] for x in args if x.startswith('--property='))
        values = {'ActiveState': d['active'], 'MainPID': '123' if d['active']=='active' else '0',
                  'Type': 'simple' if mode=='old-unit' else 'notify', 'LoadState': d.get('load','loaded'), 'ControlGroup': '', 'User': 'tester', 'Group': 'tester', 'Result': d.get('result','success'), 'ExecMainStatus': d.get('status','0'),
                  'InvocationID': d.get('invocation','test-start'),
                  'KillMode': 'control-group' if mode=='old-unit' else 'mixed',
                  'SendSIGKILL': 'yes' if mode=='old-unit' else 'no'}
        print(values[prop]); sys.exit(0)
    if cmd == 'is-active': sys.exit(0 if d['active']=='active' and mode!='not-ready' else 3)
    record('systemctl '+ ' '.join(args))
    if cmd == 'disable':
        if mode=='disable-fail': sys.exit(1)
        if args[-1]=='bds.service' and mode!='disable-still-running':
            d['active']='inactive'; save(d)
            fifo=root/'bedrock-server/.server.stdin'
            if fifo.exists(): fifo.unlink()
        sys.exit(0)
    if cmd == 'status': sys.exit(0 if d['active']=='active' else 3)
    if cmd == 'stop':
        if mode == 'stop-fail': sys.exit(1)
        d['active']='inactive'
        fifo=root/'bedrock-server/.server.stdin'
        if fifo.exists(): fifo.unlink()
        if mode == 'abnormal-stop': d['result']='exit-code'; d['status']='1'
        health=root/'health'; health.mkdir(exist_ok=True)
        (health/'status.json').write_text(json.dumps({'invocation_id':'stale-start' if mode=='stale-receipt' else d.get('invocation','test-start'),
            'ready':False,'failed':mode=='abnormal-stop','stopped':mode!='abnormal-stop',
            'child_exit_status':1 if mode=='abnormal-stop' else 0}))
        save(d); sys.exit(0)
    if cmd == 'start':
        if mode == 'not-ready':
            d['active']='failed'; save(d); sys.exit(1)
        if mode == 'start-fail': sys.exit(1)
        d['active']='active'; d['invocation']='new-start'; save(d)
        fifo=root/'bedrock-server/.server.stdin'
        if not fifo.exists(): os.mkfifo(fifo)
        # Change the live world to prove tar reads the offline snapshot.
        (root/'bedrock-server/worlds/Test/db/data').write_text('after restart')
    sys.exit(0)
if name == 'cp':
    record('copy')
    if state()['active'] != 'inactive': record('UNSAFE COPY'); sys.exit(20)
    if mode == 'copy-fail': sys.exit(1)
    if mode == 'interrupt':
        (root/'copy-waiting').touch()
        time.sleep(30)
    if mode == 'copy-growth':
        (root/'copy-waiting').touch()
    sys.exit(subprocess.call([os.environ['REAL_CP']]+args))
if name == 'tar':
    record('tar '+args[0])
    if args[0]=='-czf' and mode=='compress-fail':
        pathlib.Path(args[1]).write_text('incomplete'); sys.exit(2)
    if args[0]=='-czf' and mode=='verify-fail':
        import gzip
        pathlib.Path(args[1]).write_bytes(gzip.compress(b'not an archive')); sys.exit(0)
    if args[0]=='-czf' and mode not in ('start-fail','not-ready') and os.environ['INITIAL_ACTIVE']=='active':
        if state()['active']!='active': record('COMPRESSED WHILE STOPPED'); sys.exit(21)
    sys.exit(subprocess.call([os.environ['REAL_TAR']]+args))
if name == 'df':
    free=1 if mode=='no-space' else 999999999
    print('Filesystem 1024-blocks Used Available Capacity Mounted on')
    print(f'disk 9999999999 0 {free} 1% /'); sys.exit(0)
record('unexpected '+name+' '+str(args)); sys.exit(99)
'''

class BackupTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='bds-test-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.server = self.root/'bedrock-server'
        (self.server/'worlds/Test/db').mkdir(parents=True)
        (self.server/'worlds/Test/db/data').write_text('snapshot')
        (self.server/'bedrock_server').write_text('original binary'); (self.server/'bedrock_server').chmod(0o755)
        (self.server/'server.properties').write_text('server-name=Custom\n')
        (self.server/'.installed-version').write_text('1.0.0.0')
        (self.server/'behavior_packs/custom').mkdir(parents=True)
        (self.server/'behavior_packs/custom/manifest.json').write_text('{"custom":true}')
        self.backups = self.root/'backups'
        self.backups.mkdir()
        self.old = self.backups/'bds-worlds-old.tar.gz'
        self.old.write_text('old backup must survive failures')
        os.utime(self.old, (1,1))
        self.units = self.root/'units'; self.units.mkdir()
        self.script = self.root/'bds.sh'
        self.script.write_text(SCRIPT.read_text().replace('/run/$APP_NAME-health',str(self.root/'health')).replace('/var/lib/$APP_NAME-health',str(self.root/'diagnostics')).replace('SYSTEMD_DIR="/etc/systemd/system"',
                                                       'SYSTEMD_DIR='+shlex.quote(str(self.units))))
        shutil.copy2(SCRIPT.parent/'supervisor.py', self.root/'supervisor.py')
        self.bin = self.root/'bin'; self.bin.mkdir()
        mock = self.bin/'mock'; mock.write_text('#!'+sys.executable+'\n'+MOCK); mock.chmod(0o755)
        for name in ['id','flock','chown','curl','journalctl','systemctl','cp','tar','df','timeout']:
            (self.bin/name).symlink_to(mock)
        self.env = dict(os.environ, TEST_ROOT=str(self.root), INITIAL_ACTIVE='active',
                        REAL_CP=shutil.which('cp'), REAL_TAR=shutil.which('tar'),
                        PATH=str(self.bin)+':'+os.environ['PATH'], BDS_CONFIG=str(self.root/'bds.conf'))
        (self.root/'bds.conf').write_text('BACKUP_NOTICE_SECONDS=0\nSERVER_START_TIMEOUT_SECONDS=2\nSERVER_STABLE_SECONDS=0\nBACKUP_LOCK_WAIT_SECONDS=0\nDISCORD_WEBHOOK_URL=https://example.invalid/test\n')
        self.set_state('active')

    def set_state(self, active):
        (self.root/'state').write_text(json.dumps({'active': active}))
        self.env['INITIAL_ACTIVE'] = active
        fifo=self.server/'.server.stdin'
        if fifo.exists(): fifo.unlink()
        if active=='active':
            os.mkfifo(fifo)
            fd=os.open(fifo, os.O_RDWR | os.O_NONBLOCK)
            self.addCleanup(os.close,fd)

    def run_script(self, mode='', command='backup'):
        result=subprocess.run(['bash',str(self.script),command],env=dict(self.env,TEST_MODE=mode),
                              text=True,capture_output=True,timeout=10)
        return result

    def events(self):
        p=self.root/'events'
        return p.read_text() if p.exists() else ''

    def archives(self):
        return [p for p in self.backups.glob('bds-server-*.tar.gz') if p != self.old]

    def assert_clean(self):
        self.assertEqual(list(self.backups.glob('.bds-stage.*')),[])
        self.assertEqual(list(self.backups.glob('*.partial')),[])

    def test_running_server_restarts_before_compression_and_snapshot_is_consistent(self):
        r=self.run_script(); self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        events=self.events()
        self.assertLess(events.index('systemctl stop'),events.index('copy'))
        self.assertLess(events.index('copy'),events.index('systemctl start'))
        self.assertLess(events.index('systemctl start'),events.index('tar -czf'))
        self.assertEqual(len(self.archives()),1)
        with tarfile.open(self.archives()[0]) as tf:
            self.assertEqual(json.load(tf.extractfile('metadata.json'))['bds_version'],'1.0.0.0')
            self.assertEqual(tf.extractfile('server/bedrock_server').read(),b'original binary')
            self.assertIn('server/behavior_packs/custom/manifest.json',tf.getnames())
            self.assertEqual(tf.extractfile('server/worlds/Test/db/data').read(),b'snapshot')
        self.assertFalse(self.old.exists()); self.assert_clean()

    def test_stopped_server_stays_stopped(self):
        self.set_state('inactive'); r=self.run_script()
        self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertNotIn('systemctl start',self.events()); self.assertNotIn('systemctl stop',self.events())
        self.assertEqual(json.loads((self.root/'state').read_text())['active'],'inactive')
        self.assert_clean()

    def test_copy_failure_restores_server_and_preserves_previous_backups(self):
        r=self.run_script('copy-fail'); self.assertNotEqual(r.returncode,0)
        self.assertIn('systemctl start',self.events()); self.assertNotIn('tar -czf',self.events())
        self.assertTrue(self.old.exists()); self.assertEqual(self.archives(),[]); self.assert_clean()

    def test_stop_failure_does_not_copy(self):
        r=self.run_script('stop-fail'); self.assertNotEqual(r.returncode,0)
        self.assertNotIn('\ncopy\n',self.events()); self.assertTrue(self.old.exists()); self.assert_clean()

    def test_abnormal_shutdown_does_not_copy(self):
        r=self.run_script('abnormal-stop'); self.assertNotEqual(r.returncode,0)
        self.assertNotIn('\ncopy\n',self.events()); self.assertIn('systemctl start',self.events()); self.assert_clean()

    def test_compression_and_validation_failures_preserve_existing_backups(self):
        for mode in ['compress-fail','verify-fail']:
            with self.subTest(mode=mode):
                self.set_state('active')
                r=self.run_script(mode); self.assertNotEqual(r.returncode,0)
                self.assertTrue(self.old.exists()); self.assertEqual(self.archives(),[]); self.assert_clean()

    def test_restart_failure_still_saves_snapshot_and_reports_failure(self):
        r=self.run_script('start-fail'); self.assertNotEqual(r.returncode,0)
        self.assertEqual(len(self.archives()),1); self.assert_clean()
        self.assertIn('サーバー再起動',self.events())
        self.assertIn('バックアップは保存済み',self.events())
        self.assertNotIn('バックアップが完了しました',self.events())

    def test_notify_unit_without_ready_is_not_success(self):
        r=self.run_script('not-ready'); self.assertNotEqual(r.returncode,0)
        self.assertEqual(len(self.archives()),1); self.assert_clean()

    def test_previous_invocation_shutdown_receipt_cannot_authorize_copy(self):
        r=self.run_script('stale-receipt');self.assertNotEqual(r.returncode,0)
        self.assertNotIn('copy',self.events());self.assertEqual(self.archives(),[])

    def test_old_systemd_settings_abort_before_stopping(self):
        r=self.run_script('old-unit'); self.assertNotEqual(r.returncode,0)
        self.assertNotIn('systemctl stop',self.events()); self.assertIn('configure',self.events()); self.assert_clean()

    def test_no_space_does_not_stop_server_or_delete_backups(self):
        r=self.run_script('no-space'); self.assertNotEqual(r.returncode,0)
        self.assertNotIn('systemctl stop',self.events()); self.assertTrue(self.old.exists()); self.assert_clean()

    def test_transitional_state_is_rejected(self):
        self.set_state('activating'); r=self.run_script()
        self.assertNotEqual(r.returncode,0); self.assertNotIn('\ncopy\n',self.events())

    def test_unmanaged_server_fifo_is_rejected(self):
        self.set_state('inactive'); os.mkfifo(self.server/'.server.stdin')
        r=self.run_script(); self.assertNotEqual(r.returncode,0); self.assertNotIn('\ncopy\n',self.events())

    def test_backup_path_inside_world_is_rejected(self):
        with (self.root/'bds.conf').open('a') as f:
            f.write('BACKUP_DIR='+shlex.quote(str(self.server/'worlds/nested-backups'))+'\n')
        r=self.run_script(); self.assertNotEqual(r.returncode,0)
        self.assertNotIn('systemctl stop',self.events())

    def test_shared_lock_reports_backup_timeout_skips_update_and_rejects_manual_start(self):
        import fcntl
        with (self.root/'.maintenance.lock').open('w') as lock:
            fcntl.flock(lock,fcntl.LOCK_EX | fcntl.LOCK_NB)
            for command in ['backup','update','auto-update','start','stop','restart','restore','configure','uninstall']:
                r=self.run_script(command=command)
                self.assertEqual(r.returncode,0 if command in ['update','auto-update'] else 1,r.stdout+r.stderr)
                self.assertIn('別のメンテナンス',r.stderr)
        self.assertNotIn('systemctl ',self.events())
        self.assertIn('バックアップを開始できませんでした',self.events())

    def test_backup_waits_for_lock_then_completes(self):
        import fcntl
        with (self.root/'bds.conf').open('a') as f: f.write('BACKUP_LOCK_WAIT_SECONDS=3\n')
        with (self.root/'.maintenance.lock').open('w') as lock:
            fcntl.flock(lock,fcntl.LOCK_EX | fcntl.LOCK_NB)
            timer=threading.Timer(.7,lambda: fcntl.flock(lock,fcntl.LOCK_UN)); timer.start()
            try: r=self.run_script()
            finally: timer.join()
        self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertIn('lock-wait',self.events()); self.assertEqual(len(self.archives()),1)

    def test_backup_directory_inside_runtime_is_rejected(self):
        with (self.root/'bds.conf').open('a') as f:
            f.write('BACKUP_DIR='+shlex.quote(str(self.server/'backups'))+'\n')
        r=self.run_script(); self.assertNotEqual(r.returncode,0)
        self.assertNotIn('systemctl stop',self.events())

    def test_termination_during_copy_restarts_server(self):
        p=subprocess.Popen(['bash',str(self.script),'backup'],env=dict(self.env,TEST_MODE='interrupt'),
                           stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,start_new_session=True)
        try:
            deadline=time.monotonic()+10
            while not (self.root/'copy-waiting').exists() and time.monotonic()<deadline:
                time.sleep(.02)
            self.assertTrue((self.root/'copy-waiting').exists())
            other=self.run_script(command='update')
            self.assertEqual(other.returncode,0)
            self.assertIn('別のメンテナンス',other.stderr)
            os.killpg(p.pid,signal.SIGTERM)
            out,err=p.communicate(timeout=10)
            self.assertNotEqual(p.returncode,0,out+err)
            self.assertIn('systemctl start',self.events()); self.assertTrue(self.old.exists()); self.assert_clean()
        finally:
            if p.poll() is None: os.killpg(p.pid,signal.SIGKILL); p.wait()

    def test_server_wrapper_finishes_graceful_shutdown_when_main_receives_term(self):
        executable=self.server/'bedrock_server'
        executable.write_text('#!/bin/bash\necho "Server started."\nwhile IFS= read -r line; do\n'
                              '  if [[ "$line" == stop ]]; then sleep 0.3; echo "Saved"; exit 0; fi\ndone\n')
        executable.chmod(0o755)
        p=subprocess.Popen(['bash',str(self.script),'run'],env=self.env,
                           stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,start_new_session=True)
        try:
            self.assertEqual(p.stdout.readline().strip(),'Server started.')
            r=self.run_script(command='send-stop'); self.assertEqual(r.returncode,0)
            p.send_signal(signal.SIGTERM)
            out,err=p.communicate(timeout=5)
            self.assertEqual(p.returncode,0,out+err)
            self.assertIn('Saved',out)
            self.assertFalse((self.server/'.server.stdin').exists())
        finally:
            if p.poll() is None: os.killpg(p.pid,signal.SIGKILL); p.wait()

    def test_configure_changes_units_without_download_or_server_restart(self):
        r=self.run_script(command='configure'); self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        unit=(self.units/'bds.service').read_text()
        self.assertIn('KillMode=mixed',unit); self.assertIn('SendSIGKILL=no',unit)
        self.assertNotIn('After=bds.service',(self.units/'bds-backup.service').read_text())
        self.assertNotIn('systemctl stop ',self.events()); self.assertNotIn('systemctl start ',self.events())
        self.assertIn('systemctl daemon-reload',self.events())

if __name__=='__main__': unittest.main()
