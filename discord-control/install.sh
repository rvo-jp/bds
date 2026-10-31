#!/usr/bin/env bash
set -euo pipefail
[[ "$(id -u)" == 0 ]] || { echo 'rootで実行してください。' >&2; exit 1; }
base="$(cd "$(dirname "$0")" && pwd)"
[[ "$base" == /opt/bds/discord-control ]] || { echo '/opt/bds/discord-controlへ配置してください。' >&2; exit 1; }
[[ -f /etc/bds-discord-control.json ]] || { echo '先に /etc/bds-discord-control.json を設定してください。' >&2; exit 1; }
command -v visudo >/dev/null
/usr/bin/python3 "$base/server.py" --check
bash -n /opt/bds/bds.sh
# The receiver must never be able to alter a root-executed program, unit, or shell config.
python3 - <<'PY'
from pathlib import Path
paths=[Path('/opt'),Path('/opt/bds'),Path('/opt/bds/bds.sh'),Path('/opt/bds/bds.conf'),Path('/opt/bds/discord-control')]
paths += list(Path('/opt/bds/discord-control').iterdir())
for path in paths:
    info=path.stat()
    if path.is_symlink() or info.st_uid != 0 or info.st_mode & 0o022:
        raise SystemExit('root所有・他ユーザー書き込み不可の実パスにしてください: '+str(path))
PY
if ! id bds-discord-control >/dev/null 2>&1; then
    useradd --system --user-group --home-dir /var/lib/bds-discord-control --shell /usr/sbin/nologin bds-discord-control
fi
chown root:bds-discord-control /etc/bds-discord-control.json
chmod 0640 /etc/bds-discord-control.json
runuser -u bds-discord-control -- /usr/bin/python3 "$base/server.py" --check
visudo -cf "$base/sudoers"
install -o root -g root -m 0440 "$base/sudoers" /etc/sudoers.d/bds-discord-control
for unit in bds-discord-control.service bds-control-restart.service bds-control-reboot.service; do
    install -o root -g root -m 0644 "$base/$unit" "/etc/systemd/system/$unit"
done
systemctl daemon-reload
systemd-analyze verify /etc/systemd/system/bds-discord-control.service /etc/systemd/system/bds-control-restart.service /etc/systemd/system/bds-control-reboot.service
systemctl enable --now bds-discord-control.service
echo '受付は127.0.0.1:8765で起動しました。HTTPSの公開設定とDiscordコマンド登録を行ってください。'
