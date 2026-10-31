#!/usr/bin/env bash

set -euo pipefail

APP_NAME="bds"
SYSTEMD_DIR="/etc/systemd/system"
SCRIPT_PATH="$(readlink -f "$0" 2>/dev/null || realpath "$0")"
BASE_DIR="$(cd "$(dirname "$SCRIPT_PATH")" && pwd)"
CONFIG_FILE="${BDS_CONFIG:-$BASE_DIR/bds.conf}"

if [[ -f "$CONFIG_FILE" ]]; then
    # bds.conf は shell 設定ファイルとして読み込みます。heredoc による複数行設定も使えます。
    # shellcheck source=/dev/null
    source "$CONFIG_FILE"
fi

DEST="${BEDROCK_DIR:-$BASE_DIR/bedrock-server}"
# 派生パスを作る前に末尾の / を取り除く（ルート / 自体は検証で拒否）。
while [[ "$DEST" != / && "$DEST" == */ ]]; do DEST="${DEST%/}"; done
API_URLS=(
    "https://net-secondary.web.minecraft-services.net/api/v1.0/download/links"
    "https://net.web.minecraft-services.net/api/v1.0/download/links"
)
VERSION_FILE="$DEST/.installed-version"
URL_FILE="$DEST/.installed-url"
STDIN_FIFO="$DEST/.server.stdin"
NOTICE_SECONDS="${UPDATE_NOTICE_SECONDS:-300}"
BACKUP_DIR="${BACKUP_DIR:-$BASE_DIR/backups}"
BACKUP_RETENTION_DAYS="${BACKUP_RETENTION_DAYS:-14}"
BACKUP_ON_CALENDAR="${BACKUP_ON_CALENDAR:-*-*-* 04:30:00}"
BACKUP_NOTICE_SECONDS="${BACKUP_NOTICE_SECONDS:-60}"
SERVER_START_TIMEOUT_SECONDS="${SERVER_START_TIMEOUT_SECONDS:-${BACKUP_START_TIMEOUT_SECONDS:-120}}"
SERVER_STABLE_SECONDS="${SERVER_STABLE_SECONDS:-15}"
SERVER_HEALTH_INTERVAL_SECONDS="${SERVER_HEALTH_INTERVAL_SECONDS:-30}"
SERVER_HEALTH_RESPONSE_TIMEOUT_SECONDS="${SERVER_HEALTH_RESPONSE_TIMEOUT_SECONDS:-10}"
SERVER_HEALTH_FAILURES="${SERVER_HEALTH_FAILURES:-3}"
SERVER_RESTART_DELAY_SECONDS="${SERVER_RESTART_DELAY_SECONDS:-30}"
SERVER_RESTART_LIMIT="${SERVER_RESTART_LIMIT:-3}"
DISCORD_TIMEOUT_SECONDS="${DISCORD_TIMEOUT_SECONDS:-30}"
BACKUP_LOCK_WAIT_SECONDS="${BACKUP_LOCK_WAIT_SECONDS:-1800}"
BACKUP_MIN_FREE_MB="${BACKUP_MIN_FREE_MB:-1024}"
CONTROL_NOTICE_SECONDS="${CONTROL_NOTICE_SECONDS:-60}"
CONTROL_REBOOT_MARKER="${CONTROL_REBOOT_MARKER:-/run/$APP_NAME-reboot-pending}"
PLAYER_NOTIFY_ENABLED="${PLAYER_NOTIFY_ENABLED:-false}"
PLAYER_NOTIFY_STATE_DIR="${PLAYER_NOTIFY_STATE_DIR:-$BASE_DIR/.player-notify}"
SERVER_NAME_FORMAT="${SERVER_NAME_FORMAT-}"
CURL_USER_AGENT="${CURL_USER_AGENT:-Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36}"

