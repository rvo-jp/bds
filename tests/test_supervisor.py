"""Readiness, fault detection and safe shutdown without a production server."""
import argparse
import importlib.util
import io
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock

spec=importlib.util.spec_from_file_location('bds_supervisor',Path(__file__).resolve().parents[1]/'supervisor.py')
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)

class SupervisorTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup)
        self.root=Path(self.tmp.name);self.runtime=self.root/'server';self.runtime.mkdir()
        (self.runtime/'server.properties').write_text('server-port=23456\n')
        self.state=self.root/'state';self.state.mkdir()
        self.now=0.;self.listening=True;self.notifications=[]
        args=argparse.Namespace(runtime=str(self.runtime),state_dir=str(self.state),diagnostics_dir=str(self.root/'diagnostics'),start_timeout=5,stable=0,interval=1,response_timeout=.5,failures=3)
        self.s=m.Supervisor(args,clock=lambda:self.now,listener=lambda pid,port:self.listening,notifier=self.notifications.append)
        self.s.child=Mock(pid=42,stdin=io.BytesIO());self.s.child.poll.return_value=None
        self.s.start_deadline=5
    def tick(self,t):self.now=t;self.s.health(t)
    def ready(self):
        self.s.line(b'[INFO] Server started.');self.tick(0)
        self.s.line(b'[INFO] There are 0/10 players online:');self.tick(.01)
        self.assertTrue(self.s.ready)
    def test_started_log_without_own_listener_is_never_ready(self):
        self.listening=False;self.s.line(b'[INFO] Server started.');self.tick(0);self.tick(5)
        self.assertTrue(self.s.failed);self.assertFalse(self.s.ready)
        self.assertFalse(any('READY=1' in x for x in self.notifications))
        self.assertIn(b'stop\n',self.s.child.stdin.getvalue())
    def test_listener_without_console_response_is_never_ready(self):
        self.s.line(b'[INFO] Server started.');self.tick(0);self.tick(5)
        self.assertTrue(self.s.failed)
    def test_ready_requires_log_listener_and_console_then_notify(self):
        self.ready();d=json.loads((self.state/'status.json').read_text())
        self.assertTrue(d['ready']);self.assertEqual(d['pid'],42)
        self.assertEqual(sum('READY=1' in n for n in self.notifications),1)
    def test_stability_may_finish_after_start_deadline(self):
        self.s.args.stable=7;self.s.line(b'[INFO] Server started.');self.tick(0)
        self.s.line(b'[INFO] There are 0/10 players online:')
        for t in range(8):
            self.tick(t);self.s.line(b'[INFO] There are 0/10 players online:')
        self.assertTrue(self.s.ready);self.assertFalse(self.s.failed)
    def test_runtime_listener_loss_requires_three_checks_and_saves_diagnostics(self):
        self.ready();self.listening=False
        self.tick(1);self.assertFalse(self.s.failed)
        self.tick(2);self.assertFalse(self.s.failed)
        self.tick(3);self.assertTrue(self.s.failed)
        files=list((self.root/'diagnostics').glob('failure-*.json'));self.assertEqual(len(files),1)
        self.assertFalse(json.loads(files[0].read_text())['listener_owned'])
        self.assertFalse(json.loads((self.state/'status.json').read_text())['ready'])
    def test_runtime_console_stall_triggers_graceful_stop(self):
        self.ready()
        for t in [1,1.6,2,2.6,3,3.6]:self.tick(t)
        self.assertTrue(self.s.failed);self.assertIn(b'stop\n',self.s.child.stdin.getvalue())
        self.s.child.kill.assert_not_called();self.s.child.terminate.assert_not_called()
    def test_healthy_reply_resets_failure_streak(self):
        self.ready();self.listening=False;self.tick(1);self.tick(2)
        self.listening=True;self.tick(3);self.s.line(b'[INFO] There are 1/10 players online:');self.tick(3.1)
        self.assertFalse(self.s.failed);self.assertEqual(self.s.misses,0)
    def test_planned_stop_is_not_marked_failure(self):
        self.ready();self.s.stop();self.assertFalse(self.s.failed)
        self.assertFalse(json.loads((self.state/'status.json').read_text())['ready'])
    def test_console_backpressure_is_buffered_without_blocking_shutdown(self):
        writer=Mock();writer.write.side_effect=[None,2,3]
        self.s.child.stdin=writer
        self.assertTrue(self.s.send('list'))
        self.assertEqual(bytes(self.s.commands),b'list\n')
        self.s.flush_commands();self.assertEqual(bytes(self.s.commands),b'st\n')
        self.s.flush_commands();self.assertEqual(bytes(self.s.commands),b'')

    def test_stop_receipt_records_actual_child_exit_and_rejects_failed_child(self):
        self.s.stopping=True;self.s.child.poll.return_value=0
        self.assertTrue(self.s.state()['stopped'])
        self.s.child.poll.return_value=1
        self.assertFalse(self.s.state()['stopped'])
        self.s.child.poll.return_value=0;self.s.failed=True
        self.assertFalse(self.s.state()['stopped'])

    def test_listener_must_belong_to_this_child_not_some_other_process(self):
        proc=self.root/'proc';fd=proc/'42/fd';fd.mkdir(parents=True)
        (fd/'1').symlink_to('socket:[99]');net=proc/'42/net';net.mkdir()
        row='0: 0100007F:5BA0 00000000:0000 0A 0:0 00:0 0 0 0 100'
        (net/'tcp').write_text('header\n'+row+'\n')
        self.assertFalse(m.owns_listener(42,23456,proc))
        (net/'tcp').write_text('header\n'+row.replace('100','99')+'\n')
        self.assertTrue(m.owns_listener(42,23456,proc))
    @unittest.skipUnless(sys.platform=='linux','Requires local Linux sockets')
    def test_notify_datagram(self):
        from unittest.mock import patch
        path=str(self.root/'notify');s=socket.socket(socket.AF_UNIX,socket.SOCK_DGRAM);self.addCleanup(s.close);s.bind(path)
        with patch.dict(os.environ,{'NOTIFY_SOCKET':path}):m.notify('READY=1')
        self.assertEqual(s.recv(100),b'READY=1')

