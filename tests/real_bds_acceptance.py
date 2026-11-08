#!/usr/bin/env python3
"""Exercise official BDS in a disposable unit and persistent isolated network namespace.
Usage (root): python3 tests/real_bds_acceptance.py BDSCTL OFFICIAL_ZIP
No production units, worlds, ports or Discord endpoints are used.
"""
import hashlib,json,os,pathlib,pwd,shutil,subprocess,sys,tempfile,time,uuid,zipfile

VERSION='1.26.63.1'
ZIP_SHA256='b40b04c8f5a7c42a19ee685c22fc593e66116b9c22741b8c75489306b252ff62'

def run(*args,ok=True,timeout=120):
    p=subprocess.run([str(x) for x in args],capture_output=True,text=True,timeout=timeout)
    if ok and p.returncode:raise AssertionError(f'{args}: {p.stdout}\n{p.stderr}')
    return p

def main():
    assert os.geteuid()==0 and sys.platform=='linux'
    source=pathlib.Path(sys.argv[1]).resolve();archive=pathlib.Path(sys.argv[2]).resolve()
    assert hashlib.file_digest(archive.open('rb'),'sha256').hexdigest()==ZIP_SHA256
    root=pathlib.Path(tempfile.mkdtemp(prefix='bds-real-',dir='/var/lib'));root.chmod(0o755)
    binary=root/'bdsctl';shutil.copy2(source,binary)
    user='bds-real-'+uuid.uuid4().hex[:8];unit=user+'.service';unitfile=pathlib.Path('/run/systemd/system')/unit
    created=False;network_created=False
    try:
        run('useradd','--system','--user-group','--no-create-home','--shell','/usr/sbin/nologin',user);created=True;account=pwd.getpwnam(user)
        run('ip','netns','add',user);network_created=True
        run('ip','-n',user,'link','set','lo','up')
        vendor=root/'releases'/VERSION;vendor.mkdir(parents=True)
        # Only the pinned, digest-verified official archive reaches extractall.
        with zipfile.ZipFile(archive) as z:z.extractall(vendor)
        (vendor/'bedrock_server').chmod(0o755)
        (vendor/'release.json').write_text(json.dumps({'id':VERSION,'url':'fixture','sha256':ZIP_SHA256}))
        vendor_files={str(p.relative_to(vendor)):hashlib.file_digest(p.open('rb'),'sha256').hexdigest() for p in vendor.rglob('*') if p.is_file()}
        (root/'releases/current').symlink_to(VERSION)
        data=root/'store/data';data.mkdir(parents=True)
        (data/'worlds').mkdir()
        (data/'server.properties').write_text('server-name=Isolated acceptance\nlevel-name=Test\nonline-mode=false\nallow-list=false\ntransport=nethernet\nserver-port=29132\nserver-udp-ports=29132\nenable-lan-visibility=false\nview-distance=5\ntick-distance=4\nmax-threads=1\n')
        run('chown','-R',user+':'+user,data)
        for name in ['runtime','diagnostics','discord']:
            (root/name).mkdir();os.chown(root/name,account.pw_uid,account.pw_gid)
        config=root/'config.toml';config.write_text(f'''[paths]
store="{root}/store"
releases="{root}/releases"
runtime="{root}/runtime"
diagnostics="{root}/diagnostics"
discord_state="{root}/discord"
webhook_file="{root}/no-webhook"
[server]
user="{user}"
unit="{unit}"
startup_seconds=30
stable_seconds=1
[maintenance]
notice_seconds=0
minimum_free_mib=0
''')
        def ctl(*args):return run(binary,'--config',config,*args)
        ctl('generate-units',root/'units',binary)
        text=(root/'units'/unit).read_text()
        text='\n'.join(l for l in text.splitlines() if not l.startswith(('RuntimeDirectory','StateDirectory','OnFailure=')))+'\n'
        # Keep the same private namespace across restarts: recreating it would
        # erase TIME_WAIT and hide the production restart failure.
        text=text.replace('[Service]',f'[Service]\nNetworkNamespacePath=/run/netns/{user}')
        unitfile.write_text(text);run('systemctl','daemon-reload')
        work=root/'store/engine'/VERSION
        key=data/'keys/server_identity_key.pem'
        def public_key():return run('openssl','pkey','-in',key,'-pubout').stdout
        identity=None
        for attempt in range(3):
            run('systemctl','start',unit)
            status=json.loads((root/'runtime/status.json').read_text());assert status['ready'] and not status['failed']
            assert pathlib.Path(f'/proc/{status["pid"]}/cwd').resolve()==work
            assert (work/'Dedicated_Server.txt').is_file()
            assert run('runuser','-u',user,'--','test','-w',work).returncode==0
            assert run('runuser','-u',user,'--','test','-w',vendor/'bedrock_server',ok=False).returncode!=0
            assert (work/'keys').resolve()==data/'keys'
            assert key.stat().st_mode & 0o777==0o600
            assert run('runuser','-u',user,'--','test','-r',key).returncode==0
            identity=identity or public_key()
            assert public_key()==identity
            # Saving the running key proves BDS loaded our key instead of generating another.
            ctl('console','serveridentity save')
            ctl('console','list')
            if attempt==0:
                (work/'cache-probe').write_text('retained')
                ctl('backup')
                assert json.loads((root/'runtime/status.json').read_text())['ready']
                ids=[p.name for p in (root/'store/backups').iterdir() if not p.name.startswith('.')]
                assert len(ids)==1;ctl('verify',ids[0])
                assert public_key()==identity
                assert public_key()==run('openssl','pkey','-in',root/'store/backups'/ids[0]/'data/keys/server_identity_key.pem','-pubout').stdout
            assert (work/'cache-probe').read_text()=='retained'
            run('systemctl','stop',unit)
            assert json.loads((root/'runtime/status.json').read_text())['stopped']
            logs=run('journalctl','_SYSTEMD_INVOCATION_ID='+status['invocation'],'--no-pager','-o','cat').stdout
            assert 'Saved server identity key to ' in logs
            assert 'Invalid server identity key file' not in logs
            assert public_key()==identity
            # Statistics output is optional without client traffic; caches can flush on exit.
            for name in ['Dedicated_Server.txt','minecraftpe','premium_cache','treatments']:
                assert (work/name).exists(),name
            print(f'PASS real BDS cycle {attempt+1}: writable engine state, stable P-384 identity, normal exit',flush=True)
        # An HTTP connection makes BDS actively close TCP. Its TIME_WAIT remains
        # after the old process exits, and BDS itself cannot rebind immediately.
        run('systemctl','start',unit)
        run('ip','netns','exec',user,sys.executable,'-c','''import socket
with socket.create_connection(('127.0.0.1',29132),timeout=5) as client:
 client.sendall(b'GET /v1/join HTTP/1.1\\r\\nHost: localhost\\r\\nConnection: close\\r\\n\\r\\n')
 while client.recv(65536):pass
''')
        run('systemctl','stop',unit)
        time_wait=run('ip','netns','exec',user,'ss','-Hant','state','time-wait','sport = :29132').stdout
        assert time_wait,'fixture must retain server-side TIME_WAIT after BDS exit'
        previous=json.loads((root/'runtime/status.json').read_text())['invocation']
        start_job=subprocess.Popen(['systemctl','start',unit],stdout=subprocess.PIPE,stderr=subprocess.PIPE)
        try:
            until=time.monotonic()+15
            while time.monotonic()<until:
                current=json.loads((root/'runtime/status.json').read_text())
                if current['invocation']!=previous and current.get('phase')=='preparing':break
                time.sleep(.05)
            assert current['invocation']!=previous and current['phase']=='preparing'
            assert current['pid']==0 and not current['ready'] and not current['failed']
            run('systemctl','stop',unit)
            start_job.communicate(timeout=15)
            cancelled=json.loads((root/'runtime/status.json').read_text())
            assert cancelled['phase']=='stopped' and cancelled['pid']==0 and cancelled['child_exit'] is None
        finally:
            if start_job.poll() is None:
                run('systemctl','stop',unit,ok=False);start_job.communicate(timeout=15)
        run('systemctl','start',unit)
        current=json.loads((root/'runtime/status.json').read_text())
        assert current['ready'] and current['phase']=='running' and not current['failed']
        assert run('systemctl','show',unit,'-p','NRestarts','--value').stdout.strip()=='0'
        logs=run('journalctl','_SYSTEMD_INVOCATION_ID='+current['invocation'],'--no-pager','-o','cat').stdout
        assert '終了待ちの接続が解消するまでBDSの起動を保留します' in logs
        assert logs.count('Starting Server')==1 and 'BDS異常' not in logs
        assert public_key()==identity
        run('systemctl','stop',unit)
        print('PASS real BDS waits for TCP reuse before launch, cancels pending launch, and restarts without failure/recovery',flush=True)
        for rel,digest in vendor_files.items():assert hashlib.file_digest((vendor/rel).open('rb'),'sha256').hexdigest()==digest,rel
        assert {str(p.relative_to(vendor)) for p in vendor.rglob('*') if p.is_file()}==set(vendor_files)
        assert not any(p.is_symlink() for p in vendor.rglob('*'))
        print('PASS official BDS creates runtime files without modifying vendor files; offline backup verifies',flush=True)
    except BaseException:
        print(run('journalctl','-u',unit,'--no-pager','-n','100',ok=False).stdout,file=sys.stderr)
        raise
    finally:
        run('systemctl','stop',unit,ok=False)
        unitfile.unlink(missing_ok=True);run('systemctl','daemon-reload',ok=False);run('systemctl','reset-failed',unit,ok=False)
        if created:run('userdel',user)
        if network_created:run('ip','netns','del',user,ok=False)
        shutil.rmtree(root)

if __name__=='__main__':main()
