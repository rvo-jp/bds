"""Real Ed25519/HTTP parsing; privileged actions and Discord sends are mocked."""
import asyncio
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import AsyncMock, patch

try:
    from aiohttp.test_utils import TestClient, TestServer
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
    from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat
    AVAILABLE = True
except ImportError:
    AVAILABLE = False

if AVAILABLE:
    spec=importlib.util.spec_from_file_location('discord_control_server',Path(__file__).resolve().parents[1]/'discord-control/server.py')
    module=importlib.util.module_from_spec(spec); spec.loader.exec_module(module)


@unittest.skipUnless(AVAILABLE,'Install discord-control/requirements.txt to run HTTP/signature tests')
class DiscordControlTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        self.root=Path(self.tmp.name)
        self.key=Ed25519PrivateKey.generate()
        self.config={'application_id':'100','guild_id':'200',
            'public_key':self.key.public_key().public_bytes(Encoding.Raw,PublicFormat.Raw).hex(),
            'allowed_user_ids':['300'],'allowed_role_ids':['400'],'port':8765,'bedrock_port':19132,'cooldown_seconds':300}
        self.result_patch = patch.object(module, 'operation_result', return_value=None)
        self.result_mock = self.result_patch.start(); self.addCleanup(self.result_patch.stop)
        self.clock=10000
        self.state=module.State(self.root/'state.json')
        self.control=module.Control(self.config,self.state,now=lambda:self.clock,boot=lambda:'boot-a')
        self.request={'id':'500','application_id':'100','guild_id':'200','type':2,
            'member':{'user':{'id':'300'},'roles':[]},'data':{'name':'restart'},'token':'test.token'}

    def confirmation(self, operation='restart'):
        self.request['data']['name']=operation
        response=self.control.handle(self.request)
        custom_id=response['data']['components'][0]['components'][0]['custom_id']
        click=copy.deepcopy(self.request)
        click.update(id='501',type=3,data={'component_type':2,'custom_id':custom_id})
        return click

    def test_same_allowlist_authorizes_both_commands_and_role_members(self):
        for index,operation in enumerate(('restart','reboot')):
            request=copy.deepcopy(self.request); request['id']=str(600+index); request['data']['name']=operation
            request['member']={'user':{'id':'301'},'roles':['400']}
            response=self.control.handle(request)
            self.assertTrue(response['data']['components'])
        self.assertIsNone(self.state.data['job'])

    def test_unauthorized_wrong_server_dm_and_wrong_app_are_rejected(self):
        for changes in ({'guild_id':'999'},{'application_id':'999'},
                        {'member':{'user':{'id':'999'},'roles':[]}},
                        {'guild_id':None,'member':None,'user':{'id':'300'}}):
            request=copy.deepcopy(self.request); request.update(changes)
            self.assertIn('権限',self.control.handle(request)['data']['content'])
            self.assertIsNone(self.state.data['job'])

    def test_confirmation_is_bound_to_requester_and_expires(self):
        click=self.confirmation()
        other=copy.deepcopy(click); other['member']['user']['id']='301'; other['member']['roles']=['400']
        self.assertIn('本人',self.control.handle(other)['data']['content'])
        self.assertIsNone(self.state.data['job'])
        self.clock+=61
        self.assertIn('期限',self.control.handle(click)['data']['content'])
        self.assertIsNone(self.state.data['job'])

    def test_permissions_are_rechecked_when_confirming(self):
        click=self.confirmation(); self.config['allowed_user_ids']=[]
        self.assertIn('権限',self.control.handle(click)['data']['content'])
        self.assertIsNone(self.state.data['job'])

    def test_acceptance_is_persisted_and_duplicate_click_is_not_reexecuted(self):
        click=self.confirmation('reboot')
        response=self.control.handle(click)
        self.assertEqual(response['type'],7)
        saved=json.loads((self.root/'state.json').read_text())
        self.assertEqual(saved['job']['operation'],'reboot')
        self.assertEqual(saved['job']['actor'],'300')
        self.assertEqual(self.control.handle(click),response)
        self.assertEqual(saved,json.loads((self.root/'state.json').read_text()))

    def test_failed_persistence_never_acknowledges_or_wakes_worker(self):
        click=self.confirmation()
        with patch.object(self.state,'save',side_effect=OSError('disk full')):
            with self.assertRaises(OSError): self.control.handle(click)
        self.assertFalse(self.control.wake.is_set())

    def test_busy_and_cooldown_apply_to_both_commands_after_restart(self):
        self.control.handle(self.confirmation())
        request=copy.deepcopy(self.request); request['id']='502'; request['data']['name']='reboot'
        self.assertIn('処理中',self.control.handle(request)['data']['content'])
        self.state.data['job']=None; self.state.save()
        restored=module.Control(self.config,module.State(self.root/'state.json'),now=lambda:self.clock,boot=lambda:'boot-b')
        self.assertIn('連続実行',restored.handle(request)['data']['content'])
        self.clock+=301
        self.assertTrue(restored.handle(request)['data']['components'])

    def test_cancel_does_not_execute(self):
        click=self.confirmation(); click['data']['custom_id']=click['data']['custom_id'].replace('confirm:','cancel:')
        self.assertIn('キャンセル',self.control.handle(click)['data']['content'])
        self.assertIsNone(self.state.data['job'])

    def test_signatures_reject_modified_body_wrong_key_and_stale_timestamp(self):
        body=json.dumps(self.request).encode(); timestamp=str(self.clock)
        headers={'X-Signature-Timestamp':timestamp,'X-Signature-Ed25519':self.key.sign(timestamp.encode()+body).hex()}
        self.assertTrue(self.control.verify(headers,body))
        self.assertFalse(self.control.verify(headers,body+b' '))
        self.assertFalse(self.control.verify({},body))
        headers['X-Signature-Ed25519']=Ed25519PrivateKey.generate().sign(timestamp.encode()+body).hex()
        self.assertFalse(self.control.verify(headers,body))
        headers['X-Signature-Ed25519']=self.key.sign(timestamp.encode()+body).hex()
        self.clock+=301; self.assertFalse(self.control.verify(headers,body))

    async def test_http_ping_and_invalid_signature_without_side_effects(self):
        client=TestClient(TestServer(module.make_app(self.control)))
        await client.start_server()
        try:
            body=b'{"type":1}'; timestamp=str(self.clock)
            headers={'X-Signature-Timestamp':timestamp,'X-Signature-Ed25519':self.key.sign(timestamp.encode()+body).hex()}
            response=await client.post('/interactions',data=body,headers=headers)
            self.assertEqual(response.status,200); self.assertEqual(await response.json(),{'type':1})
            response=await client.post('/interactions',data=body)
            self.assertEqual(response.status,401)
            self.assertIsNone(self.state.data['job'])
        finally: await client.close()

    async def test_worker_starts_only_fixed_unit_and_waits_for_its_result(self):
        self.control.handle(self.confirmation())
        job=self.state.data['job']
        before={'ActiveState':'inactive','Result':'success','ExecMainStartTimestampMonotonic':'1'}
        after={'ActiveState':'inactive','Result':'success','ExecMainStartTimestampMonotonic':'2'}
        self.result_mock.side_effect = [None, self.record()]
        with patch.object(module,'unit_status',AsyncMock(side_effect=[before,after])), patch.object(module,'command',AsyncMock()) as command:
            result=await self.control.execute(job)
        self.assertIn('完了',result)
        command.assert_awaited_once_with('/usr/bin/sudo','-n','/usr/bin/systemctl','--no-block','start','bds-control-restart.service')

    async def test_unit_failure_never_reports_success(self):
        self.control.handle(self.confirmation())
        statuses=[{'ActiveState':'inactive','Result':'success','ExecMainStartTimestampMonotonic':'1'},
                  {'ActiveState':'failed','Result':'exit-code','ExecMainStartTimestampMonotonic':'2'}]
        self.result_mock.side_effect = [None, self.record(service_result='exit-code', exit_status='1')]
        with patch.object(module,'unit_status',AsyncMock(side_effect=statuses)),patch.object(module,'command',AsyncMock()):
            result=await self.control.execute(self.state.data['job'])
        self.assertIn('完了できません',result)

    async def test_service_crash_before_dispatch_does_not_retry(self):
        self.control.handle(self.confirmation())
        with patch.object(module,'command',AsyncMock()) as command:
            result=await self.control.execute(self.state.data['job'],recovered=True)
        self.assertIn('再実行しません',result); command.assert_not_awaited()

    async def test_new_boot_checks_recovery_instead_of_requesting_another_reboot(self):
        self.control.handle(self.confirmation('reboot')); self.control.boot=lambda:'boot-b'
        with patch.object(self.control,'recovered_reboot',AsyncMock(return_value='recovered')) as recovery,patch.object(module,'command',AsyncMock()) as command:
            self.assertEqual(await self.control.execute(self.state.data['job'],recovered=True),'recovered')
        recovery.assert_awaited_once(); command.assert_not_awaited()

    def test_configuration_requires_an_explicit_allowlist(self):
        config=copy.deepcopy(self.config); config['allowed_user_ids']=[]; config['allowed_role_ids']=[]
        path=self.root/'config.json'; path.write_text(json.dumps(config))
        with self.assertRaises(ValueError): module.load_config(path)

    def test_reboot_receipt_requires_current_invocation_and_fresh_health(self):
        path=self.root/'health.json'
        data={'invocation_id':'current','ready':True,'port':19132,'checked_at':100.0}
        status={'Type':'notify','ActiveState':'active','InvocationID':'current'}
        path.write_text(json.dumps(data))
        self.assertTrue(module.supervised_ready(status,19132,path,now=lambda:105))
        self.assertFalse(module.supervised_ready(status,19132,path,now=lambda:120))
        self.assertFalse(module.supervised_ready(status,19132,path,now=lambda:99))
        for changes in ({'invocation_id':'previous'},{'ready':False},{'port':19133}):
            path.write_text(json.dumps(dict(data,**changes)))
            self.assertFalse(module.supervised_ready(status,19132,path,now=lambda:105))

    def test_reboot_receipt_rejects_old_unit_or_unreadable_state(self):
        path=self.root/'missing-health.json'
        status={'Type':'notify','ActiveState':'active','InvocationID':'current'}
        self.assertFalse(module.supervised_ready(status,19132,path))
        path.write_text('not json')
        self.assertFalse(module.supervised_ready(status,19132,path))
        path.write_text(json.dumps({'invocation_id':'current','ready':True,'port':19132,'checked_at':100.0}))
        self.assertFalse(module.supervised_ready(dict(status,Type='simple'),19132,path,now=lambda:105))

    def record(self, **changes):
        result = {'operation': 'restart', 'phase': 'finished', 'invocation_id': 'a'*32,
                  'boot_id': 'boot-a', 'service_result': 'success', 'exit_code': 'exited', 'exit_status': '0'}
        result.update(changes)
        return result

    async def test_success_survives_systemd_garbage_collection(self):
        self.control.handle(self.confirmation())
        self.result_mock.side_effect = [None, self.record()]
        empty = {'ActiveState': 'inactive', 'Result': 'success', 'ExecMainStartTimestampMonotonic': '0'}
        with patch.object(module, 'unit_status', AsyncMock(return_value=empty)), patch.object(module, 'command', AsyncMock()):
            self.assertIn('完了しました', await self.control.execute(self.state.data['job']))

    async def test_old_result_and_running_record_are_not_reported_as_success(self):
        self.control.handle(self.confirmation())
        old = self.record(invocation_id='b'*32)
        self.result_mock.side_effect = [old, old, self.record(phase='running'), self.record()]
        empty = {'ActiveState': 'inactive', 'Result': 'success'}
        with patch.object(module, 'unit_status', AsyncMock(return_value=empty)), patch.object(module, 'command', AsyncMock()), patch.object(module.asyncio, 'sleep', AsyncMock()) as sleep:
            self.assertIn('完了しました', await self.control.execute(self.state.data['job']))
        self.assertEqual(sleep.await_count, 2)

    async def test_receiver_restart_reads_saved_result_without_dispatch(self):
        self.control.handle(self.confirmation())
        job = self.state.data['job']; job.update(phase='dispatched', previous_result=None)
        self.result_mock.return_value = self.record()
        with patch.object(module, 'command', AsyncMock()) as command:
            self.assertIn('完了しました', await self.control.execute(job, recovered=True))
        command.assert_not_awaited()

    async def test_timeout_or_signal_is_not_success_even_with_zero_exit_status(self):
        self.control.handle(self.confirmation())
        for changes in ({'service_result': 'timeout'}, {'exit_code': 'killed', 'exit_status': 'TERM'}):
            self.result_mock.side_effect = [None, self.record(**changes)]
            with patch.object(module, 'unit_status', AsyncMock(return_value={'ActiveState': 'inactive'})), patch.object(module, 'command', AsyncMock()):
                self.assertIn('完了できません', await self.control.execute(self.state.data['job']))
