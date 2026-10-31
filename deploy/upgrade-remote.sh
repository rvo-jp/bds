# Sourced over SSH after the commit-bound install_bundle function.
set -eu
[ "$#" = 2 ] || exit 1
bundle=$1; commit=$2
[ "$(id -u)" = 0 ] || { echo 'rootで実行してください' >&2; exit 1; }
command -v flock >/dev/null
exec 9>/run/bdsctl-deploy.lock
flock -n 9 || { echo '別の配布作業が実行中です' >&2; exit 1; }
# All validation, configuration parsing and unit generation happen before stopping.
(install_bundle --preflight "$bundle" "$commit")
ctl=/usr/local/bin/bdsctl
[ -x "$ctl" ] && [ -f /usr/local/share/bdsctl/BUILD.json ] || { echo '通常更新は新構成へ移行した後に使用してください' >&2; exit 1; }
# The deployment layout is fixed; reject custom layouts rather than guarding the wrong state.
"$bundle/bdsctl" deployment-check
record=/var/lib/bds/state/deployment.txt
[ ! -e "$record" ] || { echo "$record に前回の作業記録があります。復旧手順を確認してください" >&2; exit 1; }
for unit in bds-daily.service bds-update.service bds-discord-restart.service bds-discord-reboot.service; do
    state=$(systemctl show "$unit" -p ActiveState --value)
    case "$state" in inactive|failed) ;; *) echo "$unit が実行中です" >&2; exit 1;; esac
done
daily=$(systemctl show bds-daily.timer -p ActiveState --value)
update=$(systemctl show bds-update.timer -p ActiveState --value)
discord=$(systemctl show bds-discord.service -p ActiveState --value)
for state in "$daily" "$update" "$discord"; do
    case "$state" in active|inactive|failed) ;; *) echo '起動・停止の途中です' >&2; exit 1;; esac
done
umask 077
# Record before stopping so an interrupted SSH session leaves an actionable receipt.
printf '%s\n' "commit=$commit" "bundle=$bundle" "previous=$($ctl --version)" "daily=$daily" "update=$update" "discord=$discord" 'phase=entering-maintenance' > "$record"
"$ctl" maintenance-begin
systemctl stop bds-daily.timer bds-update.timer bds-discord.service
printf '%s\n' 'phase=installing' >> "$record"
(install_bundle "$bundle" "$commit")
printf '%s\n' 'phase=installed' >> "$record"
printf '%s\n' 'phase=resuming-services' >> "$record"
for pair in "bds-discord.service:$discord" "bds-daily.timer:$daily" "bds-update.timer:$update"; do
    unit=${pair%:*}; state=${pair##*:}
    if [ "$state" = active ]; then systemctl start "$unit"; fi
done
# Scheduled jobs and Discord can resume while the maintenance guard still blocks actions.
# End last, so failure to restore a service leaves the server protected.
"$ctl" maintenance-end
"$ctl" status
rm "$record"
printf '%s\n' '更新完了。元から停止していたサービスは起動せず、有効化設定も変更していません。アプリから接続を確認してください。'
