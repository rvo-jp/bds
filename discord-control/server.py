#!/usr/bin/env python3
"""Signed Discord interactions; privileges are confined to two fixed systemd jobs."""
import argparse
import asyncio
import fcntl
import json
import logging
import os
from pathlib import Path
import re
import secrets
import time

from aiohttp import ClientSession, ClientTimeout, web
from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

LOG = logging.getLogger('bds-control')
UNITS = {'restart': 'bds-control-restart.service', 'reboot': 'bds-control-reboot.service'}
ID = re.compile(r'[0-9]{1,20}\Z')
TOKEN = re.compile(r'[A-Za-z0-9._-]{1,512}\Z')


def load_config(path):
    config = json.loads(Path(path).read_text())
    for field in ('application_id', 'guild_id'):
        if not isinstance(config.get(field), str) or not ID.fullmatch(config[field]):
            raise ValueError('Invalid ' + field)
    key = bytes.fromhex(config['public_key'])
    if len(key) != 32:
        raise ValueError('Public Key must contain 32 bytes')
    for field in ('allowed_user_ids', 'allowed_role_ids'):
        values = config.get(field, [])
        if not isinstance(values, list) or any(not isinstance(v, str) or not ID.fullmatch(v) for v in values):
            raise ValueError('Invalid ' + field)
        config[field] = values
    if not config['allowed_user_ids'] and not config['allowed_role_ids']:
        raise ValueError('At least one allowed user or role is required')
    for field, default, low, high in [('port', 8765, 1024, 65535), ('bedrock_port', 19132, 1, 65535),
                                     ('cooldown_seconds', 300, 60, 86400)]:
        value = config.get(field, default)
        if type(value) is not int or not low <= value <= high:
            raise ValueError('Invalid ' + field)
        config[field] = value
    return config


def message(text, update=False):
    data = {'content': text, 'components': [], 'allowed_mentions': {'parse': []}}
    if not update:
        data['flags'] = 64
    return {'type': 7 if update else 4, 'data': data}


def boot_id():
    return Path('/proc/sys/kernel/random/boot_id').read_text().strip()


class State:
    def __init__(self, path):
        self.path = Path(path)
        self.data = json.loads(self.path.read_text()) if self.path.exists() else {'job': None, 'last_accepted': 0}
        if not isinstance(self.data, dict) or 'job' not in self.data or 'last_accepted' not in self.data:
            raise ValueError('Invalid control state; inspect instead of resetting cooldown')

    def save(self):
        temporary = self.path.with_suffix('.tmp')
        with temporary.open('w') as stream:
            json.dump(self.data, stream, ensure_ascii=False)
            stream.flush()
            os.fsync(stream.fileno())
        temporary.replace(self.path)
        descriptor = os.open(self.path.parent, os.O_RDONLY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)


async def command(*args):
    process = await asyncio.create_subprocess_exec(*args, stdout=asyncio.subprocess.PIPE,
                                                 stderr=asyncio.subprocess.DEVNULL)
    try:
        output, _ = await asyncio.wait_for(process.communicate(), 8)
    except asyncio.TimeoutError:
        process.kill()
        await process.wait()
        raise RuntimeError('systemctl timed out') from None
    if process.returncode:
        raise RuntimeError('systemctl request failed')
    return output.decode()


async def unit_status(unit):
    output = await command('/usr/bin/systemctl', 'show', unit, '--property=ActiveState,Result,Type,InvocationID')
    return dict(line.split('=', 1) for line in output.splitlines())


def operation_result(operation):
    try:
        result = json.loads(Path('/run/bds-control-' + operation + '/result.json').read_text())
    except FileNotFoundError:
        return None
    if (not isinstance(result, dict) or result.get('operation') != operation
            or result.get('phase') not in ('running', 'finished')
            or not re.fullmatch(r'[0-9a-f]{32}', str(result.get('invocation_id', '')))
            or not isinstance(result.get('boot_id'), str)):
        raise ValueError('Invalid operation result')
    return result


