"""Exercise real shell/jq parsing with fake journal, HTTP and service management."""
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'bds.sh'
MOCK = r'''
import fcntl,json,os,pathlib,sys,time
root=pathlib.Path(os.environ['TEST_ROOT']); name=pathlib.Path(sys.argv[0]).name; args=sys.argv[1:]
def append(file,data):
    with (root/file).open('a') as f: f.write(json.dumps(data,ensure_ascii=False)+'\n')
if name=='flock':
    try: fcntl.flock(int(args[-1]),fcntl.LOCK_EX|fcntl.LOCK_NB)
    except BlockingIOError: sys.exit(1)
    sys.exit(0)
if name=='sleep':
    append('delays',args[0]); time.sleep(min(float(args[0]),.02)); sys.exit(0)
if name=='journalctl':
    append('journal-args',args)
    def entries(): return [json.loads(x) for x in (root/'journal').read_text().splitlines() if x]
    if '--follow' not in args:
        data=entries()
        if data: print(json.dumps(data[-1]))
        sys.exit(0)
    if os.environ.get('JOURNAL_MODE')=='fail': sys.exit(1)
    assert '--no-tail' in args
    cursor=next(x.split('=',1)[1] for x in args if x.startswith('--after-cursor='))
    data=entries(); found=[i for i,x in enumerate(data) if x['__CURSOR']==cursor]
    if not found: sys.exit(1)
    index=found[0]+1
    (root/'following').touch()
    while True:
        data=entries()
        while index<len(data):
            item=data[index]; index+=1
            if item.get('MALFORMED'): print('invalid json',flush=True)
            else: print(json.dumps(item),flush=True)
        if os.environ.get('JOURNAL_MODE')=='eof': sys.exit(0)
        time.sleep(.01)
if name=='curl':
    p=root/'requests'
    count=len(p.read_text().splitlines()) if p.exists() else 0
    response_file=root/'responses'; responses=json.loads(response_file.read_text()) if response_file.exists() else []
    response=responses[min(count,len(responses)-1)] if responses else {'status':204}
    payload=json.loads(args[args.index('--data-binary')+1])
    checkpoint=root/'.player-notify/cursor'
    append('requests',{'payload':payload,'args':args,'checkpoint':checkpoint.read_text().strip() if checkpoint.exists() else None})
    if os.environ.get('CURL_MODE')=='timeout':
        time.sleep(float(args[args.index('--max-time')+1])); print('000',end=''); sys.exit(28)
    if os.environ.get('CURL_MODE')=='block':
        (root/'sending').touch(); time.sleep(30)
    pathlib.Path(args[args.index('--output')+1]).write_text(json.dumps(response.get('body',{})))
    pathlib.Path(args[args.index('--dump-header')+1]).write_text('Retry-After: '+str(response.get('retry_after',.02))+'\r\n')
    print(str(response.get('status',204)).zfill(3),end='')
    sys.exit(response.get('exit',0))
if name=='id': print('0' if args==['-u'] else 'tester'); sys.exit(0)
if name=='systemctl':
    append('systemctl-args',args)
    if args[0]=='show':
        prop=next(x.split('=',1)[1] for x in args if x.startswith('--property='))
        print({'User':'tester','Group':'tester','LoadState':'loaded','ActiveState':'inactive','MainPID':'0','ControlGroup':''}[prop])
    sys.exit(0)
sys.exit(99)
'''

