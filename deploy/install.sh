#!/bin/sh
# Installs reviewed artifacts and unit files only. Never starts or enables BDS.
set -eu
[ "$(id -u)" = 0 ] || { echo 'rootで実行してください' >&2; exit 1; }
cd "$(dirname "$0")/.."
artifact_dir=$(cd "${1:?検証済み成果物のディレクトリを指定してください}" && pwd)
[ "$(uname -m)" = x86_64 ] || { echo 'x86_64 Linux専用です' >&2; exit 1; }
for unit in bds.service bds-discord.service bds-discord-control.service; do
    state=$(systemctl show "$unit" --property=ActiveState --value)
    case "$state" in active|activating|deactivating|reloading) echo "$unit を停止してから実行してください" >&2; exit 1;; esac
done
(cd "$artifact_dir" && sha256sum --check SHA256SUMS)
sha256sum --check "$artifact_dir/SOURCES.sha256"
[ -f /etc/bds/config.toml ] || { echo '/etc/bds/config.tomlを先に用意してください' >&2; exit 1; }
install -m 0755 "$artifact_dir/bdsctl-linux-x86_64" /usr/local/bin/bdsctl.new
mv /usr/local/bin/bdsctl.new /usr/local/bin/bdsctl
/usr/local/bin/bdsctl prepare-directories
staging=$(mktemp -d)
trap 'rm -rf "$staging"' EXIT HUP INT TERM
/usr/local/bin/bdsctl generate-units "$staging"
install -m 0644 "$staging/"*.service "$staging/"*.timer /etc/systemd/system/
install -m 0644 "$staging/50-bds-discord.rules" /etc/polkit-1/rules.d/50-bds-discord.rules
systemctl daemon-reload
printf '%s\n' 'インストール完了。サービスの起動・有効化は行っていません。移行手順を確認してください。'
