"""Root-only Linux/systemd probe. Executes only true/false in an isolated unit."""
import asyncio,importlib.util,json,pathlib,shutil,subprocess,tempfile
from unittest.mock import patch
source=pathlib.Path(__file__).resolve().parents[1]/'discord-control'
payload={name:(source/name).read_text() for name in ('server.py','result.py')}
unit='bds-review-result-probe.service'
unitpath=pathlib.Path('/run/systemd/system')/unit
resultdir=pathlib.Path('/run/bds-review-result-restart')
assert not unitpath.exists() and not resultdir.exists()
work=pathlib.Path(tempfile.mkdtemp(prefix='bds-control-review-'))
def run(*args): return subprocess.run(args,check=True,capture_output=True,text=True).stdout
# No BDS service is required for this isolated probe.
try:
    (work/'server.py').write_text(payload['server.py'].replace('/run/bds-control-', '/run/bds-review-result-'))
    (work/'result.py').write_text(payload['result.py'].replace('/run/bds-control-', '/run/bds-review-result-'))
    spec=importlib.util.spec_from_file_location('tested_control',work/'server.py'); module=importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
    module.UNITS={'restart':unit}
    config={'public_key':'00'*32,'application_id':'100','guild_id':'200','allowed_user_ids':['300'],'allowed_role_ids':[],'bedrock_port':19132,'cooldown_seconds':300}
    state=module.State(work/'state.json'); control=module.Control(config,state)
    async def exercise(program,expected):
        unitpath.write_text(f'[Unit]\nDescription=Harmless control result review probe\nAfter=network-online.target\n[Service]\nType=oneshot\nRuntimeDirectory=bds-review-result-restart\nRuntimeDirectoryMode=0755\nRuntimeDirectoryPreserve=yes\nExecStartPre=/usr/bin/python3 {work}/result.py restart running\nExecStart=/usr/bin/{program}\nExecStopPost=/usr/bin/python3 {work}/result.py restart finished\n')
        run('systemctl','daemon-reload')
        job={'operation':'restart','boot_id':module.boot_id(),'phase':'accepted','previous_result':None}
        state.data['job']=job
        result=await asyncio.wait_for(control.execute(job),15)
        assert expected in result, result
        record=module.operation_result('restart')
        print(program, result, 'receipt',record,flush=True)
        assert run('runuser','-u','nobody','--','cat',str(resultdir/'result.json'))
        if program=='true':
            await asyncio.sleep(1)
            props=run('systemctl','show',unit,'--property=ActiveState,ExecMainStartTimestampMonotonic')
            assert 'ExecMainStartTimestampMonotonic=0' in props,props
            with patch.object(module,'command',side_effect=AssertionError('must not dispatch recovered operation')):
                assert '完了しました' in await control.execute(job,recovered=True)
            print('GC + receiver recovery passed',flush=True)
    asyncio.run(exercise('true','完了しました'))
    asyncio.run(exercise('true','完了しました'))
    asyncio.run(exercise('false','完了できません'))
    print('systemd result smoke tests passed',flush=True)
finally:
    subprocess.run(['systemctl','reset-failed',unit],capture_output=True)
    unitpath.unlink(missing_ok=True)
    run('systemctl','daemon-reload')
    shutil.rmtree(resultdir,ignore_errors=True)
    shutil.rmtree(work)
