#!/usr/bin/env python3
"""Real Linux/systemd acceptance with disposable units, user, data and loopback ports.
Does not operate bds.service, existing worlds, Discord endpoints or OS reboot.
Run as root: python3 tests/linux_acceptance.py BDSCTL FAKE_BDS
"""
import hashlib,json,os,pathlib,pwd,shutil,socket,subprocess,sys,tempfile,time,uuid

def run(*args,ok=True,timeout=90):
    result=subprocess.run([str(a) for a in args],capture_output=True,text=True,timeout=timeout)
    if ok and result.returncode:
        raise AssertionError(f'{args}: {result.stdout}\n{result.stderr}')
    return result

def port():
    with socket.socket() as s:
        s.bind(('127.0.0.1',0));return s.getsockname()[1]

def main():
    assert os.geteuid()==0 and sys.platform=='linux'
    source=pathlib.Path(sys.argv[1]).resolve();fake=pathlib.Path(sys.argv[2]).resolve()
    token=uuid.uuid4().hex[:8];user='bds-test-'+token;unit='bds-smoke-'+token+'.service';receiver='bds-smoke-discord-'+token+'.service'
    root=pathlib.Path(tempfile.mkdtemp(prefix='bdsctl-smoke-',dir='/run'));root.chmod(0o755)
    units=[];created=False;binary=root/'bdsctl';shutil.copy2(source,binary)
    report={'checks':[]}
    def check(label):report['checks'].append(label);print('PASS',label,flush=True)
    def ctl(*args,ok=True):return run(binary,'--config',root/'config.toml',*args,ok=ok)
    def status():return json.loads((root/'runtime/status.json').read_text())
    try:
        run('useradd','--system','--user-group','--no-create-home','--shell','/usr/sbin/nologin',user);created=True;account=pwd.getpwnam(user)
        for name in ['store','releases','runtime','diagnostics','discord']:(root/name).mkdir()
        for name in ['runtime','diagnostics','discord']:
            os.chown(root/name,account.pw_uid,account.pw_gid);(root/name).chmod(0o700)
        data=root/'store/data';(data/'worlds').mkdir(parents=True);(data/'worlds/data').write_text('original-world')
        server_port=port();discord_port=port()
        (data/'server.properties').write_text(f'transport=nethernet\nserver-port={server_port}\n')
        release=root/'releases/1.2.3.4';release.mkdir();shutil.copy2(fake,release/'bedrock_server')
        (release/'release.json').write_text(json.dumps({'id':'1.2.3.4','url':'fixture','sha256':'fixture'}))
        os.symlink('1.2.3.4',root/'releases/current')
        (root/'config.toml').write_text(f'''[paths]
store = "{root}/store"
releases = "{root}/releases"
runtime = "{root}/runtime"
diagnostics = "{root}/diagnostics"
discord_state = "{root}/discord"
webhook_file = "{root}/disabled-webhook"
[server]
user = "{user}"
unit = "{unit}"
startup_seconds = 3
stable_seconds = 1
health_seconds = 30
response_seconds = 1
failure_limit = 3
restart_seconds = 1
start_limit = 2
[maintenance]
notice_seconds = 0
minimum_free_mib = 0
retention = 2
[discord]
enabled = true
listen = "127.0.0.1:{discord_port}"
application_id = "1555006504107511919"
public_key = "1e0e5eafd3168ceedaba850d1228bac6947b6be9f0fd0b1a7f410521bb0a90c9"
guild_id = "1506611823267414036"
allowed_users = ["642606556151545857"]
''')
        ctl('recover');run('chown','-R',user+':'+user,data)
        ctl('generate-units',root/'generated',binary)
        for original,name in [(unit,unit),('bds-discord.service',receiver)]:
            text=(root/'generated'/original).read_text()
            text='\n'.join(l for l in text.splitlines() if not l.startswith(('RuntimeDirectory','StateDirectory','OnFailure=')))+'\n'
            if original=='bds-discord.service':text=text.replace('User=bds-discord','User='+user).replace('Group=bds-discord','Group='+user)
            path=pathlib.Path('/run/systemd/system')/name;path.write_text(text);units.append(name)
        run('systemctl','daemon-reload');run('systemd-analyze','verify',*[pathlib.Path('/run/systemd/system')/x for x in units])
        run('systemctl','start',unit,receiver);assert status()['ready'];check('systemd READY requires owned TCP listener and console response')
        # Both resident Rust processes, excluding the fake Minecraft child.
        pids=[int(run('systemctl','show',u,'-p','MainPID','--value').stdout) for u in units]
        def cpu_ticks(pid):return sum(map(int,pathlib.Path(f'/proc/{pid}/stat').read_text().rsplit(')',1)[1].split()[11:13]))
        before=sum(cpu_ticks(p) for p in pids);started=time.monotonic();time.sleep(30)
        seconds=time.monotonic()-started;after=sum(cpu_ticks(p) for p in pids)
        pss=sum(int(l.split()[1]) for p in pids for l in pathlib.Path(f'/proc/{p}/smaps_rollup').read_text().splitlines() if l.startswith('Pss:'))
        cpu=(after-before)/os.sysconf('SC_CLK_TCK')/seconds*100
        report['resident_pss_kib']=pss;report['idle_cpu_percent_one_core']=round(cpu,3);report['cpu_sample_seconds']=round(seconds,2)
        assert pss<=20*1024,(pss,'PSS target exceeded');assert cpu<=0.1,(cpu,'idle CPU target exceeded');check('two Rust processes meet PSS/idle CPU targets in isolated fixture')
        # Unauthenticated requests can never become an action.
        import urllib.request,urllib.error
        try:urllib.request.urlopen(urllib.request.Request(f'http://127.0.0.1:{discord_port}/interactions',data=b'{"type":1}'),timeout=3)
        except urllib.error.HTTPError as e:assert e.code==401
        else:raise AssertionError('unsigned interaction accepted')
        check('unsigned HTTP interaction rejected')
        # A slow body must not serialize unrelated requests; all sockets expire.
        slow=[]
        try:
            def partial():
                connection=socket.create_connection(('127.0.0.1',discord_port),timeout=1)
                connection.settimeout(3)
                headers=f'POST /interactions HTTP/1.1\r\nHost: localhost\r\nContent-Length: 65536\r\nX-Signature-Timestamp: {int(time.time())}\r\nX-Signature-Ed25519: '+('0'*128)+'\r\n\r\n{'
                connection.sendall(headers.encode());return connection
            slow.append(partial());time.sleep(.1)
            try:urllib.request.urlopen(urllib.request.Request(f'http://127.0.0.1:{discord_port}/interactions',data=b'{}'),timeout=1)
            except urllib.error.HTTPError as e:assert e.code==401
            else:raise AssertionError('unsigned interaction accepted')
            for _ in range(3):slow.append(partial())
            time.sleep(.1)
            try:urllib.request.urlopen(urllib.request.Request(f'http://127.0.0.1:{discord_port}/interactions',data=b'{}'),timeout=1)
            except urllib.error.HTTPError as e:assert e.code==503,e.code
            else:raise AssertionError('unbounded connection accepted')
            for connection in slow:assert b'408' in connection.recv(256).split(b'\r\n',1)[0]
        finally:
            for connection in slow:connection.close()
        check('slow HTTP bodies expire, concurrent requests progress and excess connections are rejected')
        # Copy failure must not leave a previously running server stopped.
        fakebin=root/'fakebin';fakebin.mkdir();(fakebin/'rsync').write_text('#!/bin/sh\nexit 23\n');(fakebin/'rsync').chmod(0o755)
        failed=subprocess.run([str(binary),'--config',str(root/'config.toml'),'backup'],env={**os.environ,'PATH':str(fakebin)+':'+os.environ['PATH']},capture_output=True,text=True,timeout=60)
        assert failed.returncode and status()['ready'];check('copy failure restarts old server without touching live world')
        # Killing the manager cannot unlock the still-running copy child.
        entered=root/'copy-entered';finished=root/'copy-finished'
        (fakebin/'rsync').write_text(f'#!/bin/sh\ntouch {entered}\nsleep 3\n/usr/bin/rsync "$@"\ntouch {finished}\n')
        process=subprocess.Popen([str(binary),'--config',str(root/'config.toml'),'backup'],env={**os.environ,'PATH':str(fakebin)+':'+os.environ['PATH']},stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        until=time.monotonic()+15
        while not entered.exists() and time.monotonic()<until:time.sleep(.05)
        assert entered.exists();process.kill();process.wait(timeout=3)
        assert ctl('recover',ok=False).returncode!=0,'orphan copy lost its lock'
        until=time.monotonic()+10
        while not finished.exists() and time.monotonic()<until:time.sleep(.05)
        assert finished.exists();time.sleep(.1);ctl('recover');run('systemctl','start',unit);assert status()['ready'];check('orphaned copy retains lock and incomplete snapshot is cleaned only afterward')
        # Root job receipts must remain readable by the unprivileged HTTP receiver,
        # including when the maintenance unit applies UMask=0077.
        job={'id':uuid.uuid4().hex,'user':'642606556151545857','kind':'restart','phase':'dispatched','created':int(time.time()),'boot':pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip()}
        jobfile=root/'discord/job.json';jobfile.write_text(json.dumps(job));os.chown(jobfile,account.pw_uid,account.pw_gid);jobfile.chmod(0o600)
        result=subprocess.run([str(binary),'--config',str(root/'config.toml'),'discord-job','restart'],capture_output=True,text=True,timeout=60,preexec_fn=lambda:os.umask(0o077));assert result.returncode==0,result.stderr
        outcome=json.loads(run('runuser','-u',user,'--','cat',root/'store/state/discord-outcome.json').stdout);assert outcome['phase']=='done'
        invocation=status()['invocation'];assert ctl('discord-job','restart',ok=False).returncode;assert status()['invocation']==invocation;check('root job receipt is readable by receiver and duplicate execution is rejected')
        invocation=status()['invocation'];ctl('backup');assert status()['ready'] and status()['invocation']!=invocation;check('offline backup stops and starts real systemd invocation')
        snapshots=sorted((root/'store/backups').iterdir());snapshot=json.loads((snapshots[-1]/'snapshot.json').read_text());sid=snapshot['id'];snapshot_dir=root/'store/backups'/sid
        assert (snapshot_dir/'data/worlds/data').stat().st_ino!=(data/'worlds/data').stat().st_ino
        ctl('verify',sid);ctl('export',sid,root/'snapshot.tar.zst');run('zstd','-t',root/'snapshot.tar.zst');check('snapshot verifies and exports through one zstd process')
        (data/'worlds/data').write_text('changed');ctl('restore',sid);assert (data/'worlds/data').read_text()=='original-world';assert status()['ready'];check('restore copies independent world and restarts successfully')
        bad=root/'releases/1.2.3.5';shutil.copytree(release,bad,symlinks=True);(bad/'mode').write_text('no-listener');(bad/'release.json').write_text(json.dumps({'id':'1.2.3.5','url':'fixture','sha256':'fixture'}))
        (root/'store/state/pending-release.json').write_text((bad/'release.json').read_text())
        result=ctl('daily',ok=False);assert result.returncode!=0,result.stdout;assert os.readlink(root/'releases/current')=='1.2.3.4';assert status()['ready'];assert (root/'store/state/rejected-release.json').is_file();assert (data/'worlds/data').read_text()=='original-world';check('failed update rolls back world and executable after bounded retries')
        assert any((root/'diagnostics').iterdir());check('failed startup leaves persistent diagnostic evidence')
        run('systemctl','stop',unit);assert status()['stopped'] and status()['child_exit']==0
        transaction={'id':'simulated-interruption','kind':'maintenance','phase':'applying','old_release':'1.2.3.4','snapshot':sid,'was_active':True,'owner':99999999,'owner_start':'not-running','boot':pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip()}
        (root/'store/state/transaction.json').write_text(json.dumps(transaction));(data/'worlds/data').write_text('interrupted-new-version');(root/'releases/current').unlink();os.symlink('1.2.3.5',root/'releases/current')
        ctl('recover');assert (data/'worlds/data').read_text()=='original-world';assert os.readlink(root/'releases/current')=='1.2.3.4';run('systemctl','start',unit);assert status()['ready'];check('interrupted update recovers before next BDS start')
        # Half-replaced directory after a power loss is recoverable too.
        run('systemctl','stop',unit);(root/'store/state/transaction.json').write_text(json.dumps(transaction));shutil.rmtree(data)
        ctl('recover');assert (data/'worlds/data').read_text()=='original-world';check('interrupted data-directory replacement is recoverable')
        # Initial import retries use a durable record, not deletion of existing data.
        imported=root/'imported';old=root/'old';(old/'worlds').mkdir(parents=True)
        (old/'worlds/data').write_text('import-world');(old/'server.properties').write_text(f'transport=nethernet\nserver-port={server_port}\n');(old/'.installed-version').write_text('1.2.3.4')
        cached=imported/'releases/1.2.3.4';cached.mkdir(parents=True);shutil.copy2(fake,cached/'bedrock_server')
        (cached/'release.json').write_text(json.dumps({'id':'1.2.3.4','url':'fixture','sha256':'fixture'}));(cached/'world_templates').write_text('damaged cached release')
        import_config=root/'import-config.toml';text=(root/'config.toml').read_text()
        for name in ['store','releases','runtime','diagnostics','discord']:text=text.replace(f'{root}/{name}"',f'{imported}/{name}"')
        import_config.write_text(text)
        def importing(*args,ok=True):return run(binary,'--config',import_config,*args,ok=ok)
        result=importing('import-legacy',old,ok=False);assert result.returncode and (imported/'store/state/import.json').exists()
        assert (imported/'store/data/worlds/data').read_text()=='import-world'
        assert importing('restart',ok=False).returncode
        (cached/'world_templates').unlink();importing('import-legacy',old)
        assert not (imported/'store/state/import.json').exists();assert os.readlink(imported/'releases/current')=='1.2.3.4'
        assert (old/'worlds/data').read_text()=='import-world';check('failed initial import retries safely without deleting destination or changing source')
        # Simulate power loss just before data publication, then after activation.
        stage=imported/'store/.import-interrupted';(imported/'store/data').rename(stage);(imported/'releases/current').unlink()
        marker=imported/'store/state/import.json';record={'release':'1.2.3.4','stage':stage.name};marker.write_text(json.dumps(record))
        importing('recover');assert (imported/'store/data/worlds/data').read_text()=='import-world';assert not marker.exists()
        marker.write_text(json.dumps(record));importing('recover');assert not marker.exists()
        check('initial import recovers interruption before publication and after activation')
        # A status write failure must not deadlock BDS on its stdout pipe.
        (release/'mode').write_text('large-stop-output');run('systemctl','reset-failed',unit,ok=False);run('systemctl','start',unit);assert status()['ready']
        status_file=root/'runtime/status.json';status_file.unlink();status_file.mkdir()
        try:
            until=time.monotonic()+10
            while time.monotonic()<until:
                state=run('systemctl','show',unit,'-p','ActiveState','--value').stdout.strip()
                if state=='failed':break
                time.sleep(.1)
            assert state=='failed',state
            assert run('systemctl','show',unit,'-p','MainPID','--value').stdout.strip()=='0'
        finally:
            status_file.rmdir();(release/'mode').unlink()
        check('supervisor error still drains large stop output and BDS exits without a forced kill')
        print(json.dumps(report,ensure_ascii=False,indent=2),flush=True)
        if os.environ.get('BDS_ACCEPTANCE_REPORT'):pathlib.Path(os.environ['BDS_ACCEPTANCE_REPORT']).write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n')
    except BaseException:
        for name in units:print(run('journalctl','-u',name,'--no-pager','-n','70',ok=False).stdout,file=sys.stderr)
        raise
    finally:
        for name in units:
            run('systemctl','stop',name,ok=False);run('systemctl','reset-failed',name,ok=False);(pathlib.Path('/run/systemd/system')/name).unlink(missing_ok=True)
        run('systemctl','daemon-reload',ok=False)
        if created:run('userdel',user,ok=False)
        shutil.rmtree(root)
if __name__=='__main__':main()