@unittest.skipUnless(shutil.which('jq'), 'jq is required to test the production JSON parser')
class PlayerNotificationsTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory(prefix='bds-notify-test-'); self.addCleanup(self.tmp.cleanup)
        self.root=Path(self.tmp.name); (self.root/'units').mkdir()
        self.script=self.root/'bds.sh'
        import shutil
        shutil.copy2(SCRIPT.parent/'supervisor.py', self.root/'supervisor.py')
        self.script.write_text(SCRIPT.read_text().replace('SYSTEMD_DIR="/etc/systemd/system"','SYSTEMD_DIR='+shlex.quote(str(self.root/'units'))))
        bindir=self.root/'bin'; bindir.mkdir(); mock=bindir/'mock'
        mock.write_text('#!'+sys.executable+'\n'+MOCK); mock.chmod(0o755)
        for name in ['flock','journalctl','curl','sleep','systemctl','id']: (bindir/name).symlink_to(mock)
        self.env=dict(os.environ,TEST_ROOT=str(self.root),PATH=str(bindir)+':'+os.environ['PATH'],BDS_CONFIG=str(self.root/'bds.conf'))
        self.config('PLAYER_NOTIFY_ENABLED=true\nDISCORD_WEBHOOK_URL=https://example.invalid/secret\n')
        (self.root/'journal').write_text('')
        self.entry('c1','[INFO] Player connected: AlreadyOnline, xuid: 123')
        self.processes=[]; self.addCleanup(self.stop_all)

    def config(self,text): (self.root/'bds.conf').write_text(text)

    def entry(self,cursor,message,**extra):
        with (self.root/'journal').open('a') as f:
            f.write(json.dumps(dict(__CURSOR=cursor,MESSAGE=message,**extra))+'\n')

    def start(self,**env):
        p=subprocess.Popen(['bash',str(self.script),'watch-players'],env=dict(self.env,**env),
                           stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,start_new_session=True)
        self.processes.append(p); return p

    def stop(self,p):
        if p.poll() is None: os.killpg(p.pid,signal.SIGTERM)
        try: return p.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(p.pid,signal.SIGKILL); return p.communicate(timeout=5)

    def stop_all(self):
        for p in self.processes: self.stop(p)

    def wait_for(self,check):
        deadline=time.monotonic()+5
        while time.monotonic()<deadline:
            if check(): return
            time.sleep(.02)
        self.fail('Timed out waiting for notifier state')

    def records(self,name):
        p=self.root/name
        return [json.loads(x) for x in p.read_text().splitlines()] if p.exists() else []

    def cursor(self):
        p=self.root/'.player-notify/cursor'
        return p.read_text().strip() if p.exists() else ''

    def ready(self): self.wait_for(lambda: (self.root/'following').exists())

    def test_only_join_leave_notify_with_safe_payload_and_no_initial_replay(self):
        p=self.start(); self.ready()
        self.assertEqual(self.records('requests'),[])
        self.entry('c2','[2026-10-01 00:00:01:001 INFO] Player connected: テスト @everyone "A", xuid: 123, pfid: ab')
        self.entry('c3','[INFO] <Steve> Player connected: Fake, xuid: 123')
        self.entry('c4','[INFO] Server started.')
        self.entry('c5','[INFO] Player disconnected: テスト @everyone "A", xuid: 123')
        self.wait_for(lambda: self.cursor()=='c5')
        requests=self.records('requests'); self.assertEqual(len(requests),2)
        self.assertEqual(requests[0]['payload']['content'],'テスト @everyone "A" がログインしました。')
        self.assertEqual(requests[1]['payload']['content'],'テスト @everyone "A" がログアウトしました。')
        for r in requests:
            self.assertEqual(r['payload']['allowed_mentions'],{'parse':[]})
            self.assertIn('--max-time',r['args']); self.assertIn('--connect-timeout',r['args'])
        self.assertEqual(requests[0]['checkpoint'],'c1')
        self.stop(p); self.assertEqual(list((self.root/'.player-notify').glob('.run.*')),[])

    def test_startup_no_log_file_prefix_notifies_without_accepting_chat_spoof(self):
        p=self.start(); self.ready()
        prefix='NO LOG FILE! - [2026-09-25 18:05:37:086 INFO] '
        self.entry('c2',prefix+'Player connected: Steve, xuid: 123')
        self.entry('c3',prefix+'<Steve> Player connected: Fake, xuid: 123')
        self.entry('c4',prefix+'Player disconnected: Steve, xuid: 123')
        self.wait_for(lambda:self.cursor()=='c4')
        self.assertEqual([x['payload']['content'] for x in self.records('requests')],
                         ['Steve がログインしました。','Steve がログアウトしました。'])

    def test_resume_catches_all_events_over_ten_without_replaying_sent_events(self):
        p=self.start(); self.ready()
        self.entry('c2','[INFO] Player connected: First, xuid: 1')
        self.wait_for(lambda:self.cursor()=='c2'); self.stop(p)
        for i in range(3,20): self.entry('c'+str(i),'[INFO] Player connected: P'+str(i)+', xuid: 1')
        p=self.start(); self.wait_for(lambda:self.cursor()=='c19')
        self.assertEqual(len(self.records('requests')),18)
        follows=[x for x in self.records('journal-args') if '--follow' in x]
        self.assertIn('--after-cursor=c2',follows[-1]); self.assertIn('--no-tail',follows[-1])

    def test_transient_failure_and_rate_limit_retry_then_success(self):
        (self.root/'responses').write_text(json.dumps([
            {'status':503},{'status':429,'body':{'retry_after':.07}},{'status':204}]))
        p=self.start(); self.ready(); self.entry('c2','[INFO] Player connected: Steve, xuid: 1')
        self.wait_for(lambda:self.cursor()=='c2')
        requests=self.records('requests'); self.assertEqual(len(requests),3)
        self.assertTrue(all(x['checkpoint']=='c1' for x in requests))
        self.assertEqual(self.records('delays'),['2','0.07'])

    def test_429_header_fallback_is_respected(self):
        (self.root/'responses').write_text(json.dumps([{'status':429,'retry_after':.09},{'status':204}]))
        p=self.start(); self.ready(); self.entry('c2','[INFO] Player connected: Steve, xuid: 1')
        self.wait_for(lambda:self.cursor()=='c2'); self.assertEqual(self.records('delays'),['0.09'])

    def test_final_rate_limit_also_waits_before_next_event(self):
        (self.root/'responses').write_text(json.dumps([{'status':429,'body':{'retry_after':.03}}]*3+[{'status':204}]))
        p=self.start(); self.ready()
        self.entry('c2','[INFO] Player connected: Steve, xuid: 1')
        self.entry('c3','[INFO] Player disconnected: Steve, xuid: 1')
        self.wait_for(lambda:self.cursor()=='c3')
        self.assertEqual(len(self.records('requests')),4)
        self.assertEqual(self.records('delays'),['0.03']*3)

    def test_exhausted_network_failures_are_logged_and_do_not_block_future_events(self):
        (self.root/'responses').write_text(json.dumps([{'status':0,'exit':7}]*3+[{'status':204}]))
        p=self.start(); self.ready(); self.entry('c2','[INFO] Player connected: Steve, xuid: 1')
        self.entry('c3','[INFO] Player disconnected: Steve, xuid: 1')
        self.wait_for(lambda:self.cursor()=='c3'); out,err=self.stop(p)
        self.assertEqual(len(self.records('requests')),4)
        self.assertIn('未送信',err); self.assertIn('cursor=c2',err); self.assertNotIn('secret',err)

    def test_permanent_http_error_is_not_retried(self):
        (self.root/'responses').write_text(json.dumps([{'status':404}]))
        p=self.start(); self.ready(); self.entry('c2','[INFO] Player connected: Steve, xuid: 1')
        self.wait_for(lambda:self.cursor()=='c2'); self.assertEqual(len(self.records('requests')),1)

    def test_journal_exit_even_successful_exits_watcher_for_service_restart(self):
        for mode in ['eof','fail']:
            p=self.start(JOURNAL_MODE=mode); out,err=p.communicate(timeout=5)
            self.assertNotEqual(p.returncode,0); self.assertIn('監視が終了',err)
            self.assertEqual(list((self.root/'.player-notify').glob('.run.*')),[])

    def test_parser_failure_exits_watcher_without_advancing_cursor(self):
        p=self.start(); self.ready(); self.entry('c2','bad',MALFORMED=True)
        out,err=p.communicate(timeout=5); self.assertNotEqual(p.returncode,0); self.assertEqual(self.cursor(),'c1')

    def test_duplicate_watcher_refused_but_maintenance_lock_does_not_block_monitor(self):
        import fcntl
        with (self.root/'.maintenance.lock').open('w') as f:
            fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB)
            p=self.start(); self.ready(); other=self.start(); out,err=other.communicate(timeout=5)
            self.assertEqual(other.returncode,78); self.assertIn('すでに実行中',err)

    def test_interrupted_send_keeps_previous_cursor_for_retry_after_restart(self):
        p=self.start(CURL_MODE='block'); self.ready(); self.entry('c2','[INFO] Player connected: Steve, xuid: 1')
        self.wait_for(lambda:(self.root/'sending').exists()); self.stop(p)
        self.assertEqual(self.cursor(),'c1')
        other=self.start(); self.wait_for(lambda:self.cursor()=='c2')
        self.assertEqual(len(self.records('requests')),2)

    def notify(self,**environment):
        return subprocess.run(['bash',str(self.script),'notify','テスト @everyone'],
                              env=dict(self.env,**environment),capture_output=True,text=True,timeout=5)

    def test_regular_notification_uses_same_retry_and_mention_protection(self):
        (self.root/'responses').write_text(json.dumps([
            {'status':503},{'status':429,'body':{'retry_after':.07}},{'status':204}]))
        r=self.notify(); self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        requests=self.records('requests'); self.assertEqual(len(requests),3)
        self.assertEqual(requests[0]['payload'],{'content':'テスト @everyone','allowed_mentions':{'parse':[]}})
        self.assertEqual(self.records('delays'),['2','0.07'])

    def test_regular_notification_failure_logs_status_without_failing_caller(self):
        (self.root/'responses').write_text(json.dumps([{'status':404}]))
        r=self.notify(); self.assertEqual(r.returncode,0)
        self.assertIn('HTTP=404',r.stderr); self.assertIn('試行=1',r.stderr)
        self.assertNotIn('secret',r.stderr); self.assertEqual(len(self.records('requests')),1)

    def test_regular_notification_network_failure_retries_three_times(self):
        (self.root/'responses').write_text(json.dumps([{'status':0,'exit':7}]*3))
        r=self.notify(); self.assertEqual(r.returncode,0)
        self.assertEqual(len(self.records('requests')),3); self.assertIn('curl=7',r.stderr)
        self.assertEqual(self.records('delays'),['2','4'])

    def test_long_rate_limit_does_not_block_regular_notification(self):
        (self.root/'responses').write_text(json.dumps([{'status':429,'body':{'retry_after':3600}}]))
        r=self.notify(); self.assertEqual(r.returncode,0)
        self.assertEqual(len(self.records('requests')),1); self.assertEqual(self.records('delays'),[])
        self.assertIn('HTTP=429',r.stderr); self.assertIn('上限',r.stderr)

    def test_slow_request_uses_remaining_budget_and_stops_retrying(self):
        with (self.root/'bds.conf').open('a') as f: f.write('DISCORD_TIMEOUT_SECONDS=1\n')
        begin=time.monotonic(); r=self.notify(CURL_MODE='timeout')
        self.assertEqual(r.returncode,0); self.assertLess(time.monotonic()-begin,3)
        requests=self.records('requests'); self.assertEqual(len(requests),1)
        args=requests[0]['args']; self.assertEqual(args[args.index('--max-time')+1],'1')
        self.assertIn('curl=28',r.stderr)

    def test_notification_budget_accepts_decimal_with_leading_zero(self):
        with (self.root/'bds.conf').open('a') as f: f.write('DISCORD_TIMEOUT_SECONDS=08\n')
        r=self.notify(); self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        args=self.records('requests')[0]['args']
        self.assertEqual(args[args.index('--max-time')+1],'8')

    def test_invalid_notification_budget_does_not_fail_caller(self):
        with (self.root/'bds.conf').open('a') as f: f.write('DISCORD_TIMEOUT_SECONDS=invalid\n')
        r=self.notify(); self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertIn('DISCORD_TIMEOUT_SECONDS',r.stderr); self.assertEqual(self.records('requests'),[])

    def test_player_rate_limit_beyond_budget_waits_before_next_event(self):
        (self.root/'responses').write_text(json.dumps([{'status':429,'body':{'retry_after':100}},{'status':204}]))
        p=self.start(); self.ready()
        self.entry('c2','[INFO] Player connected: Steve, xuid: 1')
        self.entry('c3','[INFO] Player disconnected: Steve, xuid: 1')
        self.wait_for(lambda:self.cursor()=='c3')
        self.assertEqual(len(self.records('requests')),2)
        self.assertEqual(self.records('delays'),['100'])

    def test_missing_configuration_does_not_read_journal(self):
        self.config('PLAYER_NOTIFY_ENABLED=false\n')
        p=self.start(); out,err=p.communicate(timeout=5)
        self.assertEqual(p.returncode,78); self.assertEqual(self.records('journal-args'),[])

    def test_configure_enables_independent_service_and_status_logs_aliases(self):
        r=subprocess.run(['bash',str(self.script),'configure'],env=self.env,capture_output=True,text=True,timeout=5)
        self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        unit=(self.root/'units/bds-player-notify.service').read_text()
        for setting in ['Restart=always','RestartPreventExitStatus=78','KillMode=control-group','watch-players']:
            self.assertIn(setting,unit)
        for setting in ['Requires=bds.service','After=bds.service','PartOf=bds.service']: self.assertNotIn(setting,unit)
        self.assertIn(['restart','bds-player-notify.service'],self.records('systemctl-args'))
        for command in ['status','logs']:
            # logs uses journalctl; verify arguments without contacting real services.
            r=subprocess.run(['bash',str(self.script),command,'player-notify'],env=self.env,capture_output=True,text=True,timeout=5)
            self.assertEqual(r.returncode,0,r.stderr)
        self.assertIn(['status','bds-player-notify.service'],self.records('systemctl-args'))
        self.assertTrue(any('bds-player-notify.service' in x for x in self.records('journal-args')))

    def test_configure_disabled_does_not_start_notifier(self):
        self.config('PLAYER_NOTIFY_ENABLED=false\n')
        r=subprocess.run(['bash',str(self.script),'configure'],env=self.env,capture_output=True,text=True,timeout=5)
        self.assertEqual(r.returncode,0,r.stderr)
        self.assertIn(['disable','--now','bds-player-notify.service'],self.records('systemctl-args'))
        self.assertNotIn(['restart','bds-player-notify.service'],self.records('systemctl-args'))

    def test_uninstall_removes_and_stops_notifier(self):
        (self.root/'units/bds-player-notify.service').touch()
        r=subprocess.run(['bash',str(self.script),'uninstall'],env=self.env,capture_output=True,text=True,timeout=5)
        self.assertEqual(r.returncode,0,r.stderr)
        self.assertFalse((self.root/'units/bds-player-notify.service').exists())
        self.assertIn(['disable','--now','bds-player-notify.service'],self.records('systemctl-args'))

if __name__=='__main__': unittest.main()
