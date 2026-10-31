#!/usr/bin/env python3
"""Register only the two guild commands. The Bot Token is read without echo."""
import argparse
import getpass
import json
import urllib.error
import urllib.request

from server import load_config


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', default='/etc/bds-discord-control.json')
    args = parser.parse_args()
    config = load_config(args.config)
    token = getpass.getpass('Discord Bot Token（表示・保存しません）: ').strip()
    if not token:
        raise SystemExit('Token is required')
    url = f"https://discord.com/api/v10/applications/{config['application_id']}/guilds/{config['guild_id']}/commands"
    for name, description in [('restart', '予告後にMinecraftサーバーを再起動します'),
                              ('reboot', '予告・正常停止後にVPSのOSを再起動します')]:
        payload = {'name': name, 'type': 1, 'description': description, 'default_member_permissions': '0'}
        request = urllib.request.Request(url, data=json.dumps(payload).encode(), method='POST',
            headers={'Authorization': 'Bot ' + token, 'Content-Type': 'application/json',
                     'User-Agent': 'BDS-Discord-Control/1.0'})
        try:
            with urllib.request.urlopen(request, timeout=20) as response:
                result = json.load(response)
                if result.get('name') != name:
                    raise SystemExit('Unexpected command registration response')
        except urllib.error.HTTPError as error:
            raise SystemExit(f'Command registration failed: HTTP {error.code}') from None
        except (urllib.error.URLError, TimeoutError):
            raise SystemExit('Command registration failed: network error') from None
        print('/' + name + ' を登録しました。')
    print('Discordのサーバー設定 → 連携サービスで、両コマンドを許可するメンバーまたはロールを設定してください。')


if __name__ == '__main__':
    main()