def supervised_ready(status, port, path=Path('/run/bds-health/status.json'), now=time.time):
    try:
        data = json.loads(path.read_text())
        age = now() - data['checked_at']
        return (status.get('Type') == 'notify' and status.get('ActiveState') == 'active'
                and bool(status.get('InvocationID'))
                and data['invocation_id'] == status['InvocationID']
                and data['ready'] is True and data['port'] == port and 0 <= age <= 15)
    except (OSError, ValueError, KeyError, TypeError):
        return False


class Control:
    def __init__(self, config, state, *, now=time.time, boot=boot_id):
        self.config, self.state, self.now, self.boot = config, state, now, boot
        self.key = Ed25519PublicKey.from_public_bytes(bytes.fromhex(config['public_key']))
        self.confirmations = {}
        self.responses = {}
        self.worker = None
        self.wake = asyncio.Event()

    def verify(self, headers, body):
        timestamp = headers.get('X-Signature-Timestamp', '')
        signature = headers.get('X-Signature-Ed25519', '')
        try:
            if not re.fullmatch(r'[0-9]{1,12}', timestamp) or abs(self.now() - int(timestamp)) > 300:
                return False
            self.key.verify(bytes.fromhex(signature), timestamp.encode('ascii') + body)
            return True
        except (InvalidSignature, ValueError, UnicodeError):
            return False

    def actor(self, interaction):
        if interaction.get('application_id') != self.config['application_id'] or interaction.get('guild_id') != self.config['guild_id']:
            return None
        member = interaction.get('member')
        if not isinstance(member, dict) or not isinstance(member.get('user'), dict):
            return None
        user = member['user'].get('id')
        roles = member.get('roles', [])
        if not isinstance(user, str) or not ID.fullmatch(user) or not isinstance(roles, list) or any(not isinstance(r, str) for r in roles):
            return None
        if user in self.config['allowed_user_ids'] or set(roles).intersection(self.config['allowed_role_ids']):
            return user
        return None

    def handle(self, interaction):
        now = self.now()
        if interaction.get('type') == 1:
            return {'type': 1}
        actor = self.actor(interaction)
        if actor is None:
            return message('このサーバーを操作する権限がありません。')
        interaction_id = interaction.get('id', '')
        if not isinstance(interaction_id, str) or not ID.fullmatch(interaction_id):
            return message('要求のIDが不正です。')
        self.confirmations = {k: v for k, v in self.confirmations.items() if v['expires'] >= now}
        self.responses = {k: v for k, v in self.responses.items() if v[0] >= now}
        if interaction_id in self.responses:
            return self.responses[interaction_id][1]
        job = self.state.data['job']
        if job and interaction_id == job['interaction_id']:
            return message('この要求は受け付け済みです。', update=interaction.get('type') == 3)
        data = interaction.get('data')
        if not isinstance(data, dict):
            return message('要求が不正です。')
        if interaction['type'] == 2:
            operation = data.get('name')
            if operation not in UNITS or data.get('options'):
                return message('利用できるコマンドは /restart と /reboot です。')
            busy = self.busy(now)
            if busy:
                return message(busy)
            if len(self.confirmations) >= 100 or len(self.responses) >= 500:
                return message('要求が多いため、少し待ってから操作してください。')
            nonce = secrets.token_urlsafe(18)
            self.confirmations[nonce] = {'actor': actor, 'operation': operation, 'expires': now + 60}
            target = 'Minecraftサーバー' if operation == 'restart' else 'VPSのOS全体'
            response = message(f'{target}を、ゲーム内への予告後に再起動します。実行しますか？（確認期限60秒）')
            response['data']['components'] = [{'type': 1, 'components': [
                {'type': 2, 'style': 4, 'label': '再起動する', 'custom_id': 'confirm:' + nonce},
                {'type': 2, 'style': 2, 'label': 'キャンセル', 'custom_id': 'cancel:' + nonce}]}]
        elif interaction['type'] == 3 and data.get('component_type') == 2:
            custom_id = data.get('custom_id', '')
            if not isinstance(custom_id, str):
                return message('確認要求が不正です。')
            action, separator, nonce = custom_id.partition(':')
            confirmation = self.confirmations.get(nonce) if separator else None
            if action not in ('confirm', 'cancel') or not confirmation or confirmation['actor'] != actor:
                return message('確認の期限が切れたか、操作した本人ではありません。もう一度コマンドを実行してください。')
            if action == 'cancel':
                self.confirmations.pop(nonce)
                response = message('再起動をキャンセルしました。', update=True)
            else:
                busy = self.busy(now)
                if busy:
                    return message(busy)
                token = interaction.get('token', '')
                if not isinstance(token, str) or not TOKEN.fullmatch(token):
                    return message('応答用トークンが不正です。')
                self.state.data['job'] = {'interaction_id': interaction_id, 'actor': actor,
                    'operation': confirmation['operation'], 'token': token, 'created': now,
                    'boot_id': self.boot(), 'phase': 'accepted', 'previous_result': None}
                self.state.data['last_accepted'] = now
                self.state.save()  # Never acknowledge before the receipt and cooldown reach disk.
                self.confirmations.pop(nonce)
                self.wake.set()
                LOG.info('accepted operation=%s user=%s request=%s', confirmation['operation'], actor, interaction_id)
                response = message('再起動要求を受け付けました。実行結果をここに表示します。', update=True)
        else:
            return message('この操作には対応していません。')
        self.responses[interaction_id] = (now + 300, response)
        return response

    def busy(self, now):
        if self.state.data['job']:
            return '別の再起動要求を処理中です。'
        remaining = self.config['cooldown_seconds'] - (now - self.state.data['last_accepted'])
        if remaining > 0:
            return f'連続実行を防ぐため、あと{int(remaining) + 1}秒待ってください。'
        return None

    async def endpoint(self, request):
        body = await request.read()
        if not self.verify(request.headers, body):
            raise web.HTTPUnauthorized()
        try:
            interaction = json.loads(body)
            if not isinstance(interaction, dict):
                raise ValueError()
            return web.json_response(self.handle(interaction))
        except (ValueError, KeyError, TypeError):
            raise web.HTTPBadRequest() from None
        except OSError:
            LOG.critical('Cannot persist request; refusing execution and restarting receiver')
            asyncio.get_running_loop().call_soon(os._exit, 1)
            raise web.HTTPServiceUnavailable() from None

    async def reply(self, job, text):
        if self.now() - job['created'] >= 14 * 60:
            LOG.error('result delivery expired request=%s', job['interaction_id'])
            return
        url = f"https://discord.com/api/v10/webhooks/{self.config['application_id']}/{job['token']}/messages/@original"
        payload = {'content': text, 'components': [], 'allowed_mentions': {'parse': []}}
        async with ClientSession(timeout=ClientTimeout(total=10)) as session:
            for attempt in range(3):
                try:
                    async with session.patch(url, json=payload, allow_redirects=False) as response:
                        if 200 <= response.status < 300:
                            return
                        status = response.status
                        delay = 2
                        if status == 429:
                            result = await response.json(content_type=None)
                            delay = min(max(float(result.get('retry_after', 2)), 0), 10)
                        elif status < 500:
                            break
                except Exception:
                    # Exception messages may contain the interaction token; never log them.
                    status, delay = 'network', 2
                if attempt < 2:
                    await asyncio.sleep(delay)
        LOG.error('result delivery failed status=%s request=%s', status, job['interaction_id'])

    async def recovered_reboot(self, job):
        if job['operation'] != 'reboot':
            return '処理中にOSが再起動されたため、Minecraft再起動の結果を確認できません。'
        stable_since = None
        deadline = asyncio.get_running_loop().time() + 900
        while asyncio.get_running_loop().time() < deadline:
            status = await unit_status('bds.service')
            ready = supervised_ready(status, self.config['bedrock_port'])
            if ready:
                stable_since = stable_since or asyncio.get_running_loop().time()
                if asyncio.get_running_loop().time() - stable_since >= 15:
                    return 'OSの再起動とMinecraftサーバーの復帰を確認しました。'
            else:
                stable_since = None
            await asyncio.sleep(2)
        return 'OSは再起動しましたが、Minecraftサーバーの復帰を確認できません。管理者がログを確認してください。'

    async def execute(self, job, recovered=False):
        unit = UNITS[job['operation']]
        if self.boot() != job['boot_id']:
            return await self.recovered_reboot(job)
        if not recovered:
            before = await unit_status(unit)
            if before['ActiveState'] in ('active', 'activating', 'deactivating'):
                return '同じ再起動処理が実行中のため、要求を実行しませんでした。'
            previous = operation_result(job['operation'])
            job['previous_result'] = previous['invocation_id'] if previous else None
            job['phase'] = 'dispatched'
            self.state.save()
            await command('/usr/bin/sudo', '-n', '/usr/bin/systemctl', '--no-block', 'start', unit)
        elif job['phase'] == 'accepted':
            return '受付サービスが中断したため、要求は再実行しませんでした。状態を確認してから再操作してください。'
        elif 'previous_result' not in job:
            return '旧形式の実行記録のため、結果を確認できません。要求は再実行しません。'
        deadline = asyncio.get_running_loop().time() + 1200
        while asyncio.get_running_loop().time() < deadline:
            result = operation_result(job['operation'])
            observed = (result is not None and result['boot_id'] == job['boot_id']
                        and result['invocation_id'] != job['previous_result'])
            if observed and result['phase'] == 'finished':
                if (result.get('service_result'), result.get('exit_code'), result.get('exit_status')) != ('success', 'exited', '0'):
                    return '再起動を完了できませんでした。更新・バックアップとの競合や停止失敗の詳細は管理者がログを確認してください。'
                if job['operation'] == 'restart':
                    return 'Minecraftサーバーの再起動が完了しました。'
                # reboot is only a request until a different boot ID is observed on startup.
            status = await unit_status(unit)
            if observed and status['ActiveState'] == 'failed':
                return '再起動処理または実行結果の記録に失敗しました。管理者がログを確認してください。'
            await asyncio.sleep(1)
        return '再起動の完了を確認できません。自動で再実行せず、管理者が状態を確認してください。'

    async def work(self):
        recovered = self.state.data['job'] is not None
        if recovered:
            self.wake.set()
        while True:
            await self.wake.wait()
            self.wake.clear()
            job = self.state.data['job']
            if job is None:
                continue
            try:
                text = await self.execute(job, recovered=recovered)
            except asyncio.CancelledError:
                raise  # Preserve the receipt for a new process/boot; never repeat the operation.
            except Exception:
                text = '要求の実行結果を確認できません。管理者がサービスの状態とログを確認してください。'
                LOG.error('operation check failed request=%s', job['interaction_id'])
            LOG.info('finished operation=%s user=%s request=%s result=%s', job['operation'], job['actor'], job['interaction_id'], text)
            await self.reply(job, text)
            self.state.data['job'] = None
            self.state.save()  # Remove the short-lived Discord token after reporting.
            recovered = False


