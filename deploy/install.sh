#!/bin/sh
# Run locally or stream over SSH: sh -s -- [--check|--preflight] BUNDLE_DIR COMMIT
# No checkout, compiler or source files are required on the target.
set -eu
check_only=false
preflight=false
if [ "${1:-}" = --preflight ]; then preflight=true; shift; fi
if [ "${1:-}" = --check ]; then check_only=true; shift; fi
[ "$#" = 2 ] || { echo 'Usage: install.sh [--check] BUNDLE_DIR EXPECTED_COMMIT' >&2; exit 1; }
artifact_dir=$(cd "$1" && pwd)
expected_commit=$2
case "$expected_commit" in *[!0-9a-f]*|'') echo '40桁のコミットIDが必要です' >&2; exit 1;; esac
[ "${#expected_commit}" = 40 ] || { echo '40桁のコミットIDが必要です' >&2; exit 1; }
[ "$(uname -s)" = Linux ] && [ "$(uname -m)" = x86_64 ] || { echo 'x86_64 Linux専用です' >&2; exit 1; }
(cd "$artifact_dir" && sha256sum --check --strict SHA256SUMS)
version=$("$artifact_dir/bdsctl" --version)
case "$version" in bdsctl\ *" ($expected_commit)") ;; *) echo 'バイナリと指定コミットが一致しません' >&2; exit 1;; esac
if "$check_only"; then printf '%s\n' "成果物検証成功: ${version}（インストール未実行）"; exit 0; fi
[ "$(id -u)" = 0 ] || { echo 'rootで実行してください' >&2; exit 1; }
for command in systemctl install rsync zstd getent useradd groupadd usermod chown pkcheck; do
    command -v "$command" >/dev/null || { echo "$command が必要です" >&2; exit 1; }
done
if ! "$preflight"; then
for unit in bds.service bds-discord.service bds-discord-control.service bds-player-notify.service bds-backup.service bds-update.service bds-control-restart.service bds-control-reboot.service bds-discord-restart.service bds-discord-reboot.service bds-daily.service bds-update-check.service bds-backup.timer bds-update.timer bds-daily.timer bds-update-check.timer; do
    if load=$(systemctl show "$unit" --property=LoadState --value 2>/dev/null); then :; elif [ "$load" != not-found ]; then echo "$unit の状態を取得できません" >&2; exit 1; fi
    [ "$load" != not-found ] || continue
    state=$(systemctl show "$unit" --property=ActiveState --value)
    case "$state" in active|activating|deactivating|reloading) echo "$unit を停止してから実行してください" >&2; exit 1;; esac
    pid=$(systemctl show "$unit" --property=MainPID --value)
    case "$pid" in ''|0) ;; *) echo "$unit にプロセスが残っています" >&2; exit 1;; esac
    cgroup=$(systemctl show "$unit" --property=ControlGroup --value)
    if [ -n "$cgroup" ] && [ -r "/sys/fs/cgroup$cgroup/cgroup.procs" ] && [ -n "$(cat "/sys/fs/cgroup$cgroup/cgroup.procs")" ]; then
        echo "$unit に子プロセスが残っています" >&2; exit 1
    fi
done
fi
[ -f /etc/bds/config.toml ] || { echo '/etc/bds/config.tomlを先に用意してください' >&2; exit 1; }
staging=$(mktemp -d)
trap 'rm -rf "$staging"' EXIT HUP INT TERM
"$artifact_dir/bdsctl" generate-units "$staging" /usr/local/bin/bdsctl
# Never follow an existing mask/link when installing or saving the previous files.
files="/usr/local/bin/bdsctl /usr/local/share/bdsctl/BUILD.json /etc/polkit-1/rules.d/50-bds-discord.rules /etc/systemd/system/bds-update-check.timer /etc/systemd/system/bds-update-check.service"
for source in "$staging/"*.service "$staging/"*.timer; do files="$files /etc/systemd/system/$(basename "$source")"; done
for target in $files; do
    [ ! -L "$target" ] || { echo "$target はリンクです。管理者が配置を確認してください" >&2; exit 1; }
done
if "$preflight"; then printf '%s\n' '停止前の検証が完了しました。'; exit 0; fi
"$artifact_dir/bdsctl" prepare-directories
# Restore a consistent set of installed files on failure. No old version is retained after success.
for target in $files; do
    if [ -e "$target" ]; then mkdir -p "$staging/previous$(dirname "$target")"; cp -p "$target" "$staging/previous$target"; fi
done
committed=false
finish() {
    code=$?
    trap - EXIT HUP INT TERM
    rm -f /usr/local/bin/bdsctl.new
    if ! "$committed"; then
        echo '設置に失敗しました。変更前の管理プログラムとサービス定義を戻します。' >&2
        for target in $files; do
            if [ -e "$staging/previous$target" ]; then cp -p "$staging/previous$target" "$target" || true; else rm -f "$target"; fi
        done
        systemctl daemon-reload || true
    fi
    rm -rf "$staging"
    exit "$code"
}
trap finish EXIT
trap 'exit 130' HUP INT TERM
install -d -m 0755 /usr/local/bin /usr/local/share/bdsctl /etc/systemd/system /etc/polkit-1/rules.d
install -m 0755 "$artifact_dir/bdsctl" /usr/local/bin/bdsctl.new
mv /usr/local/bin/bdsctl.new /usr/local/bin/bdsctl
install -m 0644 "$artifact_dir/BUILD.json" /usr/local/share/bdsctl/BUILD.json
install -m 0644 "$staging/"*.service "$staging/"*.timer /etc/systemd/system/
install -m 0644 "$staging/50-bds-discord.rules" /etc/polkit-1/rules.d/50-bds-discord.rules
if [ -f /etc/systemd/system/bds-update-check.timer ]; then
    systemctl disable bds-update-check.timer
fi
rm -f /etc/systemd/system/bds-update-check.timer /etc/systemd/system/bds-update-check.service
systemctl daemon-reload
committed=true
printf '%s\n' 'インストール完了。サービスの起動・有効化は行っていません。'
