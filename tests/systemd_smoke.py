#!/usr/bin/env python3
"""Opt-in real-systemd smoke test. Uses unique runtime units and a fake BDS only.
Run as root on Linux: python3 tests/systemd_smoke.py --run
No production unit, configuration, world or Discord endpoint is modified.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import uuid
import zipfile

GOOD_SERVER = '''#!/usr/bin/python3
import socket,sys,pathlib
launch=pathlib.Path('launch-count')
n=int(launch.read_text())+1 if launch.exists() else 1
launch.write_text(str(n))
mode=pathlib.Path('failure-mode').read_text().strip() if pathlib.Path('failure-mode').exists() else ''
s=socket.socket();s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)
if mode!='always' and not (mode=='first' and n==1):
 s.bind(('127.0.0.1',int(pathlib.Path('port').read_text())));s.listen()
print('[INFO] Server started.',flush=True)
for line in sys.stdin:
 if line.strip()=='list':print('[INFO] There are 0/10 players online:',flush=True)
 if line.strip()=='stop':
  pathlib.Path('worlds/Test/db/shutdown-marker').write_text('saved')
  break
'''
BAD_SERVER = '''#!/usr/bin/python3
import pathlib,time
pathlib.Path('worlds/Test/db/data').write_text('converted')
print('[INFO] Server started.',flush=True)
time.sleep(1)
raise SystemExit(7)
'''

def command(args, expected=0, env=None, timeout=90):
    result=subprocess.run(args,env=env,capture_output=True,text=True,timeout=timeout)
    if expected is not None and result.returncode != expected:
        raise AssertionError(f'{args!r}: exit={result.returncode}\n{result.stdout}\n{result.stderr}')
    return result


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run',action='store_true',help='Create isolated temporary runtime units and run checks')
    options=parser.parse_args()
    if not options.run: parser.error('--run must be specified explicitly')
    if sys.platform!='linux' or os.geteuid()!=0 or not Path('/run/systemd/system').is_dir():
        parser.error('Requires root on a running Linux systemd host')
    for binary in ['systemctl','systemd-analyze','python3','jq','curl','flock','tar']:
        if shutil.which(binary) is None: parser.error('Missing command: '+binary)
    original=Path(__file__).resolve().parents[1]/'bds.sh'
    name='bds-smoke-'+uuid.uuid4().hex[:12]
    suffixes=['.service','-update.service','-update.timer','-backup.service','-backup.timer','-player-notify.service','-failure.service']
    units=[name+suffix for suffix in suffixes]
    unit_paths=[Path('/run/systemd/system')/unit for unit in units]
    assert all(not p.exists() for p in unit_paths)
    root=Path(tempfile.mkdtemp(prefix=name+'-',dir='/tmp'))
    production_args=['systemctl','show','bds.service','--property=ActiveState,MainPID,InvocationID']
    production_before=command(production_args,expected=None).stdout
    results=[]
    try:
        shutil.copy2(original.parent/'supervisor.py', root/'supervisor.py')
        script=root/'bds.sh'
        source=original.read_text().replace('APP_NAME="bds"',f'APP_NAME="{name}"')
        source=source.replace('SYSTEMD_DIR="/etc/systemd/system"','SYSTEMD_DIR="/run/systemd/system"')
        source=source.replace('systemctl enable','systemctl --runtime enable').replace('systemctl disable','systemctl --runtime disable')
        # Limits apply only to isolated test services, never to production BDS.
        source=source.replace('[Service]\n','[Service]\nCPUQuota=10%\nMemoryMax=128M\nTasksMax=32\n')
        script.write_text(source); script.chmod(0o755)
        server=root/'bedrock-server'; (server/'worlds/Test/db').mkdir(parents=True)
        (server/'worlds/Test/db/data').write_text('seed')
        (server/'bedrock_server').write_text(GOOD_SERVER); (server/'bedrock_server').chmod(0o755)
        import socket
        with socket.socket() as probe:
            probe.bind(('127.0.0.1',0)); port=probe.getsockname()[1]
        (server/'port').write_text(str(port))
        (server/'server.properties').write_text(f'server-name=smoke\nlevel-name=Test\nserver-port={port}\n')
        (server/'.installed-version').write_text('1.0.0.0')
        bindir=root/'bin'; bindir.mkdir()
        # Only download transport is a fixture. systemctl/journalctl and all filesystem operations are real.
        downloader=bindir/'curl'
        downloader.write_text('''#!/usr/bin/python3
import json, pathlib, shutil, sys
root=pathlib.Path(__file__).resolve().parents[1]
args=sys.argv[1:]
if '--output' in args:
    shutil.copyfile(root/'release.zip',args[args.index('--output')+1])
else:
    version=(root/'available-version').read_text().strip()
    print(json.dumps({'result':{'links':[{'downloadType':'serverBedrockLinux',
        'downloadUrl':'https://example.invalid/bedrock-server-'+version+'.zip'}]}}))
'''); downloader.chmod(0o755)
        (root/'available-version').write_text('1.0.0.0')
        config=root/'bds.conf'
        config.write_text(f'''export PATH="{bindir}:$PATH"
DISCORD_WEBHOOK_URL=
PLAYER_NOTIFY_ENABLED=false
BACKUP_NOTICE_SECONDS=0
UPDATE_NOTICE_SECONDS=0
BACKUP_LOCK_WAIT_SECONDS=5
SERVER_START_TIMEOUT_SECONDS=3
SERVER_STABLE_SECONDS=2
SERVER_RESTART_LIMIT=2
SERVER_RESTART_DELAY_SECONDS=1
SERVER_HEALTH_INTERVAL_SECONDS=1
SERVER_HEALTH_RESPONSE_TIMEOUT_SECONDS=1
BACKUP_MIN_FREE_MB=32
CHECK_INTERVAL=100d
BACKUP_ON_CALENDAR="2099-01-01 00:00:00"
''')
        env=dict(os.environ,BDS_CONFIG=str(config))
        def run(*args,expected=0):
            # Each scenario has an independent retry budget; never reset inside recovery.
            if args and args[0] in ('backup','update','restore','restart'):
                command(['systemctl','reset-failed',name+'.service'])
            return command(['bash',str(script),*args],expected=expected,env=env)
        def active():
            return command(['systemctl','is-active',name+'.service'],expected=None).stdout.strip()
        def release(version,binary):
            with zipfile.ZipFile(root/'release.zip','w') as archive:
                archive.writestr('bedrock_server',binary)
                archive.writestr('server.properties','server-name=default\n')
            (root/'available-version').write_text(version)
        release('1.0.0.0',GOOD_SERVER)
        run('configure')
        command(['systemctl','stop',name+'-update.timer',name+'-backup.timer',name+'-update.service'])
        command(['systemd-analyze','verify',*map(str,unit_paths)])
        results.append('generated units verified')
        # One failed listener followed by automatic recovery, managed only by systemd.
        (server/'failure-mode').write_text('first')
        first=run('start',expected=None)
        import time
        deadline=time.monotonic()+15
        while time.monotonic()<deadline and active()!='active':time.sleep(.2)
        assert active()=='active', first.stdout+first.stderr
        assert int((server/'launch-count').read_text())==2
        results.append('missing listener automatically recovered once')
        run('stop')
        command(['systemctl','reset-failed',name+'.service'])
        (server/'launch-count').unlink()
        (server/'failure-mode').write_text('always')
        run('start',expected=None)
        deadline=time.monotonic()+15
        while time.monotonic()<deadline:
            result=command(['systemctl','show',name+'.service','--property=Result','--value']).stdout.strip()
            if result=='start-limit-hit' or (active()=='failed' and int((server/'launch-count').read_text())==2):break
            time.sleep(.2)
        assert active()=='failed', result
        journal=command(['journalctl','-u',name+'.service','--no-pager','-o','cat']).stdout
        assert 'Start request repeated too quickly.' in journal
        assert int((server/'launch-count').read_text())==2
        assert len(list(Path('/var/lib/'+name+'-health').glob('failure-*.json')))>=2
        results.append('repeated listener failure stopped at systemd start limit with diagnostics')
        (server/'failure-mode').unlink();(server/'launch-count').unlink()
        command(['systemctl','reset-failed',name+'.service'])
        run('start'); assert active()=='active'
        invocation=command(['systemctl','show',name+'.service','--property=InvocationID','--value']).stdout
        run('restart'); assert active()=='active'
        assert invocation!=command(['systemctl','show',name+'.service','--property=InvocationID','--value']).stdout
        assert (server/'worlds/Test/db/shutdown-marker').exists()
        results.append('start, graceful stop and restart')
        backed=run('backup',expected=None)
        if backed.returncode:
            details=command(['systemctl','show',name+'.service','--property=ActiveState,Result,ExecMainStatus,ExecMainCode,MainPID']).stdout
            raise AssertionError(backed.stdout+backed.stderr+details)
        assert active()=='active'
        archive=next((root/'backups').glob('bds-server-*.tar.gz'))
        with tarfile.open(archive) as t:
            assert t.extractfile('server/worlds/Test/db/data').read()==b'seed'
        results.append('offline full backup and restart')
        release('1.1.0.0',GOOD_SERVER)
        run('update'); assert active()=='active'
        assert (server/'.installed-version').read_text().strip()=='1.1.0.0'
        results.append('update with stability confirmation')
        release('1.2.0.0',BAD_SERVER)
        failed=run('update',expected=None); assert failed.returncode!=0
        assert active()=='active'
        assert (server/'.installed-version').read_text().strip()=='1.1.0.0'
        assert (server/'worlds/Test/db/data').read_text()=='seed'
        retained=list(root.glob('bedrock-server.failed-update-*')); assert len(retained)==1
        assert (retained[0]/'worlds/Test/db/data').read_text().strip()=='converted'
        results.append('post-ready crash rolls back binary and world')
        run('restore',str(archive)); assert active()=='active'
        assert (server/'.installed-version').read_text().strip()=='1.0.0.0'
        results.append('full restore and stable restart')
        run('stop'); assert active()=='inactive'
        assert not (server/'.server.stdin').exists()
        status=run('status',expected=3); assert '退避データ:' in status.stdout
        results.append('stopped status retains exit code and displays storage')
        run('uninstall'); assert all(not p.exists() for p in unit_paths)
        assert (server/'worlds/Test/db/data').read_text()=='seed'
        results.append('uninstall after confirmed stop')
    finally:
        # All names were generated locally with a unique prefix; do not use unit wildcards.
        command(['systemctl','stop',*units],expected=None)
        for unit in units:
            state=command(['systemctl','show',unit,'--property=ActiveState','--value'],expected=None).stdout.strip()
            pid=command(['systemctl','show',unit,'--property=MainPID','--value'],expected=None).stdout.strip()
            if state not in ('inactive','failed','') or pid not in ('0',''):
                raise RuntimeError(f'Test service still running; preserve {root} and {unit} for inspection')
        command(['systemctl','disable','--runtime',*units],expected=None)
        for path in unit_paths: path.unlink(missing_ok=True)
        command(['systemctl','daemon-reload'])
        command(['systemctl','reset-failed',*units],expected=None)
        shutil.rmtree('/run/'+name+'-health',ignore_errors=True)
        shutil.rmtree('/var/lib/'+name+'-health',ignore_errors=True)
        shutil.rmtree(root)
    production_after=command(production_args,expected=None).stdout
    assert production_before==production_after, 'Production service state changed during the test; investigate separately'
    print(json.dumps({'checks':results,'production_service_unchanged':True,'temporary_units_removed':True},ensure_ascii=False,indent=2))

if __name__=='__main__': main()