def make_app(control):
    app = web.Application(client_max_size=32768)
    app.router.add_post('/interactions', control.endpoint)
    async def lifespan(app):
        control.worker = asyncio.create_task(control.work())
        def worker_finished(task):
            if not task.cancelled():
                LOG.critical('Background worker stopped; restarting receiver without repeating requests')
                os._exit(1)
        control.worker.add_done_callback(worker_finished)
        yield
        control.worker.cancel()
        try:
            await control.worker
        except asyncio.CancelledError:
            pass
    app.cleanup_ctx.append(lifespan)
    return app


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', default='/etc/bds-discord-control.json')
    parser.add_argument('--state-dir', default='/var/lib/bds-discord-control')
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    config = load_config(args.config)
    if args.check:
        print('Discord control configuration is valid.')
        return
    os.umask(0o077)
    state_dir = Path(args.state_dir)
    state_dir.mkdir(parents=True, exist_ok=True)
    with (state_dir / 'server.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        control = Control(config, State(state_dir / 'state.json'))
        logging.basicConfig(level=logging.INFO, format='%(name)s %(levelname)s %(message)s')
        # HTTPS termination is external; never expose this plain HTTP port to the network.
        web.run_app(make_app(control), host='127.0.0.1', port=config['port'], access_log=None, print=None)


if __name__ == '__main__':
    main()