@unittest.skipUnless(sys.platform=='linux','Requires Linux process socket ownership')
class LinuxLifecycleTests(unittest.TestCase):
    def test_actual_child_fifo_and_no_orphan_on_stop(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);runtime=root/'server';runtime.mkdir()
            with socket.socket() as probe:probe.bind(('127.0.0.1',0));port=probe.getsockname()[1]
            (runtime/'server.properties').write_text(f'server-port={port}\n')
            binary=runtime/'bedrock_server';binary.write_text('#!'+sys.executable+'''\nimport socket,sys
s=socket.socket();s.bind(('127.0.0.1',PORT));s.listen()
print('[INFO] Server started.',flush=True)
for line in sys.stdin:
 if line.strip()=='list':print('[INFO] There are 0/10 players online:',flush=True)
 if line.strip()=='stop':break
'''.replace('PORT',str(port)));binary.chmod(0o755)
            state=root/'state';p=subprocess.Popen([sys.executable,str(Path(m.__file__)),'--runtime',str(runtime),'--state-dir',str(state),'--diagnostics-dir',str(root/'diag'),'--stable','0','--interval','.1','--response-timeout','.2','--start-timeout','3'],stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
            try:
                deadline=time.monotonic()+5
                while time.monotonic()<deadline:
                    try:
                        d=json.loads((state/'status.json').read_text())
                        if d['ready']:break
                    except (OSError,ValueError):pass
                    time.sleep(.05)
                else:self.fail('Supervisor did not become ready')
                with (runtime/'.server.stdin').open('w') as stream:stream.write('stop\n')
                self.assertEqual(p.wait(timeout=5),0)
                self.assertFalse((runtime/'.server.stdin').exists());self.assertFalse(Path('/proc/'+str(d['pid'])).exists())
            finally:
                if p.poll() is None:p.terminate();p.wait(timeout=5)
                p.stderr.close()
