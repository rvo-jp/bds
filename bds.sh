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
BACKUP_HOLD_SECONDS="${BACKUP_HOLD_SECONDS:-10}"
BACKUP_MIN_FREE_MB="${BACKUP_MIN_FREE_MB:-1024}"
SERVER_NAME_FORMAT="${SERVER_NAME_FORMAT-}"
CURL_USER_AGENT="${CURL_USER_AGENT:-Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36}"

usage() {
    cat <<EOF
使い方: $0 <コマンド>

コマンド:
  start            BDS 本体と systemd 設定を準備して bds.service を起動します。
  stop             bds.service を停止します。
  restart          BDS 本体と systemd 設定を準備して bds.service を再起動します。
  run              bedrock_server をフォアグラウンドで直接起動します。
  send-stop        起動中の bedrock_server へ stop を送信します。
  command <cmd>    起動中の bedrock_server へワールド内コマンドを送信します。
  backup           サーバーを停止せずに worlds をバックアップします。
  restore <archive>
                   バックアップアーカイブから worlds を復元します。
  status [target]  systemd unit の状態を確認します。target: server/update/backup/*-timer
  logs [target]    journal を表示します。target: server/update/backup
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
  BACKUP_HOLD_SECONDS
                   save hold 後に待機する秒数。既定: 10
  BACKUP_MIN_FREE_MB
                   バックアップ前に確保する最低空き容量 MB。既定: 1024
  SERVER_NAME_FORMAT
                   server-name の形式。%v=プレイヤー向け表記、%V=BDSフル表記。未設定または空なら自動変更しません。
  DISCORD_WEBHOOK_URL
                   Discord 通知用 Webhook URL。未設定なら通知しません。
EOF
}

need_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "必要なコマンドが見つかりません: $1" >&2
        echo "Ubuntu では依存パッケージを入れてください: sudo apt update && sudo apt install -y curl jq libarchive-tools unzip" >&2
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
    need_cmds curl jq
    if ! command -v bsdtar >/dev/null 2>&1 && ! command -v unzip >/dev/null 2>&1; then
        echo "必要なコマンドが見つかりません: bsdtar または unzip" >&2
        echo "Ubuntu では依存パッケージを入れてください: sudo apt update && sudo apt install -y libarchive-tools unzip" >&2
        exit 1
    fi
}

require_backup_deps() {
    need_cmds tar find date df du awk grep curl jq mktemp
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
    local unit
    for unit in "$@"; do
        systemctl disable --now "$unit" 2>/dev/null || true
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
            configure_systemd
            systemctl_run start "$APP_NAME.service"
            ;;
        restart)
            configure_systemd
            systemctl_run restart "$APP_NAME.service"
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
    local unit
    unit="$(systemd_unit_for_target "${1:-server}")"
    systemctl status "$unit"
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
        --location \
        --user-agent "$CURL_USER_AGENT" \
        "$url"
}

curl_download() {
    local url="$1"
    local output="$2"

    curl \
        --fail \
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

notify_discord() {
    local message="$1"
    if [[ -z "${DISCORD_WEBHOOK_URL:-}" ]]; then
        return 0
    fi

    local payload
    payload="$(jq -n --arg content "$message" '{content: $content}')"
    curl -fsS \
        -H "Content-Type: application/json" \
        -d "$payload" \
        "$DISCORD_WEBHOOK_URL" \
        >/dev/null \
        || echo "Discord 通知に失敗しました。" >&2
}

notify_command() {
    local message="${1:-}"
    if [[ -z "$message" ]]; then
        echo "使い方: $0 notify <message>" >&2
        exit 1
    fi

    notify_discord "$message"
    return 0
}

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

    notify_discord "アップデートが見つかりました: ${old_version:-none} -> $new_version。${seconds}秒後に再起動します。"
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
    local props="$DEST/server.properties"
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

extract_server() {
    local url="$1"
    local tmp_zip
    tmp_zip="$(mktemp)"
    trap "rm -f '$tmp_zip'; trap - RETURN" RETURN

    echo "ダウンロード中: $url"
    curl_download "$url" "$tmp_zip"

    mkdir -p "$DEST"

    local excludes=()
    [[ -f "$DEST/allowlist.json" ]] && excludes+=("allowlist.json")
    [[ -f "$DEST/permissions.json" ]] && excludes+=("permissions.json")
    [[ -f "$DEST/server.properties" ]] && excludes+=("server.properties")
    [[ -f "$DEST/whitelist.json" ]] && excludes+=("whitelist.json")
    [[ -d "$DEST/worlds" ]] && excludes+=("worlds/*")
    [[ -d "$DEST/resource_packs" ]] && excludes+=("resource_packs/*")
    [[ -d "$DEST/behavior_packs" ]] && excludes+=("behavior_packs/*")

    if command -v bsdtar >/dev/null 2>&1; then
        local bsdtar_excludes=()
        local item
        for item in "${excludes[@]}"; do
            bsdtar_excludes+=("--exclude=$item")
        done
        bsdtar -xf "$tmp_zip" -C "$DEST" "${bsdtar_excludes[@]}"
    else
        local unzip_excludes=()
        local item
        for item in "${excludes[@]}"; do
            unzip_excludes+=("-x" "$item")
        done
        unzip -oq "$tmp_zip" -d "$DEST" "${unzip_excludes[@]}"
    fi

    chmod +x "$DEST/bedrock_server"
}

install_server() {
    require_deps

    local url version installed
    url="$(download_url)"
    if [[ -z "$url" || "$url" == "null" ]]; then
        echo "Bedrock Linux サーバーのダウンロード URL が見つかりません。" >&2
        exit 1
    fi

    version="$(version_from_url "$url")"
    installed="$(current_version || true)"

    if [[ "$installed" == "$version" && -x "$DEST/bedrock_server" ]]; then
        update_server_name_version "$version"
        echo "すでに最新版です: $version"
        return 0
    fi

    extract_server "$url"
    printf '%s\n' "$version" > "$VERSION_FILE"
    printf '%s\n' "$url" > "$URL_FILE"
    update_server_name_version "$version"
    echo "Bedrock Dedicated Server をインストールしました: $version"
}

start_server() {
    if [[ ! -x "$DEST/bedrock_server" ]]; then
        echo "サーバー実行ファイルが見つかりません。先に実行してください: $0 start" >&2
        exit 1
    fi

    cd "$DEST"
    export LD_LIBRARY_PATH="$DEST:${LD_LIBRARY_PATH:-}"

    rm -f "$STDIN_FIFO"
    mkfifo "$STDIN_FIFO"
    exec 3<>"$STDIN_FIFO"
    trap 'printf "stop\n" >&3 2>/dev/null || true' TERM INT
    trap 'exec 3>&- 2>/dev/null || true; rm -f "$STDIN_FIFO"' EXIT

    ./bedrock_server <&3
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
    required_mb="$((source_mb + BACKUP_MIN_FREE_MB))"

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

validate_backup_archive() {
    local archive="$1"
    local validation_status=0

    if [[ ! -f "$archive" ]]; then
        echo "バックアップアーカイブが見つかりません: $archive" >&2
        exit 1
    fi

    archive_contains_worlds "$archive" || validation_status=$?
    if [[ "$validation_status" -eq 2 ]]; then
        echo "バックアップアーカイブを読み取れません: $archive" >&2
        exit 1
    fi

    if [[ "$validation_status" -ne 0 ]]; then
        echo "バックアップアーカイブに worlds/ が含まれていません: $archive" >&2
        exit 1
    fi
}

archive_contains_worlds() {
    local archive="$1"
    local list_file

    list_file="$(mktemp)"
    if ! tar -tzf "$archive" > "$list_file"; then
        rm -f "$list_file"
        return 2
    fi

    if grep -q '^worlds/' "$list_file"; then
        rm -f "$list_file"
        return 0
    fi

    rm -f "$list_file"
    return 1
}

backup_server() {
    require_backup_deps
    validate_non_negative_integer "BACKUP_RETENTION_DAYS" "$BACKUP_RETENTION_DAYS"
    validate_non_negative_integer "BACKUP_HOLD_SECONDS" "$BACKUP_HOLD_SECONDS"

    local worlds_dir="$DEST/worlds"
    if [[ ! -d "$worlds_dir" ]]; then
        local message="ワールドディレクトリが見つからないため、バックアップを中止しました: $worlds_dir"
        echo "$message" >&2
        notify_discord "$message"
        exit 1
    fi

    mkdir -p "$BACKUP_DIR"
    ensure_backup_space "$worlds_dir" "$BACKUP_DIR"

    local timestamp archive status=0
    timestamp="$(date +%Y%m%d-%H%M%S)"
    archive="$BACKUP_DIR/bds-worlds-$timestamp.tar.gz"

    echo "バックアップを作成します: $archive"
    notify_discord "バックアップを開始します。"

    local resume_trap_set=0
    if server_accepts_commands; then
        send_server_command "say バックアップを開始します。短時間サーバーが重くなる場合があります。"
        send_server_command "save hold"
        trap 'send_server_command "save resume"' INT TERM EXIT
        resume_trap_set=1
        echo "save hold 後、アーカイブ作成前に ${BACKUP_HOLD_SECONDS} 秒待機します。"
        sleep "$BACKUP_HOLD_SECONDS"
    fi

    if ! tar -czf "$archive" -C "$DEST" worlds; then
        status=1
    fi

    if [[ "$resume_trap_set" -eq 1 ]]; then
        send_server_command "save resume"
        trap - INT TERM EXIT
    fi

    if [[ "$status" -ne 0 ]]; then
        rm -f "$archive"
        notify_discord "バックアップに失敗しました。"
        if server_accepts_commands; then
            send_server_command "say バックアップに失敗しました。"
        fi
        echo "バックアップに失敗しました。" >&2
        exit 1
    fi

    local validation_status=0
    archive_contains_worlds "$archive" || validation_status=$?
    if [[ "$validation_status" -ne 0 ]]; then
        rm -f "$archive"
        notify_discord "バックアップ検証に失敗しました。"
        if server_accepts_commands; then
            send_server_command "say バックアップ検証に失敗しました。"
        fi
        echo "バックアップ検証に失敗しました。" >&2
        exit 1
    fi

    find "$BACKUP_DIR" \
        -type f \
        -name 'bds-worlds-*.tar.gz' \
        -mtime +"$BACKUP_RETENTION_DAYS" \
        -delete

    chown_for_service "$BACKUP_DIR"

    notify_discord "バックアップが完了しました: $(basename "$archive")"
    if server_accepts_commands; then
        send_server_command "say バックアップが完了しました。"
    fi
    echo "バックアップが完了しました: $archive"
}

restore_server() {
    require_backup_deps

    local archive="${1:-}"
    if [[ -z "$archive" ]]; then
        echo "使い方: $0 restore <backup-archive>" >&2
        exit 1
    fi

    validate_backup_archive "$archive"
    mkdir -p "$DEST"

    local service_was_active=0
    if command -v systemctl >/dev/null 2>&1 && systemctl is-active --quiet "$APP_NAME.service"; then
        service_was_active=1
        notify_discord "バックアップの復元のため、サーバーを停止します。"
        systemctl_run stop "$APP_NAME.service"
    fi

    local timestamp current_worlds restore_backup_dir
    timestamp="$(date +%Y%m%d-%H%M%S)"
    current_worlds="$DEST/worlds"
    restore_backup_dir="$DEST/worlds.pre-restore-$timestamp"

    if [[ -e "$current_worlds" ]]; then
        mv "$current_worlds" "$restore_backup_dir"
        echo "既存の worlds を退避しました: $restore_backup_dir"
    fi

    if ! tar -xzf "$archive" -C "$DEST"; then
        echo "復元に失敗しました。展開できませんでした: $archive" >&2
        if [[ -d "$restore_backup_dir" && ! -e "$current_worlds" ]]; then
            mv "$restore_backup_dir" "$current_worlds"
            echo "退避済み worlds を戻しました: $restore_backup_dir"
        fi
        exit 1
    fi

    if [[ ! -d "$current_worlds" ]]; then
        echo "復元に失敗しました: worlds/ が展開されませんでした。" >&2
        if [[ -d "$restore_backup_dir" && ! -e "$current_worlds" ]]; then
            mv "$restore_backup_dir" "$current_worlds"
        fi
        exit 1
    fi

    chown_for_service "$DEST"

    notify_discord "バックアップの復元が完了しました: $(basename "$archive")"
    echo "復元が完了しました: $archive"
    if [[ -d "$restore_backup_dir" ]]; then
        echo "以前の worlds はここに残しています: $restore_backup_dir"
    fi

    if [[ "$service_was_active" -eq 1 ]]; then
        systemctl_run start "$APP_NAME.service"
        notify_discord "復元後にサーバーを再起動しました。"
    fi
}

auto_update() {
    require_deps

    local url version installed service_was_active=0
    url="$(download_url)"
    if [[ -z "$url" || "$url" == "null" ]]; then
        echo "Bedrock Linux サーバーのダウンロード URL が見つかりません。" >&2
        exit 1
    fi

    version="$(version_from_url "$url")"
    installed="$(current_version || true)"

    if [[ "$installed" == "$version" && -x "$DEST/bedrock_server" ]]; then
        echo "更新はありません: $version"
        return 0
    fi

    if command -v systemctl >/dev/null 2>&1 && systemctl is-active --quiet "$APP_NAME.service"; then
        service_was_active=1
        warn_before_update "$installed" "$version"
        systemctl_run stop "$APP_NAME.service"
    fi

    notify_discord "アップデートを開始します: ${installed:-none} -> $version。"
    extract_server "$url"
    printf '%s\n' "$version" > "$VERSION_FILE"
    printf '%s\n' "$url" > "$URL_FILE"
    update_server_name_version "$version"
    chown_for_service "$BASE_DIR" "$DEST"
    echo "Bedrock Dedicated Server を更新しました: ${installed:-none} -> $version"

    if [[ "$service_was_active" -eq 1 ]]; then
        systemctl_run start "$APP_NAME.service"
        notify_discord "アップデートが完了しました: $version。サーバーを再起動しました。"
    else
        notify_discord "アップデートが完了しました: $version。"
    fi
}

configure_systemd() {
    if ! is_root; then
        echo "root 権限で実行してください: sudo $0 start" >&2
        exit 1
    fi

    local run_user run_group interval backup_on_calendar
    run_user="${SUDO_USER:-$(id -un)}"
    run_group="$(id -gn "$run_user")"
    interval="${CHECK_INTERVAL:-6h}"
    backup_on_calendar="${BACKUP_ON_CALENDAR:-*-*-* 04:30:00}"

    install_server
    chown -R "$run_user:$run_group" "$BASE_DIR" "$DEST"

    cat >"$(systemd_unit_path "$APP_NAME.service")" <<EOF
[Unit]
Description=Minecraft Bedrock Dedicated Server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$run_user
Group=$run_group
WorkingDirectory=$DEST
Environment=BDS_CONFIG=$CONFIG_FILE
Environment=LD_LIBRARY_PATH=$DEST
ExecStart=$SCRIPT_PATH run
ExecStartPost=$SCRIPT_PATH notify "サーバーが起動しました。"
ExecStop=$SCRIPT_PATH send-stop
ExecStopPost=$SCRIPT_PATH notify "サーバーが停止しました。"
Restart=always
RestartSec=10
TimeoutStopSec=60

[Install]
WantedBy=multi-user.target
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
Description=Minecraft Bedrock Dedicated Server ワールドバックアップ
After=$APP_NAME.service

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

    systemctl daemon-reload
    systemctl_enable "$APP_NAME.service"
    systemctl_enable_now \
        "$APP_NAME-update.timer" \
        "$APP_NAME-backup.timer"

    echo "systemd 設定を更新しました:"
    echo "  $SCRIPT_PATH status"
    echo "  $SCRIPT_PATH status update-timer"
    echo "  $SCRIPT_PATH status backup-timer"
}

uninstall_systemd() {
    if ! is_root; then
        echo "root 権限で実行してください: sudo $0 uninstall" >&2
        exit 1
    fi

    systemctl_disable_now \
        "$APP_NAME-backup.timer" \
        "$APP_NAME-update.timer" \
        "$APP_NAME.service"
    remove_systemd_units \
        "$APP_NAME.service" \
        "$APP_NAME-update.service" \
        "$APP_NAME-update.timer" \
        "$APP_NAME-backup.service" \
        "$APP_NAME-backup.timer"
    systemctl daemon-reload
    echo "systemd unit を削除しました。"
}

cmd="${1:-}"
case "$cmd" in
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
    update|auto-update)
        auto_update
        ;;
    backup)
        backup_server
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