usage() {
    cat <<EOF
使い方: $0 <コマンド>

コマンド:
  install          BDS 本体を新規インストールします。
  start            bds.service を起動します。
  stop             bds.service を停止します。
  restart          bds.service を再起動します。
  update           最新版を準備し、停止・切り替え・起動確認を行います。
  run              BDSを監督付きでフォアグラウンド起動します。
  send-stop        起動中の bedrock_server へ stop を送信します。
  command <cmd>    起動中の bedrock_server へワールド内コマンドを送信します。
  backup           停止中にサーバー一式をコピーし、再起動後に圧縮します。
  configure        BDS 本体を更新・再起動せずに systemd 設定を更新します。
  watch-players    BDS の参加・退出ログを監視して Discord へ通知します。
  control-restart  Discord連携用。予告後にBDSを正常停止・再起動します。
  control-reboot   Discord連携用。予告・BDS正常停止後にOSを再起動します。
  restore <archive>
                   サーバー一式、または worlds のバックアップを復元します。
  status [target]  状態を確認します。server は容量・退避数・最新バックアップも表示します。target: server/update/backup/player-notify/*-timer
  logs [target]    journal を表示します。target: server/update/backup/player-notify
  timers           bds 関連の systemd timer を一覧表示します。
  uninstall         systemd service と timer を削除します。

設定:
  bds.conf         任意の shell 設定ファイル。既定: $BASE_DIR/bds.conf
  BDS_CONFIG       設定ファイルのパスを上書きします。
  BEDROCK_DIR      インストール先ディレクトリ。既定: $BASE_DIR/bedrock-server
  CHECK_INTERVAL   更新確認 timer の間隔。既定: 6h
  UPDATE_NOTICE_SECONDS
                   更新前にプレイヤーへ警告して待つ秒数。既定: 300
  BACKUP_DIR       バックアップ先ディレクトリ。既定: $BASE_DIR/backups
  BACKUP_RETENTION_DAYS
                   バックアップ保持日数。既定: 14
  BACKUP_ON_CALENDAR
                   systemd のバックアップ実行時刻。既定: *-*-* 04:30:00
  BACKUP_NOTICE_SECONDS
                   バックアップのための停止を予告して待つ秒数。既定: 60
  SERVER_START_TIMEOUT_SECONDS
                   メンテナンス後の起動完了を待つ秒数。既定: 120
  SERVER_STABLE_SECONDS
                   全起動で受付・応答の安定を確認する秒数。既定: 15
  SERVER_HEALTH_INTERVAL_SECONDS / SERVER_HEALTH_RESPONSE_TIMEOUT_SECONDS
                   監視間隔/コンソール応答期限。既定: 30/10秒
  SERVER_HEALTH_FAILURES
                   障害と判断する連続失敗回数。既定: 3
  SERVER_RESTART_DELAY_SECONDS / SERVER_RESTART_LIMIT
                   自動再起動の間隔/10分間の起動回数上限。既定: 30秒/3回
  DISCORD_TIMEOUT_SECONDS
                   Discord送信・再試行・待機の合計上限秒数。既定: 30
  BACKUP_LOCK_WAIT_SECONDS
                   他のメンテナンスの完了を待つ秒数。既定: 1800
  BACKUP_MIN_FREE_MB
                   バックアップ前に確保する最低空き容量 MB。既定: 1024
  SERVER_NAME_FORMAT
                   server-name の形式。%v=プレイヤー向け表記、%V=BDSフル表記。未設定または空なら自動変更しません。
  PLAYER_NOTIFY_ENABLED
                   入退室通知サービスを有効にします。true/false。既定: false
  PLAYER_NOTIFY_STATE_DIR
                   入退室ログの読み取り位置の保存先。既定: $BASE_DIR/.player-notify
  DISCORD_WEBHOOK_URL
                   Discord 通知用 Webhook URL。未設定なら通知しません。
EOF
}

need_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "必要なコマンドが見つかりません: $1" >&2
        echo "Ubuntu では依存パッケージを入れてください: sudo apt update && sudo apt install -y curl jq python3 util-linux coreutils" >&2
        exit 1
    fi
}

need_cmds() {
    local cmd
    for cmd in "$@"; do
        need_cmd "$cmd"
    done
}

require_deps() {
    need_cmds curl jq python3
}

require_backup_deps() {
    need_cmds tar find date df du awk grep curl jq python3 mktemp cp realpath timeout
}

is_root() {
    [[ "$(id -u)" -eq 0 ]]
}

systemctl_run() {
    if is_root; then
        systemctl "$@"
    else
        sudo systemctl "$@"
    fi
}

systemd_unit_path() {
    printf '%s/%s\n' "$SYSTEMD_DIR" "$1"
}

systemctl_disable_now() {
    local unit load
    for unit in "$@"; do
        load="$(systemctl show "$unit" --property=LoadState --value)" || return 1
        [[ "$load" != not-found ]] || continue
        systemctl disable --now "$unit" || return 1
    done
}

systemctl_enable() {
    local unit
    for unit in "$@"; do
        systemctl enable "$unit"
    done
}

systemctl_enable_now() {
    local unit
    for unit in "$@"; do
        systemctl enable --now "$unit"
    done
}

service_unit_for_target() {
    local target="${1:-server}"

    case "$target" in
        server|service|bds|"")
            printf '%s\n' "$APP_NAME.service"
            ;;
        update)
            printf '%s\n' "$APP_NAME-update.service"
            ;;
        backup)
            printf '%s\n' "$APP_NAME-backup.service"
            ;;
        player-notify)
            printf '%s\n' "$APP_NAME-player-notify.service"
            ;;
        *.service)
            printf '%s\n' "$target"
            ;;
        *)
            echo "不明な service 対象です: $target" >&2
            exit 1
            ;;
    esac
}

systemd_unit_for_target() {
    local target="${1:-server}"

    case "$target" in
        update-timer)
            printf '%s\n' "$APP_NAME-update.timer"
            ;;
        backup-timer)
            printf '%s\n' "$APP_NAME-backup.timer"
            ;;
        *.timer)
            printf '%s\n' "$target"
            ;;
        *)
            service_unit_for_target "$target"
            ;;
    esac
}

service_control() {
    local action="${1:-status}"

    case "$action" in
        status)
            systemctl status "$APP_NAME.service"
            ;;
        start)
            start_and_wait
            ;;
        restart)
            [[ "$(service_property Type)" == notify ]] || { echo "先にconfigureを実行してください。" >&2; return 1; }
            if systemctl is-active --quiet "$APP_NAME.service"; then
                stop_for_maintenance || return 1
            else
                require_stopped_server || return 1
            fi
            start_and_wait
            ;;
        stop)
            systemctl_run stop "$APP_NAME.service"
            ;;
        *)
            echo "使い方: $0 <start|stop|restart>" >&2
            exit 1
            ;;
    esac
}

status_command() {
    local unit result=0
    unit="$(systemd_unit_for_target "${1:-server}")"
    systemctl status "$unit" || result=$?
    if [[ "$unit" == "$APP_NAME.service" ]]; then
        show_storage_status || echo "容量情報の集計に失敗しました。" >&2
    fi
    return "$result"
}

show_storage_status() {
    python3 - "$DEST" "$BACKUP_DIR" <<'PY'
import datetime, os, shutil, subprocess, sys
from pathlib import Path
server, backups = map(Path, sys.argv[1:])
errors = []
def items(directory):
    try:
        return list(directory.iterdir()) if directory.exists() else []
    except OSError:
        errors.append(str(directory)); return None

def size(path):
    if not path.exists(): return 0
    result = subprocess.run(['du', '-sk', '--', str(path)], capture_output=True, text=True)
    if result.returncode:
        errors.append(str(path)); return None
    try: return int(result.stdout.split()[0]) * 1024
    except (ValueError, IndexError):
        errors.append(str(path)); return None

def label(value): return '取得できません' if value is None else f'{value / (1024 * 1024):,.1f} MiB'
def regular(p): return not p.is_symlink() and p.is_file()
backup_items = items(backups)
archives = None if backup_items is None else [p for p in backup_items if regular(p) and
    p.name.startswith(('bds-server-', 'bds-worlds-')) and p.name.endswith('.tar.gz')]
parent_items = items(server.parent)
retained = None if parent_items is None else [p for p in parent_items if not p.is_symlink() and p.is_dir()
    and p.name.startswith((server.name + '.pre-', server.name + '.failed-'))]
retained_sizes = None if retained is None else [size(p) for p in retained]
retained_total = None if retained_sizes is None or None in retained_sizes else sum(retained_sizes)
print('\n運用データ（集計時点）:')
print('  サーバー: ' + label(size(server)))
count = lambda values: '件数不明' if values is None else f'{len(values)}件'
print(f'  バックアップ: {count(archives)} / {label(size(backups))}')
print(f'  退避データ: {count(retained)} / {label(retained_total)}（自動削除なし）')
if archives:
    try:
        latest = max(archives, key=lambda p: p.stat().st_mtime)
        updated = datetime.datetime.fromtimestamp(latest.stat().st_mtime).astimezone().isoformat(timespec='seconds')
        print(f'  最新バックアップ: {latest.name}\n    ファイル更新日時: {updated}')
    except OSError:
        errors.append('最新バックアップ'); print('  最新バックアップ: 取得できません')
else:
    print('  最新バックアップ: ' + ('取得できません' if archives is None else 'ありません'))
try:
    location = backups
    while not location.exists(): location = location.parent
    print('  バックアップ保存先の空き: ' + label(shutil.disk_usage(location).free))
except OSError:
    errors.append('空き容量')
if errors:
    print('  一部の情報を取得できません。読み取り権限やファイルの移動を確認してください。', file=sys.stderr)
PY
}


logs_command() {
    local target="server"
    local follow=0
    local lines=100

    while [[ "$#" -gt 0 ]]; do
        case "$1" in
            -f|--follow)
                follow=1
                shift
                ;;
            -n|--lines)
                if [[ -z "${2:-}" ]]; then
                    echo "使い方: $0 logs [target] [-n lines] [--follow]" >&2
                    exit 1
                fi
                lines="$2"
                shift 2
                ;;
            *)
                target="$1"
                shift
                ;;
        esac
    done

    validate_non_negative_integer "logs lines" "$lines"

    local unit
    unit="$(service_unit_for_target "$target")"
    if [[ "$follow" -eq 1 ]]; then
        journalctl -u "$unit" -f
    else
        journalctl -u "$unit" -n "$lines" --no-pager
    fi
}

timers_command() {
    systemctl list-timers \
        "$APP_NAME-update.timer" \
        "$APP_NAME-backup.timer"
}

remove_systemd_units() {
    local unit
    for unit in "$@"; do
        rm -f "$(systemd_unit_path "$unit")"
    done
}

chown_for_service() {
    if is_root && [[ -n "${SERVICE_USER:-}" && -n "${SERVICE_GROUP:-}" ]]; then
        chown -R "$SERVICE_USER:$SERVICE_GROUP" "$@"
    fi
}

curl_json() {
    local url="$1"

    curl \
        --fail \
        --silent \
        --show-error \
        --connect-timeout 10 --max-time 60 --retry 2 --retry-max-time 180 \
        --location \
        --user-agent "$CURL_USER_AGENT" \
        "$url"
}

curl_download() {
    local url="$1"
    local output="$2"

    curl \
        --fail \
        --connect-timeout 10 --max-time 600 --retry 2 --retry-max-time 900 \
        --location \
        --user-agent "$CURL_USER_AGENT" \
        --referer "https://www.minecraft.net/en-us/download/server/bedrock" \
        --output "$output" \
        "$url"
}

send_server_command() {
    local command="$1"
    if [[ ! -p "$STDIN_FIFO" ]]; then
        return 0
    fi

    if command -v timeout >/dev/null 2>&1; then
        timeout 5s bash -c 'printf "%s\n" "$1" > "$2"' _ "$command" "$STDIN_FIFO" || true
    else
        printf '%s\n' "$command" > "$STDIN_FIFO" || true
    fi
}

# 通常通知と入退室通知で共通の送信処理。全試行と待機に合計時間の上限を設ける。
send_discord_payload() {
    local payload="$1" work="$2" attempt http_code=000 curl_status=0 delay
    local deadline remaining request_timeout budget reason="送信失敗"
    case "$DISCORD_TIMEOUT_SECONDS" in
        ''|*[!0-9]*|0) echo "DISCORD_TIMEOUT_SECONDS は1以上の整数にしてください。" >&2; return 1 ;;
    esac
    budget="$((10#$DISCORD_TIMEOUT_SECONDS))"
    if [[ "$budget" -le 0 ]]; then
        echo "DISCORD_TIMEOUT_SECONDS は1以上の整数にしてください。" >&2
        return 1
    fi
    deadline="$((SECONDS + budget))"
    rm -f -- "$work/deferred-delay"
    for attempt in 1 2 3; do
        remaining="$((deadline - SECONDS))"
        if [[ "$remaining" -le 0 ]]; then reason="送信時間の上限"; break; fi
        request_timeout=20
        [[ "$remaining" -ge "$request_timeout" ]] || request_timeout="$remaining"
        curl_status=0
        http_code="$(curl --silent --show-error --connect-timeout 5 --max-time "$request_timeout" \
            --output "$work/response.json" --dump-header "$work/headers" --write-out '%{http_code}' \
            -H 'Content-Type: application/json' --data-binary "$payload" \
            "$DISCORD_WEBHOOK_URL" 2>"$work/curl-error")" || curl_status=$?
        if [[ "$curl_status" -eq 0 && "$http_code" == 2?? ]]; then return 0; fi
        delay="$((attempt * 2))"
        if [[ "$curl_status" -eq 0 && "$http_code" == 429 ]]; then
            delay="$(jq -er '.retry_after | select(type == "number" and . >= 0)' \
                "$work/response.json" 2>/dev/null)" || delay=""
            if [[ ! "$delay" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
                delay="$(awk 'tolower($1) == "retry-after:" {gsub("\r", "", $2); value=$2} END {print value}' \
                    "$work/headers")" || delay=""
            fi
            [[ "$delay" =~ ^[0-9]+([.][0-9]+)?$ ]] || delay=5
        elif [[ "$curl_status" -eq 0 && "$http_code" != 5?? && "$http_code" != 408 ]]; then
            reason="再試行しないHTTP応答"; break
        elif [[ "$attempt" -eq 3 ]]; then
            break
        fi
        remaining="$((deadline - SECONDS))"
        if ! awk -v delay="$delay" -v remaining="$remaining" 'BEGIN {exit !(delay < remaining)}'; then
            # 独立した入退室監視だけは、後続イベントの前にこの待機を引き継ぐ。
            if [[ "$curl_status" -eq 0 && "$http_code" == 429 ]]; then
                printf '%s\n' "$delay" > "$work/deferred-delay"
            fi
            reason="再試行待ちが送信時間の上限を超過"; break
        fi
        sleep "$delay" || return 1
        [[ "$attempt" -lt 3 ]] || break
    done
    # 応答本文・curl の詳細・Webhook URLはログへ出さない。
    echo "Discord通知に失敗しました（HTTP=${http_code}、curl=${curl_status}、試行=${attempt}、${reason}）。" >&2
    return 1
}

notify_discord() (
    local message="$1" payload work
    [[ -n "${DISCORD_WEBHOOK_URL:-}" ]] || exit 0
    payload="$(jq -n --arg content "$message" '{content: $content, allowed_mentions: {parse: []}}')" || {
        echo "Discord通知のJSON作成に失敗しました。" >&2; exit 0;
    }
    umask 077
    work="$(mktemp -d "${TMPDIR:-/tmp}/bds-discord.XXXXXXXX")" || {
        echo "Discord通知の一時領域を作成できません。" >&2; exit 0;
    }
    trap 'rm -rf -- "$work"' EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    # 通知失敗だけを理由にBDSの起動・停止・メンテナンスを失敗させない。
    send_discord_payload "$payload" "$work" || true
    exit 0
)

notify_command() {
    local message="${1:-}"
    if [[ -z "$message" ]]; then
        echo "使い方: $0 notify <message>" >&2
        exit 1
    fi

    notify_discord "$message"
    return 0
}

# 入退室通知は BDS の標準出力から独立させ、送信待ちをゲーム処理に伝播させない。
player_notification_records() {
    jq --unbuffered -r '
        .__CURSOR as $cursor |
        if ($cursor | type) != "string" or $cursor == "" or ($cursor | test("[\\t\\r\\n]")) then
            error("journal cursor is missing or invalid")
        else
            (if (.MESSAGE | type) == "string" then
                ((.MESSAGE | capture("^(?:NO LOG FILE! - )?\\[(?:[^\\]\\r\\n]* )?INFO\\] Player (?<action>connected|disconnected):\\s*(?<name>[^,\\r\\n]+),\\s*xuid:\\s*[0-9]+(?:,[^\\r\\n]*)?$")) // null)
             else null end) as $event |
            $cursor + "\t" +
            (if $event == null then "" else
                {content: (($event.name | gsub("^\\s+|\\s+$"; "")) +
                           (if $event.action == "connected" then " がログインしました。" else " がログアウトしました。" end)),
                 allowed_mentions: {parse: []}} | tojson
             end)
        end
    '
}

save_player_cursor() {
    # rename で確定するため、途中終了しても半端なカーソルを次回読み込まない。
    printf '%s\n' "$1" > "$player_work/cursor.next"
    mv -f -- "$player_work/cursor.next" "$PLAYER_NOTIFY_STATE_DIR/cursor"
}

send_player_notification() {
    send_discord_payload "$1" "$player_work"
}

watch_players() (
    if [[ "$PLAYER_NOTIFY_ENABLED" != true || -z "${DISCORD_WEBHOOK_URL:-}" ]]; then
        echo "入退室通知には PLAYER_NOTIFY_ENABLED=true と DISCORD_WEBHOOK_URL が必要です。" >&2
        exit 78
    fi
    need_cmds journalctl jq curl flock mktemp mkfifo mv awk
    umask 077
    mkdir -p "$PLAYER_NOTIFY_STATE_DIR"
    # バックアップ・更新用のロックとは別に、監視の二重起動だけを防ぐ。
    exec 8>"$PLAYER_NOTIFY_STATE_DIR/watch.lock"
    if ! flock -n 8; then
        echo "入退室通知の監視はすでに実行中です。" >&2
        exit 78
    fi
    player_work="$(mktemp -d "$PLAYER_NOTIFY_STATE_DIR/.run.XXXXXXXX")"
    player_journal_pid=""
    player_parser_pid=""
    cleanup_player_watcher() {
        local result=$?
        trap - EXIT INT TERM
        exec 7<&- || true
        [[ -z "$player_journal_pid" ]] || kill "$player_journal_pid" 2>/dev/null || true
        [[ -z "$player_parser_pid" ]] || kill "$player_parser_pid" 2>/dev/null || true
        [[ -z "$player_journal_pid" ]] || wait "$player_journal_pid" 2>/dev/null || true
        [[ -z "$player_parser_pid" ]] || wait "$player_parser_pid" 2>/dev/null || true
        rm -rf -- "$player_work"
        exit "$result"
    }
    trap cleanup_player_watcher EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM

    if [[ -f "$PLAYER_NOTIFY_STATE_DIR/cursor" ]]; then
        cursor="$(cat "$PLAYER_NOTIFY_STATE_DIR/cursor")"
        if [[ -z "$cursor" || "$cursor" == *$'\n'* || "$cursor" == *$'\r'* || "$cursor" == *$'\t'* ]]; then
            echo "保存した読み取り位置が不正です。入退室通知の cursor ファイルを確認してください。" >&2
            exit 78
        fi
    else
        # 初回は全 journal の現在の末尾を基点にする。BDS の履歴がなくても開始できる。
        # --after-cursor へ渡すので、基点取得と follow 開始の間のイベントも取りこぼさない。
        cursor="$(journalctl --no-pager --output=json --lines=1 |
            jq -ers 'last | .__CURSOR | select(type == "string" and length > 0)')" || {
            echo "journal の現在位置を取得できません。" >&2
            exit 1
        }
        save_player_cursor "$cursor"
    fi

    mkfifo "$player_work/raw" "$player_work/records"
    journalctl -u "$APP_NAME.service" --follow --no-tail --no-pager --output=json \
        --output-fields=MESSAGE --after-cursor="$cursor" > "$player_work/raw" &
    player_journal_pid=$!
    player_notification_records < "$player_work/raw" > "$player_work/records" &
    player_parser_pid=$!
    exec 7<"$player_work/records"
    echo "BDS の参加・退出通知の監視を開始しました。"
    while IFS=$'\t' read -r cursor payload <&7; do
        if [[ -n "$payload" ]]; then
            if ! send_player_notification "$payload"; then
                # 送信失敗で後続を永久に止めない。失敗した位置をログに残して先へ進む。
                echo "未送信の入退室通知をスキップします（cursor=${cursor}）。" >&2
            fi
        fi
        save_player_cursor "$cursor"
        if [[ -f "$player_work/deferred-delay" ]]; then
            # 通常通知の制限時間にBDSを巻き込まず、独立した監視で429の待機を継続する。
            sleep "$(cat "$player_work/deferred-delay")"
            rm -f -- "$player_work/deferred-delay"
        fi
    done
    echo "入退室ログの監視が終了しました。サービスを再起動してください。" >&2
    exit 1
)

warn_before_update() {
    local old_version="$1"
    local new_version="$2"
    local seconds="$NOTICE_SECONDS"

    case "$seconds" in
        ''|*[!0-9]*)
            echo "UPDATE_NOTICE_SECONDS は0以上の整数で指定してください: $seconds" >&2
            exit 1
            ;;
    esac

    if [[ "$seconds" -le 0 ]]; then
        return 0
    fi

    notify_discord "アップデートが見つかりました: ${old_version:-none} -> ${new_version}。${seconds}秒後に再起動します。"
    send_server_command "say アップデートが見つかりました。${seconds}秒後に再起動します。"

    if [[ "$seconds" -gt 120 ]]; then
        sleep "$((seconds - 60))"
        send_server_command "say アップデートを1分後に開始します。"
        sleep 50
        send_server_command "say アップデートを10秒後に開始します。"
        sleep 10
    elif [[ "$seconds" -gt 10 ]]; then
        sleep "$((seconds - 10))"
        send_server_command "say アップデートを10秒後に開始します。"
        sleep 10
    else
        sleep "$seconds"
    fi
}

download_url() {
    local api_url url
    for api_url in "${API_URLS[@]}"; do
        url="$(
            curl_json "$api_url" \
                | jq -r '.result.links[] | select(.downloadType == "serverBedrockLinux") | .downloadUrl'
        )" || {
            echo "ダウンロードリンク API の取得に失敗しました: $api_url" >&2
            continue
        }

        if [[ -n "$url" && "$url" != "null" ]]; then
            echo "ダウンロードリンク API を使用します: $api_url" >&2
            printf '%s\n' "$url"
            return 0
        fi

        echo "Bedrock Linux サーバーのダウンロード URL が見つかりません: $api_url" >&2
    done

    return 1
}

version_from_url() {
    local url="$1"
    basename "$url" | sed -E 's/^bedrock-server-//; s/\.zip$//'
}

current_version() {
    if [[ -f "$VERSION_FILE" ]]; then
        cat "$VERSION_FILE"
    fi
}

client_version() {
    local version="$1"

    printf '%s\n' "$version" | sed -E 's/^1\.//; s/(\.[0-9]+)$//'
}

render_server_name() {
    local version="$1"
    local short_version rendered
    short_version="$(client_version "$version")"
    rendered="$SERVER_NAME_FORMAT"

    if [[ -z "$rendered" ]]; then
        return 0
    fi

    rendered="${rendered//%V/$version}"
    rendered="${rendered//%v/$short_version}"
    printf '%s\n' "$rendered"
}

update_server_name_version() {
    local version="$1"
    local props="${2:-$DEST}/server.properties"
    local rendered tmp

    rendered="$(render_server_name "$version")"
    if [[ -z "$rendered" || ! -f "$props" ]]; then
        return 0
    fi

    tmp="$(mktemp)"
    awk -v rendered="$rendered" '
        BEGIN { updated = 0 }
        /^server-name=/ {
            print "server-name=" rendered
            updated = 1
            next
        }
        { print }
        END {
            if (!updated) {
                print "server-name=" rendered
            }
        }
    ' "$props" > "$tmp"
    mv "$tmp" "$props"

    echo "サーバー名を更新しました: $rendered"
}

start_server() {
    [[ -x "$DEST/bedrock_server" ]] || { echo "BDS実行ファイルがありません。" >&2; return 1; }
    export LD_LIBRARY_PATH="$DEST:${LD_LIBRARY_PATH:-}"
    exec python3 "$BASE_DIR/supervisor.py" --runtime "$DEST" \
        --state-dir "/run/$APP_NAME-health" --diagnostics-dir "/var/lib/$APP_NAME-health" \
        --start-timeout "$SERVER_START_TIMEOUT_SECONDS" --stable "$SERVER_STABLE_SECONDS" \
        --interval "$SERVER_HEALTH_INTERVAL_SECONDS" \
        --response-timeout "$SERVER_HEALTH_RESPONSE_TIMEOUT_SECONDS" --failures "$SERVER_HEALTH_FAILURES"
}

require_server_input_fifo() {
    if [[ ! -p "$STDIN_FIFO" ]]; then
        echo "サーバー入力 FIFO が見つかりません: $STDIN_FIFO" >&2
        echo "サーバーが起動中か確認してください: $0 status" >&2
        exit 1
    fi
}

server_command() {
    local command="$*"

    if [[ -z "$command" ]]; then
        echo "使い方: $0 command <Bedrockコマンド>" >&2
        echo "例: $0 command say メンテナンスを5分後に開始します。" >&2
        exit 1
    fi

    require_server_input_fifo
    send_server_command "$command"
}

stop_server() {
    require_server_input_fifo
    send_server_command "stop"
}

validate_non_negative_integer() {
    local name="$1"
    local value="$2"

    case "$value" in
        ''|*[!0-9]*)
            echo "$name は0以上の整数で指定してください: $value" >&2
            exit 1
            ;;
    esac
}

server_accepts_commands() {
    [[ -p "$STDIN_FIFO" ]]
}

backup_size_mb() {
    local path="$1"
    local size_kb
    size_kb="$(du -sk "$path" | awk '{print $1}')"
    echo "$(((size_kb + 1023) / 1024))"
}

free_space_mb() {
    local path="$1"
    local available_kb
    available_kb="$(df -Pk "$path" | awk 'NR == 2 {print $4}')"
    echo "$((available_kb / 1024))"
}

ensure_backup_space() {
    validate_non_negative_integer "BACKUP_MIN_FREE_MB" "$BACKUP_MIN_FREE_MB"

    local source_dir="$1"
    local target_dir="$2"
    local source_mb free_mb required_mb
    source_mb="$(backup_size_mb "$source_dir")"
    free_mb="$(free_space_mb "$target_dir")"
    required_mb="$((source_mb * 2 + BACKUP_MIN_FREE_MB))"

    if [[ "$free_mb" -lt "$required_mb" ]]; then
        local message="バックアップを中止しました: 空き容量は${free_mb}MB、必要容量は${required_mb}MBです。"
        echo "$message" >&2
        notify_discord "$message"
        if server_accepts_commands; then
            send_server_command "say 空き容量が不足しているため、バックアップを中止しました。"
        fi
        exit 1
    fi
}

# 一時領域内だけに展開し、パス逸脱や特殊ファイルを持つアーカイブは拒否する。
archive_tool() {
    python3 - "$@" <<'PY'
import gzip, json, os, posixpath, shutil, stat, sys, tarfile, zipfile
from pathlib import Path, PurePosixPath
mode, archive = sys.argv[1:3]
target = Path(sys.argv[3]) if len(sys.argv) > 3 else None

def clean(name):
    p = PurePosixPath(name)
    if p.is_absolute() or '..' in p.parts or '\\' in name or '\x00' in name:
        raise ValueError('unsafe archive path')
    return str(p)

def space(required):
    if target is not None:
        target.mkdir(parents=True, exist_ok=True)
        if shutil.disk_usage(target).free < required + 1024 * 1024 * 1024:
            raise ValueError('展開用の空き容量が不足しています（1GiBの余裕が必要です）')

try:
    if mode == 'release':
        with zipfile.ZipFile(archive) as z:
            entries = z.infolist()
            seen = set()
            for m in entries:
                name = clean(m.filename)
                if name in seen:
                    raise ValueError('duplicate release path')
                seen.add(name)
                kind = stat.S_IFMT(m.external_attr >> 16)
                if kind not in (0, stat.S_IFREG, stat.S_IFDIR):
                    raise ValueError('special release entry')
            if 'bedrock_server' not in seen or 'server.properties' not in seen:
                raise ValueError('BDSの実行ファイルまたは設定がZIPにありません')
            space(sum(m.file_size for m in entries))
            if z.testzip() is not None:
                raise ValueError('ZIP checksum mismatch')
            z.extractall(target)
            binary = target / 'bedrock_server'
            if not binary.is_file() or binary.stat().st_size == 0:
                raise ValueError('BDSの実行ファイルが空です')
            binary.chmod(0o755)
    else:
        # tar の終端より後も読み、gzip全体のCRCと切り詰めを検出する。
        with gzip.open(archive, 'rb') as compressed:
            while compressed.read(1024 * 1024):
                pass
        with tarfile.open(archive, 'r:gz') as t:
            members = {}
            for m in t.getmembers():
                name = clean(m.name)
                if name == '.':
                    continue
                if name in members:
                    raise ValueError('duplicate backup path')
                if not (m.isfile() or m.isdir() or m.issym() or m.islnk()):
                    raise ValueError('special backup entry')
                members[name] = m
            full = 'metadata.json' in members
            payload = 'server' if full else 'worlds'
            for name, m in members.items():
                if name != payload and not name.startswith(payload + '/') and not (full and name == 'metadata.json'):
                    raise ValueError('バックアップ対象外のパスが含まれています')
                parent = PurePosixPath(name).parent
                while str(parent) != '.':
                    ancestor = members.get(str(parent))
                    if ancestor is not None and not ancestor.isdir():
                        raise ValueError('non-directory archive ancestor')
                    parent = parent.parent
                if m.issym():
                    if m.linkname.startswith('/') or '\\' in m.linkname:
                        raise ValueError('absolute or invalid symbolic link')
                    link = posixpath.normpath(posixpath.join(posixpath.dirname(name), m.linkname))
                    if link != payload and not link.startswith(payload + '/'):
                        raise ValueError('symbolic link leaves backup')
                if m.islnk():
                    link = clean(m.linkname)
                    if link not in members or not members[link].isfile():
                        raise ValueError('hard link target is not a regular file')
            world = 'server/worlds' if full else 'worlds'
            if world not in members or not members[world].isdir():
                raise ValueError('worlds/ がありません')
            if full:
                meta_member = members['metadata.json']
                if not meta_member.isfile() or meta_member.size > 65536:
                    raise ValueError('invalid backup metadata')
                metadata = json.load(t.extractfile(meta_member))
                if not isinstance(metadata, dict) or metadata.get('format') != 'bds-full' or metadata.get('schema') != 1:
                    raise ValueError('unsupported backup format')
                executable = members.get('server/bedrock_server')
                if executable is None or not executable.isfile() or executable.size == 0 or not executable.mode & 0o111:
                    raise ValueError('BDS実行ファイルがありません')
            if mode == 'extract':
                space(sum(m.size for m in members.values() if m.isfile()))
                # リンクは通常ファイルの後に作成し、リンク経由の書き込みを防ぐ。
                for name, m in members.items():
                    out = target / name
                    if m.isdir():
                        out.mkdir(parents=True, exist_ok=True)
                    elif m.isfile():
                        out.parent.mkdir(parents=True, exist_ok=True)
                        with t.extractfile(m) as src, out.open('xb') as dst:
                            shutil.copyfileobj(src, dst)
                        out.chmod(m.mode & 0o777)
                        os.utime(out, (m.mtime, m.mtime))
                for name, m in members.items():
                    out = target / name
                    if m.issym() or m.islnk():
                        out.parent.mkdir(parents=True, exist_ok=True)
                        if m.issym():
                            out.symlink_to(m.linkname)
                        else:
                            os.link(target / clean(m.linkname), out)
                for name, m in sorted(members.items(), key=lambda x: len(PurePosixPath(x[0]).parts), reverse=True):
                    if m.isdir():
                        out = target / name
                        out.chmod(m.mode & 0o777)
                        os.utime(out, (m.mtime, m.mtime))
            print('full' if full else 'worlds')
except (OSError, ValueError, tarfile.TarError, zipfile.BadZipFile, EOFError) as e:
    print('アーカイブの検証・展開に失敗しました: ' + str(e), file=sys.stderr)
    sys.exit(1)
PY
}

archive_is_valid() {
    archive_tool inspect "$1" >/dev/null
}

write_snapshot_metadata() {
    python3 - "$1" <<'PY'
import datetime, json, sys
from pathlib import Path
root = Path(sys.argv[1])
p = root / 'server/.installed-version'
version = p.read_text().strip() if p.is_file() else None
(root / 'metadata.json').write_text(json.dumps({
    'format': 'bds-full', 'schema': 1,
    'created_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
    'bds_version': version,
}, ensure_ascii=False, indent=2) + '\n')
PY
}

# run / send-stop / notify は systemd からも呼ばれるため、ロックしません。
# ロックファイルは削除せず、同じ inode を全メンテナンス操作で共有します。
acquire_maintenance_lock() {
    refuse_during_reboot "$1"
    need_cmd flock
    exec 9>"$BASE_DIR/.maintenance.lock"
    if [[ "$1" == backup ]]; then
        validate_non_negative_integer "BACKUP_LOCK_WAIT_SECONDS" "$BACKUP_LOCK_WAIT_SECONDS"
        if ! flock -w "$BACKUP_LOCK_WAIT_SECONDS" 9; then
            local message="別のメンテナンスの完了を${BACKUP_LOCK_WAIT_SECONDS}秒待ちましたが、バックアップを開始できませんでした。"
            echo "$message" >&2
            need_cmds curl jq
            notify_discord "$message"
            exit 1
        fi
    elif ! flock -n 9; then
        echo "別のメンテナンスが実行中のため、${1}を実行しません。" >&2
        case "$1" in
            update|auto-update) exit 0 ;;
            *) exit 1 ;;
        esac
    fi
    # ロック待ちの間にOS再起動が要求された場合も、新しい処理を開始しない。
    refuse_during_reboot "$1"
}

refuse_during_reboot() {
    if [[ -e "$CONTROL_REBOOT_MARKER" ]]; then
        echo "OSの再起動処理中のため、${1}を実行しません。" >&2
        case "$1" in update|auto-update) exit 0 ;; *) exit 1 ;; esac
    fi
}

service_property() {
    systemctl show "$APP_NAME.service" --property="$1" --value
}

# systemd 管理外の run や停止処理中のワールドをコピーしないための確認。
require_stopped_server() {
    local state main_pid control_group remaining_pids
    state="$(service_property ActiveState)" || return 1
    main_pid="$(service_property MainPID)" || return 1
    if [[ ( "$state" != inactive && "$state" != failed ) || "$main_pid" != 0 || -p "$STDIN_FIFO" ]]; then
        echo "サーバーの完全な停止を確認できません: state=$state pid=$main_pid" >&2
        return 1
    fi
    control_group="$(service_property ControlGroup)" || return 1
    if [[ -n "$control_group" && -f "/sys/fs/cgroup$control_group/cgroup.procs" ]]; then
        remaining_pids="$(cat "/sys/fs/cgroup$control_group/cgroup.procs")" || return 1
        if [[ -n "$remaining_pids" ]]; then
            echo "サービスにプロセスが残っています。" >&2
            return 1
        fi
    fi
}

start_and_wait() {
    # Type=notifyのREADYは監督処理だけが送る。失敗時の再試行もsystemdだけが担当する。
    local state invocation deadline stable_until stable_seconds="${1:-0}"
    deadline="$((SECONDS + (SERVER_START_TIMEOUT_SECONDS + SERVER_STABLE_SECONDS + DISCORD_TIMEOUT_SECONDS + 15) * SERVER_RESTART_LIMIT + SERVER_RESTART_DELAY_SECONDS * (SERVER_RESTART_LIMIT - 1)))"
    if ! systemctl_run start "$APP_NAME.service"; then
        state="$(service_property ActiveState)" || return 1
        [[ "$state" == activating ]] || return 1
    fi
    while [[ "$SECONDS" -lt "$deadline" ]]; do
        state="$(service_property ActiveState)" || return 1
        case "$state" in
            failed|inactive|deactivating) return 1 ;;
        esac
        if systemctl is-active --quiet "$APP_NAME.service" && [[ -p "$STDIN_FIFO" ]]; then
            [[ "$(service_property Type)" == notify ]] || {
                echo "監督処理のservice設定が必要です。configureを実行してください。" >&2; return 1;
            }
            invocation="$(service_property InvocationID)" || return 1
            [[ -n "$invocation" ]] || return 1
            stable_until="$((SECONDS + stable_seconds))"
            while true; do
                [[ "$(service_property InvocationID)" == "$invocation" ]] || return 1
                systemctl is-active --quiet "$APP_NAME.service" || return 1
                [[ -p "$STDIN_FIFO" ]] || return 1
                [[ "$SECONDS" -lt "$stable_until" ]] || return 0
                sleep 1
            done
        fi
        sleep 1
    done
    echo "監督処理による起動・復旧の完了を確認できませんでした。" >&2
    return 1
}

# サブシェルで trap と一時状態を閉じ込め、途中失敗時も先にサーバーを復帰させる。
backup_server() (
    require_backup_deps
    need_cmds systemctl journalctl
    validate_non_negative_integer "BACKUP_RETENTION_DAYS" "$BACKUP_RETENTION_DAYS"
    validate_non_negative_integer "BACKUP_NOTICE_SECONDS" "$BACKUP_NOTICE_SECONDS"
    validate_non_negative_integer "SERVER_START_TIMEOUT_SECONDS" "$SERVER_START_TIMEOUT_SECONDS"
    if [[ "$SERVER_START_TIMEOUT_SECONDS" -eq 0 ]]; then
        echo "SERVER_START_TIMEOUT_SECONDS は1以上にしてください。" >&2
        exit 1
    fi

    backup_staging_dir=""
    backup_partial=""
    backup_restart_needed=0
    backup_start_failed=0
    backup_archive_complete=0
    backup_error="事前確認"
    backup_cleanup() {
        local result=$?
        trap - EXIT INT TERM
        if [[ "$backup_restart_needed" -eq 1 ]]; then
            if start_and_wait; then
                echo "バックアップ中断後にサーバーを復帰しました。"
            else
                backup_start_failed=1
                result=1
            fi
        fi
        [[ -z "$backup_partial" ]] || rm -f -- "$backup_partial" || result=1
        [[ -z "$backup_staging_dir" ]] || rm -rf -- "$backup_staging_dir" || result=1
        if [[ "$backup_start_failed" -eq 1 ]]; then
            notify_discord "バックアップ後のサーバー再起動に失敗しました。サーバー状態とログを確認してください。" || true
            echo "サーバー再起動に失敗しました。" >&2
        fi
        if [[ "$result" -ne 0 ]]; then
            if [[ "$backup_archive_complete" -eq 1 ]]; then
                notify_discord "バックアップは保存済みですが、${backup_error}に失敗しました。" || true
            else
                notify_discord "バックアップに失敗しました（${backup_error}）。" || true
            fi
            echo "バックアップ処理に失敗しました（${backup_error}）。" >&2
        fi
        exit "$result"
    }
    trap backup_cleanup EXIT
    trap 'backup_error="割り込み"; exit 130' INT
    trap 'backup_error="中断"; exit 143' TERM

    validate_runtime_path
    worlds_dir="$DEST/worlds"
    if [[ ! -d "$worlds_dir" ]]; then
        backup_error="ワールドディレクトリが存在しません: $worlds_dir"
        exit 1
    fi
    mkdir -p "$BACKUP_DIR"
    runtime_dir="$(realpath "$DEST")"
    backup_target="$(realpath "$BACKUP_DIR")"
    case "$backup_target/" in
        "$runtime_dir/"*)
            backup_error="バックアップ先を BEDROCK_DIR の外に指定してください"
            exit 1 ;;
    esac
    ensure_backup_space "$DEST" "$BACKUP_DIR"

    state="$(service_property ActiveState)"
    case "$state" in
        active)
            # 旧 unit は停止タイムアウト後に BDS を強制終了するため受け付けない。
            if [[ "$(service_property Type)" != notify || "$(service_property KillMode)" != mixed || "$(service_property SendSIGKILL)" != no ]]; then
                backup_error="サービス設定が古いため、sudo $SCRIPT_PATH configure を実行してください"
                exit 1
            fi
            ;;
        inactive) require_stopped_server ;;
        *) backup_error="サービスが安定した起動・停止状態ではありません: $state"; exit 1 ;;
    esac

    umask 077
    backup_staging_dir="$(mktemp -d "$BACKUP_DIR/.bds-stage.XXXXXXXX")"
    archive="$BACKUP_DIR/bds-server-$(date +%Y%m%d-%H%M%S)-$$.tar.gz"
    backup_partial="$archive.partial"
    echo "バックアップを作成します: $archive"
    notify_discord "バックアップを開始します。"

    if [[ "$state" == active ]]; then
        backup_error="停止予告"
        send_server_command "say バックアップのため${BACKUP_NOTICE_SECONDS}秒後にサーバーを停止します。コピー完了後に再起動します。"
        sleep "$BACKUP_NOTICE_SECONDS"
        backup_error="サーバーの正常停止"
        backup_restart_needed=1
        stop_for_maintenance
    fi

    backup_error="停止後の空き容量確認"
    ensure_backup_space "$DEST" "$BACKUP_DIR"
    backup_error="サーバー一式のコピー"
    cp -a "$DEST" "$backup_staging_dir/server"
    validate_runtime_links "$backup_staging_dir/server"
    write_snapshot_metadata "$backup_staging_dir"
    require_stopped_server

    if [[ "$backup_restart_needed" -eq 1 ]]; then
        # 起動失敗でも停止中に得たコピーを圧縮・保存する。再試行ループにはしない。
        if start_and_wait; then
            backup_restart_needed=0
        else
            backup_restart_needed=0
            backup_start_failed=1
            echo "サーバー再起動に失敗しました。取得済みコピーの保存を続行します。" >&2
            notify_discord "サーバー再起動に失敗しました。取得済みのバックアップデータは保存を続けます。" || true
        fi
    fi

    backup_error="圧縮"
    tar -czf "$backup_partial" -C "$backup_staging_dir" server metadata.json
    backup_error="アーカイブ検証"
    archive_is_valid "$backup_partial"
    backup_error="バックアップファイルの保存"
    chown_for_service "$backup_partial"
    mv -- "$backup_partial" "$archive"
    backup_partial=""
    backup_archive_complete=1
    backup_error="古いバックアップの削除"
    find "$BACKUP_DIR" -maxdepth 1 -type f \( -name 'bds-worlds-*.tar.gz' -o -name 'bds-server-*.tar.gz' \) \
        ! -name "$(basename "$archive")" -mtime +"$BACKUP_RETENTION_DAYS" -delete
    if [[ "$backup_start_failed" -eq 1 ]]; then
        backup_error="サーバー再起動"
        exit 1
    fi
    notify_discord "バックアップが完了しました: $(basename "$archive")"
    echo "バックアップが完了しました: $archive"
)

validate_runtime_path() {
    python3 - "$DEST" "$BASE_DIR" <<'PY'
import os, sys
from pathlib import Path
runtime, manager = map(Path, sys.argv[1:])
resolved = runtime.resolve()
if (not runtime.is_absolute() or runtime.is_symlink() or os.path.normpath(sys.argv[1]) != sys.argv[1]
        or (runtime.exists() and not runtime.is_dir())
        or resolved == manager.resolve() or resolved in manager.resolve().parents):
    sys.exit('BEDROCK_DIR は管理用ディレクトリを含まない、正規化された絶対パスの実ディレクトリにしてください。')
PY
}

validate_runtime_links() {
    python3 - "$1" <<'PY'
import os, sys
from pathlib import Path
root = Path(sys.argv[1]).resolve()
for parent, directories, files in os.walk(root, followlinks=False):
    for name in directories + files:
        path = Path(parent) / name
        if path.is_symlink():
            try:
                if os.path.isabs(os.readlink(path)):
                    raise ValueError('absolute link')
                path.resolve().relative_to(root)
            except (ValueError, RuntimeError):
                sys.exit('コピー先の外へ参照するリンクは扱えません: ' + str(path))
PY
}

maintenance_state() {
    local load state
    load="$(service_property LoadState 2>/dev/null)" || {
        [[ "$load" == not-found ]] || return 1
    }
    if [[ "$load" == not-found ]]; then
        [[ ! -p "$STDIN_FIFO" ]] || return 1
        printf 'inactive\n'
        return 0
    fi
    state="$(service_property ActiveState)" || return 1
    case "$state" in
        active)
            if [[ "$(service_property Type)" != notify || "$(service_property KillMode)" != mixed || "$(service_property SendSIGKILL)" != no ]]; then
                echo "先に sudo $SCRIPT_PATH configure でサービス設定を反映してください。" >&2
                return 1
            fi
            ;;
        inactive) require_stopped_server || return 1 ;;
        *) echo "サービスが安定した状態ではありません: $state" >&2; return 1 ;;
    esac
    printf '%s\n' "$state"
}

stop_for_maintenance() {
    local invocation
    invocation="$(service_property InvocationID)" || return 1
    [[ -n "$invocation" ]] || return 1
    systemctl_run stop "$APP_NAME.service" || return 1
    require_stopped_server || return 1
    # systemd may report a clean SIGTERM (15) for the exiting supervisor.
    # Require the actual BDS child's successful shutdown receipt from this invocation.
    python3 - "/run/$APP_NAME-health/status.json" "$invocation" <<'PYSTOP'
import json,sys
try:
    data=json.load(open(sys.argv[1]))
    valid=(data['invocation_id']==sys.argv[2] and data['stopped'] is True
           and data['child_exit_status']==0 and data['failed'] is False and data['ready'] is False)
except (OSError,ValueError,KeyError,TypeError):
    valid=False
if not valid: sys.exit('今回のBDSの正常終了記録を確認できません。')
PYSTOP
}


ensure_copy_space() {
    local required available
    required="$(( $(backup_size_mb "$1") + BACKUP_MIN_FREE_MB ))"
    available="$(free_space_mb "$2")"
    if [[ "$available" -lt "$required" ]]; then
        echo "コピー用容量不足: 必要${required}MB、空き${available}MB" >&2
        return 1
    fi
}

prepare_release() {
    local url="$1" directory="$2"
    echo "BDSをダウンロード・検証します。"
    curl_download "$url" "$directory/release.zip"
    archive_tool release "$directory/release.zip" "$directory/release"
    rm -f -- "$directory/release.zip"
}

overlay_release() {
    local release="$1" candidate="$2" entry name
    for entry in "$release"/.[!.]* "$release"/..?* "$release"/*; do
        [[ -e "$entry" || -L "$entry" ]] || continue
        name="$(basename "$entry")"
        case "$name" in
            worlds|server.properties|allowlist.json|permissions.json|whitelist.json|development_behavior_packs|development_resource_packs|development_skin_packs|config|keys)
                [[ ! -e "$candidate/$name" && ! -L "$candidate/$name" ]] || continue ;;
            behavior_packs|resource_packs|world_templates)
                # 同梱データは新版に更新し、同梱されていない独自パックも残す。
                if [[ -d "$entry" && -d "$candidate/$name" && ! -L "$candidate/$name" ]]; then
                    merge_release_directory "$entry" "$candidate/$name"
                    continue
                fi
                ;;
        esac
        rm -rf -- "$candidate/$name"
        cp -a "$entry" "$candidate/$name"
    done
}

merge_release_directory() {
    local source="$1" target="$2" entry name
    for entry in "$source"/.[!.]* "$source"/..?* "$source"/*; do
        [[ -e "$entry" || -L "$entry" ]] || continue
        name="$(basename "$entry")"
        if [[ -d "$entry" && -d "$target/$name" && ! -L "$target/$name" ]]; then
            merge_release_directory "$entry" "$target/$name"
        else
            # 既存リンクをたどらず、リンク自体を置換する。
            rm -rf -- "$target/$name"
            cp -a "$entry" "$target/$name"
        fi
    done
}

# 更新・復元の切り替えは同じ復旧処理を共有する。
# 旧ディレクトリを変更せず退避し、新版が書き換えたワールドも一緒に巻き戻す。
deploy_server() (
    operation="$1"
    archive="${2:-}"
    require_deps
    require_backup_deps
    need_cmds systemctl journalctl
    validate_runtime_path
    validate_non_negative_integer "BACKUP_MIN_FREE_MB" "$BACKUP_MIN_FREE_MB"
    validate_non_negative_integer "SERVER_STABLE_SECONDS" "$SERVER_STABLE_SECONDS"
    validate_non_negative_integer "SERVER_START_TIMEOUT_SECONDS" "$SERVER_START_TIMEOUT_SECONDS"
    if [[ "$SERVER_START_TIMEOUT_SECONDS" -eq 0 ]]; then
        echo "SERVER_START_TIMEOUT_SECONDS は1以上にしてください。" >&2
        exit 1
    fi
    mkdir -p "$(dirname "$DEST")"
    umask 077
    transaction="$(mktemp -d "$(dirname "$DEST")/.bds-transaction.XXXXXXXX")"
    candidate="$transaction/candidate"
    rollback="$DEST.pre-${operation}-$(date +%Y%m%d-%H%M%S)-$$"
    failed="$DEST.failed-${operation}-$(date +%Y%m%d-%H%M%S)-$$"
    restart_needed=0
    committed=0
    preserve_transaction=0
    phase="事前準備"
    rollback_deployment() {
        # candidateが動き始めた可能性がある。完全停止までデータを交換しない。
        if [[ "$(service_property LoadState 2>/dev/null || true)" == not-found ]]; then
            [[ ! -p "$STDIN_FIFO" ]] || return 1
        else
            if ! systemctl_run stop "$APP_NAME.service" || ! require_stopped_server; then
                echo "復旧のための停止を確認できません。退避先を保持します: $rollback" >&2
                return 1
            fi
        fi
        if [[ -e "$DEST" ]]; then
            mv -- "$DEST" "$failed" || return 1
            echo "失敗した切り替え先を保持しました: $failed" >&2
        fi
        mv -- "$rollback" "$DEST" || return 1
        # 完全停止後に旧構成へ戻した。新版の失敗回数を旧構成へ引き継がない。
        systemctl_run reset-failed "$APP_NAME.service" || return 1
        echo "元の実行ファイル・設定・ワールドへ戻しました。" >&2
    }
    cleanup_deployment() {
        local result=$? recovered=1
        trap - EXIT INT TERM
        if [[ "$committed" -eq 0 && -d "$rollback" ]]; then
            if ! rollback_deployment; then
                preserve_transaction=1
                recovered=0
                result=1
                notify_discord "${operation}の復旧に失敗しました。サーバー状態と退避先を確認してください: $rollback" || true
            fi
        fi
        if [[ "$committed" -eq 0 && "$restart_needed" -eq 1 && "$recovered" -eq 1 ]]; then
            if ! start_and_wait; then
                result=1
                notify_discord "${operation}中断後のサーバー起動に失敗しました。ログを確認してください。" || true
            fi
        fi
        if [[ "$preserve_transaction" -eq 0 ]]; then
            rm -rf -- "$transaction" || result=1
        else
            echo "作業データを保持しました: $transaction" >&2
        fi
        if [[ "$result" -ne 0 ]]; then
            echo "${operation}に失敗しました（${phase}）。" >&2
            notify_discord "${operation}に失敗しました（${phase}）。" || true
        fi
        exit "$result"
    }
    trap cleanup_deployment EXIT
    trap 'phase="割り込み"; exit 130' INT
    trap 'phase="中断"; exit 143' TERM

    if [[ "$operation" == restore ]]; then
        [[ -n "$archive" && -f "$archive" ]] || { echo "使い方: $0 restore <archive>" >&2; exit 1; }
        phase="復元データの展開・検証"
        restore_format="$(archive_tool extract "$archive" "$transaction/unpacked")"
        if [[ "$restore_format" == full ]]; then
            validate_runtime_links "$transaction/unpacked/server"
            mv -- "$transaction/unpacked/server" "$candidate"
        else
            validate_runtime_links "$transaction/unpacked/worlds"
        fi
    else
        [[ "$operation" == update || "$operation" == install ]] || exit 1
        if [[ "$operation" == install && -e "$DEST/bedrock_server" ]]; then
            echo "BDSはインストール済みです。更新には update を使ってください。" >&2
            exit 1
        fi
        if [[ "$operation" == update && ! -x "$DEST/bedrock_server" ]]; then
            echo "BDSがありません。先に install を実行してください。" >&2
            exit 1
        fi
        phase="更新情報の取得"
        url="$(download_url)"
        [[ -n "$url" && "$url" != null ]]
        version="$(version_from_url "$url")"
        installed="$(current_version || true)"
        if [[ "$operation" == update && "$installed" == "$version" ]]; then
            committed=1
            echo "更新はありません: $version"
            exit 0
        fi
        phase="ダウンロード・事前展開"
        prepare_release "$url" "$transaction"
    fi

    state="$(maintenance_state)"
    if [[ "$operation" == install && "$state" != inactive ]]; then
        echo "インストールはサーバー停止中に実行してください。" >&2
        exit 1
    fi
    if [[ -d "$DEST" && ! -d "$candidate" ]]; then
        ensure_copy_space "$DEST" "$transaction"
    fi
    if [[ "$state" == active ]]; then
        phase="停止予告"
        if [[ "$operation" == update ]]; then
            warn_before_update "$installed" "$version"
        else
            notify_discord "復元のため、サーバーを停止します。"
        fi
        phase="サーバーの正常停止"
        restart_needed=1
        stop_for_maintenance
    fi
    phase="停止中データの準備"
    if [[ ! -d "$candidate" ]]; then
        if [[ -d "$DEST" ]]; then
            ensure_copy_space "$DEST" "$transaction"
            cp -a "$DEST" "$candidate"
        else
            mkdir -p "$candidate"
        fi
    fi
    validate_runtime_links "$candidate"
    if [[ "$operation" == restore ]]; then
        if [[ "$restore_format" == worlds ]]; then
            [[ -x "$candidate/bedrock_server" ]] || { echo "worldsのみの復元にはBDS本体が必要です。" >&2; exit 1; }
            rm -rf -- "$candidate/worlds"
            mv -- "$transaction/unpacked/worlds" "$candidate/worlds"
        fi
    else
        overlay_release "$transaction/release" "$candidate"
        printf '%s\n' "$version" > "$candidate/.installed-version"
        printf '%s\n' "$url" > "$candidate/.installed-url"
        update_server_name_version "$version" "$candidate"
    fi
    # worldsの差し替え・更新ファイルの反映後の最終構成も検証する。
    validate_runtime_links "$candidate"
    [[ -s "$candidate/bedrock_server" && -x "$candidate/bedrock_server" ]]
    chown_runtime "$candidate"
    phase="ディレクトリの切り替え"
    if [[ -e "$DEST" ]]; then
        mv -- "$DEST" "$rollback"
    fi
    mv -- "$candidate" "$DEST"
    if [[ "$restart_needed" -eq 1 ]]; then
        phase="起動・安定稼働の確認"
        start_and_wait "$SERVER_STABLE_SECONDS"
    fi
    committed=1
    echo "${operation}が完了しました。"
    [[ ! -d "$rollback" ]] || echo "切り替え前のデータを保持しています: $rollback"
    notify_discord "${operation}が完了しました。"
)

chown_runtime() {
    local user group load
    if is_root; then
        load="$(service_property LoadState 2>/dev/null || true)"
        if [[ "$load" == not-found || -z "$load" ]]; then
            user="${SERVICE_USER:-root}"
            group="${SERVICE_GROUP:-}"
        else
            user="$(service_property User)"
            group="$(service_property Group)"
        fi
        user="${user:-root}"
        group="${group:-$(id -gn "$user")}"
        chown -R "$user:$group" "$1"
    fi
}

install_server() { deploy_server install; }
auto_update() { deploy_server update; }
restore_server() { deploy_server restore "${1:-}"; }


# root所有の固定systemd unitから呼ぶ。外部から受け取った文字列はコマンドにしない。
send_control_notice() {
    [[ -p "$STDIN_FIFO" ]] || { echo "停止予告用FIFOがありません。" >&2; return 1; }
    timeout 5s bash -c 'printf "%s\n" "$1" > "$2"' _ "say $1" "$STDIN_FIFO"
}

control_server() (
    local operation="$1" state seconds active_before=0 reboot_accepted=0 start_attempted=0 timer
    local -a active_timers=()
    is_root || { echo "root 権限が必要です。" >&2; exit 1; }
    need_cmds systemctl journalctl grep timeout curl jq
    validate_non_negative_integer "CONTROL_NOTICE_SECONDS" "$CONTROL_NOTICE_SECONDS"
    validate_non_negative_integer "SERVER_START_TIMEOUT_SECONDS" "$SERVER_START_TIMEOUT_SECONDS"
    validate_non_negative_integer "SERVER_STABLE_SECONDS" "$SERVER_STABLE_SECONDS"
    [[ "$SERVER_START_TIMEOUT_SECONDS" -gt 0 ]] || exit 1
    seconds="$((10#$CONTROL_NOTICE_SECONDS))"
    state="$(service_property ActiveState)"
    if [[ "$state" == failed ]]; then
        require_stopped_server
    else
        state="$(maintenance_state)"
    fi
    [[ "$state" != active ]] || active_before=1

    control_cleanup() {
        local result=$?
        trap - EXIT INT TERM
        if [[ "$operation" == reboot && "$reboot_accepted" -eq 0 ]]; then
            rm -f -- "$CONTROL_REBOOT_MARKER"
        fi
        # 停止後の中断・異常終了では、完全停止を確認できる場合だけ一度復旧する。
        # 通常の起動を既に試した場合は繰り返さず、OS再起動受理後にも起動しない。
        if [[ "$result" -ne 0 && "$reboot_accepted" -eq 0 && "$active_before" -eq 1 && "$start_attempted" -eq 0 ]] &&
            require_stopped_server >/dev/null 2>&1; then
            start_attempted=1
            start_and_wait "$SERVER_STABLE_SECONDS" || result=1
        fi
        if [[ "$operation" == reboot && "$reboot_accepted" -eq 0 ]]; then
            for timer in "${active_timers[@]}"; do
                systemctl_run start "$timer" || result=1
            done
        fi
        if [[ "$result" -ne 0 ]]; then
            notify_discord "要求された${operation}に失敗しました。サーバー状態とログを確認してください。" || true
        fi
        exit "$result"
    }
    trap control_cleanup EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM

    if [[ "$operation" == reboot ]]; then
        # ロックの解放後からOS停止まで、待機中のbackup等も実行させない。
        (umask 077; date -u +%Y-%m-%dT%H:%M:%SZ > "$CONTROL_REBOOT_MARKER")
        for timer in "$APP_NAME-update.timer" "$APP_NAME-backup.timer"; do
            if systemctl is-active --quiet "$timer"; then active_timers+=("$timer"); fi
        done
        systemctl_run stop "$APP_NAME-update.timer" "$APP_NAME-backup.timer"
    fi
    local target="Minecraftサーバー"
    [[ "$operation" != reboot ]] || target="VPSのOS"
    notify_discord "${target}を${seconds}秒後に再起動します。"
    if [[ "$active_before" -eq 1 ]]; then
        send_control_notice "${target}を${seconds}秒後に再起動します。"
    fi
    if [[ "$seconds" -gt 10 ]]; then
        sleep "$((seconds - 10))"
        if [[ "$active_before" -eq 1 ]]; then
            send_control_notice "${target}を10秒後に再起動します。"
        fi
        sleep 10
    else
        sleep "$seconds"
    fi
    if [[ "$active_before" -eq 1 ]]; then
        stop_for_maintenance
    else
        require_stopped_server
    fi
    if [[ "$operation" == restart ]]; then
        start_attempted=1
        start_and_wait "$SERVER_STABLE_SECONDS"
        notify_discord "Minecraftサーバーの再起動が完了しました。"
    else
        # --force は使用しない。要求が拒否された場合はcleanupで稼働状態を戻す。
        # shutdownでこのunitが停止されても、cleanupがBDSを起動し直さないよう先に設定。
        reboot_accepted=1
        if ! systemctl_run --no-block reboot; then
            reboot_accepted=0
            exit 1
        fi
    fi
)

configure_systemd() {
    if ! is_root; then
        echo "root 権限で実行してください: sudo $0 configure" >&2
        exit 1
    fi

    case "$PLAYER_NOTIFY_ENABLED" in
        true|false) ;;
        *) echo "PLAYER_NOTIFY_ENABLED は true または false にしてください。" >&2; exit 1 ;;
    esac
    if [[ "$PLAYER_NOTIFY_ENABLED" == true && -z "${DISCORD_WEBHOOK_URL:-}" ]]; then
        echo "入退室通知を有効にするには DISCORD_WEBHOOK_URL が必要です。" >&2
        exit 1
    fi

    local setting
    for setting in DISCORD_TIMEOUT_SECONDS SERVER_START_TIMEOUT_SECONDS SERVER_STABLE_SECONDS SERVER_HEALTH_INTERVAL_SECONDS SERVER_HEALTH_RESPONSE_TIMEOUT_SECONDS SERVER_HEALTH_FAILURES SERVER_RESTART_DELAY_SECONDS SERVER_RESTART_LIMIT; do
        validate_non_negative_integer "$setting" "${!setting}"
    done
    [[ "$SERVER_START_TIMEOUT_SECONDS" -gt 0 && "$SERVER_HEALTH_INTERVAL_SECONDS" -gt 0 && "$SERVER_HEALTH_RESPONSE_TIMEOUT_SECONDS" -gt 0 && "$SERVER_HEALTH_FAILURES" -gt 0 && "$SERVER_RESTART_DELAY_SECONDS" -gt 0 && "$SERVER_RESTART_LIMIT" -gt 0 && "$SERVER_RESTART_LIMIT" -le 3 ]] || {
        echo "監督設定は正数（安定確認のみ0可）、起動回数上限は1〜3にしてください。" >&2; return 1;
    }
    [[ -f "$BASE_DIR/supervisor.py" ]] || { echo "supervisor.pyがありません。" >&2; return 1; }
    local run_user run_group interval backup_on_calendar
    interval="${CHECK_INTERVAL:-6h}"
    backup_on_calendar="${BACKUP_ON_CALENDAR:-*-*-* 04:30:00}"
    # 実行ユーザーは既存の unit を優先する。初回は install と同じ設定を使う。
    local existing_user existing_group load_state
    load_state="$(service_property LoadState 2>/dev/null || true)"
    if [[ "$load_state" == not-found || -z "$load_state" ]]; then
        run_user="${SERVICE_USER:-root}"
        run_group="${SERVICE_GROUP:-$(id -gn "$run_user")}"
    else
        existing_user="$(service_property User)"
        existing_group="$(service_property Group)"
        run_user="${existing_user:-root}"
        run_group="${existing_group:-$(id -gn "$run_user")}"
    fi

    cat >"$(systemd_unit_path "$APP_NAME.service")" <<EOF
[Unit]
Description=Minecraft Bedrock Dedicated Server
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=600
StartLimitBurst=$SERVER_RESTART_LIMIT
OnFailure=$APP_NAME-failure.service

[Service]
Type=notify
NotifyAccess=main
RuntimeDirectory=$APP_NAME-health
RuntimeDirectoryPreserve=yes
StateDirectory=$APP_NAME-health
StateDirectoryMode=0700
User=$run_user
Group=$run_group
WorkingDirectory=$DEST
Environment=BDS_CONFIG=$CONFIG_FILE
Environment=LD_LIBRARY_PATH=$DEST
ExecStart=$SCRIPT_PATH run
ExecStartPost=-$SCRIPT_PATH notify-ready
ExecStop=$SCRIPT_PATH send-stop
ExecStopPost=-$SCRIPT_PATH notify "サーバーが停止しました。"
Restart=on-failure
RestartSec=$SERVER_RESTART_DELAY_SECONDS
TimeoutStartSec=$((SERVER_START_TIMEOUT_SECONDS + SERVER_STABLE_SECONDS + DISCORD_TIMEOUT_SECONDS + 15))
KillMode=mixed
SendSIGKILL=no
TimeoutStopSec=60

[Install]
WantedBy=multi-user.target
EOF

    cat >"$(systemd_unit_path "$APP_NAME-failure.service")" <<EOF
[Unit]
Description=Minecraft service failure notification
[Service]
Type=oneshot
Environment=BDS_CONFIG=$CONFIG_FILE
ExecStart=$SCRIPT_PATH notify-failure
EOF

    cat >"$(systemd_unit_path "$APP_NAME-update.service")" <<EOF
[Unit]
Description=Minecraft Bedrock Dedicated Server 更新
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
Environment=BDS_CONFIG=$CONFIG_FILE
Environment=SERVICE_USER=$run_user
Environment=SERVICE_GROUP=$run_group
Nice=10
IOSchedulingClass=idle
TimeoutStartSec=30min
ExecStart=$SCRIPT_PATH update
EOF

    cat >"$(systemd_unit_path "$APP_NAME-update.timer")" <<EOF
[Unit]
Description=Minecraft Bedrock Dedicated Server 定期更新確認

[Timer]
OnBootSec=10min
OnUnitActiveSec=$interval
Persistent=true

[Install]
WantedBy=timers.target
EOF

    cat >"$(systemd_unit_path "$APP_NAME-backup.service")" <<EOF
[Unit]
Description=Minecraft Bedrock Dedicated Server バックアップ

[Service]
Type=oneshot
Environment=BDS_CONFIG=$CONFIG_FILE
Environment=SERVICE_USER=$run_user
Environment=SERVICE_GROUP=$run_group
Nice=10
IOSchedulingClass=idle
TimeoutStartSec=2h
ExecStart=$SCRIPT_PATH backup
EOF

    cat >"$(systemd_unit_path "$APP_NAME-backup.timer")" <<EOF
[Unit]
Description=Minecraft Bedrock Dedicated Server 日次バックアップ

[Timer]
OnCalendar=$backup_on_calendar
Persistent=true

[Install]
WantedBy=timers.target
EOF

    cat >"$(systemd_unit_path "$APP_NAME-player-notify.service")" <<EOF
[Unit]
Description=Minecraft Bedrock Dedicated Server 入退室通知
After=systemd-journald.service network-online.target
Wants=network-online.target
StartLimitIntervalSec=0

[Service]
Type=simple
Environment=BDS_CONFIG=$CONFIG_FILE
ExecStart=$SCRIPT_PATH watch-players
Restart=always
RestartSec=5
RestartPreventExitStatus=78
KillMode=control-group
TimeoutStopSec=30
UMask=0077
NoNewPrivileges=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
EOF

    systemctl daemon-reload
    if [[ "$PLAYER_NOTIFY_ENABLED" == true ]]; then
        systemctl_enable "$APP_NAME-player-notify.service"
        # 設定とスクリプトを反映し、保存済みの読み取り位置から再開する。
        systemctl restart "$APP_NAME-player-notify.service"
    else
        systemctl_disable_now "$APP_NAME-player-notify.service"
    fi
    systemctl_enable "$APP_NAME.service"
    systemctl_enable_now \
        "$APP_NAME-update.timer" \
        "$APP_NAME-backup.timer"

    echo "systemd 設定を更新しました:"
    echo "  $SCRIPT_PATH status"
    echo "  $SCRIPT_PATH status update-timer"
    echo "  $SCRIPT_PATH status backup-timer"
    echo "  $SCRIPT_PATH status player-notify"
}

uninstall_systemd() {
    if ! is_root; then
        echo "root 権限で実行してください: sudo $0 uninstall" >&2
        exit 1
    fi

    systemctl_disable_now \
        "$APP_NAME-player-notify.service" \
        "$APP_NAME-backup.timer" \
        "$APP_NAME-update.timer" \
        "$APP_NAME.service"
    # 停止要求が成功しても、BDSが残っていればunitを削除しない。
    if [[ "$(service_property LoadState)" == not-found ]]; then
        [[ ! -p "$STDIN_FIFO" ]] || { echo "BDSの停止を確認できません。" >&2; return 1; }
    else
        require_stopped_server
    fi
    remove_systemd_units \
        "$APP_NAME-player-notify.service" \
        "$APP_NAME.service" \
        "$APP_NAME-failure.service" \
        "$APP_NAME-update.service" \
        "$APP_NAME-update.timer" \
        "$APP_NAME-backup.service" \
        "$APP_NAME-backup.timer"
    systemctl daemon-reload
    echo "systemd unit を削除しました。"
}

cmd="${1:-}"
case "$cmd" in
    install|start|stop|restart|update|auto-update|backup|restore|configure|uninstall|control-restart|control-reboot)
        acquire_maintenance_lock "$cmd"
        ;;
esac
case "$cmd" in
    control-restart)
        control_server restart
        ;;
    control-reboot)
        control_server reboot
        ;;
    watch-players)
        watch_players
        ;;
    notify-ready)
        recovered="$(python3 - "/run/$APP_NAME-health/status.json" <<'PYREADY'
import json, sys
try: print('true' if json.load(open(sys.argv[1])).get('recovered') else 'false')
except (OSError, ValueError): print('false')
PYREADY
)"
        if [[ "$recovered" == true ]]; then
            notify_discord "Minecraftサーバーが自動復旧しました。接続受付・コンソール応答を確認しました。"
        else
            notify_discord "サーバーが起動しました。接続受付・コンソール応答を確認しました。"
        fi
        ;;
    notify-failure)
        result="$(service_property Result)"
        restarts="$(service_property NRestarts)"
        state="$(service_property ActiveState)"
        if [[ "$result" == start-limit-hit || ( "$state" == failed && "$restarts" =~ ^[0-9]+$ && "$restarts" -ge "$((SERVER_RESTART_LIMIT - 1))" ) ]]; then
            notify_discord "Minecraftサーバーの自動復旧が回数上限に達しました。診断記録を確認してください。"
        else
            notify_discord "Minecraftサーバーで異常を検出しました。正常停止後、回数制限内で自動復旧します。診断記録を確認してください。"
        fi
        ;;
    run)
        start_server
        ;;
    send-stop)
        stop_server
        ;;
    command)
        shift
        server_command "$@"
        ;;
    start|stop|restart)
        service_control "$cmd"
        ;;
    install)
        install_server
        ;;
    update|auto-update)
        auto_update
        ;;
    backup)
        backup_server
        ;;
    configure)
        configure_systemd
        ;;
    restore)
        restore_server "${2:-}"
        ;;
    notify)
        notify_command "${2:-}"
        ;;
    status)
        status_command "${2:-server}"
        ;;
    logs)
        shift
        logs_command "$@"
        ;;
    timers)
        timers_command
        ;;
    uninstall)
        uninstall_systemd
        ;;
    -h|--help|help|"")
        usage
        ;;
    *)
        echo "不明なコマンドです: $cmd" >&2
        usage >&2
        exit 1
        ;;
esac
