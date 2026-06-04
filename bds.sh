#!/usr/bin/env bash

set -euo pipefail

APP_NAME="bds"
SYSTEMD_DIR="/etc/systemd/system"
SCRIPT_PATH="$(readlink -f "$0" 2>/dev/null || realpath "$0")"
BASE_DIR="$(cd "$(dirname "$SCRIPT_PATH")" && pwd)"
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
GAME8_POST_ENABLED="${GAME8_POST_ENABLED:-0}"
GAME8_POST_INTERVAL="${GAME8_POST_INTERVAL:-8h}"
GAME8_POST_BASE_URL="https://game8.jp"
GAME8_POST_ARCHIVE_ID="216448"
CURL_USER_AGENT="${CURL_USER_AGENT:-Mozilla/5.0 bds-installer}"

usage() {
    cat <<EOF
Usage: $0 <command>

Commands:
  install          Download/update Bedrock Dedicated Server files.
  start            Start bedrock_server in the foreground.
  stop             Send "stop" to a running server started by this script.
  auto-update      Check for updates. If updated, restart the systemd service.
  backup           Create a worlds backup without stopping the server.
  restore <archive>
                   Restore worlds from a backup archive.
  game8-post       Periodically post to Game8 when enabled.
  notify <message> Send a Discord notification when DISCORD_WEBHOOK_URL is set.
  install-systemd  Install systemd service and timer for 24/7 operation.
  uninstall-systemd
                   Remove installed systemd service and timer.

Environment:
  BEDROCK_DIR      Install directory. Default: $BASE_DIR/bedrock-server
  CHECK_INTERVAL   systemd timer interval. Default: 6h
  UPDATE_NOTICE_SECONDS
                   Seconds to warn players before update restart. Default: 300
  BACKUP_DIR       Backup directory. Default: $BASE_DIR/backups
  BACKUP_RETENTION_DAYS
                   Days to keep backups. Default: 14
  BACKUP_ON_CALENDAR
                   systemd backup schedule. Default: *-*-* 04:30:00
  BACKUP_HOLD_SECONDS
                   Seconds to wait after save hold. Default: 10
  BACKUP_MIN_FREE_MB
                   Minimum free space before backup. Default: 1024
  GAME8_POST_ENABLED
                   Enable periodic Game8 POST. Set to 1 to enable. Default: 0
  GAME8_POST_INTERVAL
                   systemd timer interval for Game8 POST. Default: 8h
  GAME8_POST_NAME
                   Post name. NAME is also supported. Default: generated per run.
  GAME8_POST_BODY_FILE
                   File path for post body. Recommended for multiline text.
  GAME8_POST_BODY
                   Post body. BODY is also supported. Used when BODY_FILE is unset.
  DISCORD_WEBHOOK_URL
                   Optional Discord webhook URL for update notifications.
EOF
}

need_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "Missing command: $1" >&2
        echo "Install dependencies on Ubuntu: sudo apt update && sudo apt install -y curl jq libarchive-tools unzip" >&2
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
        echo "Missing command: bsdtar or unzip" >&2
        echo "Install dependencies on Ubuntu: sudo apt update && sudo apt install -y libarchive-tools unzip" >&2
        exit 1
    fi
}

require_backup_deps() {
    need_cmds tar find date df du awk grep curl jq mktemp
}

require_game8_post_deps() {
    need_cmds curl sed head date
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

systemctl_enable_now() {
    local unit
    for unit in "$@"; do
        systemctl enable --now "$unit"
    done
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
    if [[ -p "$STDIN_FIFO" ]]; then
        timeout 5s bash -c 'printf "%s\n" "$1" > "$2"' _ "$command" "$STDIN_FIFO" || true
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
        || echo "Discord notification failed." >&2
}

notify_command() {
    local message="${1:-}"
    if [[ -z "$message" ]]; then
        echo "Usage: $0 notify <message>" >&2
        exit 1
    fi

    notify_discord "$message"
    return 0
}

game8_post_enabled() {
    case "$GAME8_POST_ENABLED" in
        1|true|TRUE|True|yes|YES|Yes|on|ON|On|enabled|ENABLED|Enabled)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

game8_post_body() {
    local timestamp="$1"

    if [[ -n "${GAME8_POST_BODY_FILE:-}" ]]; then
        if [[ ! -f "$GAME8_POST_BODY_FILE" ]]; then
            echo "Game8 POST failed: body file not found: $GAME8_POST_BODY_FILE" >&2
            exit 1
        fi
        cat "$GAME8_POST_BODY_FILE"
        return 0
    fi

    printf '%s' "${GAME8_POST_BODY:-${BODY:-backend receive check body ${timestamp}}}"
}

game8_post() {
    if ! game8_post_enabled; then
        echo "Game8 POST is disabled."
        return 0
    fi

    require_game8_post_deps

    local page_url endpoint csrf_token timestamp name body http_status
    page_url="${GAME8_POST_BASE_URL}/${GAME8_POST_ARCHIVE_ID}"
    endpoint="${GAME8_POST_BASE_URL}/api/archive_comments"
    timestamp="$(date +%Y%m%d%H%M%S)"
    name="${GAME8_POST_NAME:-${NAME:-backend-receive-check-${timestamp}}}"
    body="$(game8_post_body "$timestamp")"

    csrf_token="$(
        curl \
            --fail \
            --silent \
            --show-error \
            --location \
            --user-agent "$CURL_USER_AGENT" \
            --header "Accept: text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8" \
            --header "Accept-Language: ja,en-US;q=0.9,en;q=0.8" \
            "$page_url" \
            | sed -nE 's/.*<meta name="csrf-token" content="([^"]+)".*/\1/p' \
            | head -n 1
    )"

    if [[ -z "$csrf_token" ]]; then
        echo "Game8 POST failed: csrf-token を取得できませんでした: $page_url" >&2
        exit 1
    fi

    http_status="$(
        curl \
            --silent \
            --show-error \
            --location \
            --request POST \
            --user-agent "$CURL_USER_AGENT" \
            --header "Accept: application/json" \
            --header "Accept-Language: ja,en-US;q=0.9,en;q=0.8" \
            --header "X-CSRF-Token: $csrf_token" \
            --referer "$page_url" \
            --form "archive_comment[archive_id]=$GAME8_POST_ARCHIVE_ID" \
            --form "archive_comment[name]=$name" \
            --form "archive_comment[body]=$body" \
            --write-out "%{http_code}" \
            --output /dev/null \
            "$endpoint"
    )"

    case "$http_status" in
        200|201|204)
            echo "Game8 POST succeeded: HTTP $http_status archive_id=$GAME8_POST_ARCHIVE_ID name=$name"
            ;;
        *)
            echo "Game8 POST failed: HTTP $http_status archive_id=$GAME8_POST_ARCHIVE_ID name=$name" >&2
            exit 1
            ;;
    esac

}

warn_before_update() {
    local old_version="$1"
    local new_version="$2"
    local seconds="$NOTICE_SECONDS"

    case "$seconds" in
        ''|*[!0-9]*)
            echo "UPDATE_NOTICE_SECONDS must be a non-negative integer: $seconds" >&2
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
            echo "Download links API failed: $api_url" >&2
            continue
        }

        if [[ -n "$url" && "$url" != "null" ]]; then
            echo "Using download links API: $api_url" >&2
            printf '%s\n' "$url"
            return 0
        fi

        echo "Bedrock Linux server download URL not found in: $api_url" >&2
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

extract_server() {
    local url="$1"
    local tmp_zip
    tmp_zip="$(mktemp)"
    trap "rm -f '$tmp_zip'; trap - RETURN" RETURN

    echo "Downloading: $url"
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
        echo "Bedrock Linux server download URL not found." >&2
        exit 1
    fi

    version="$(version_from_url "$url")"
    installed="$(current_version || true)"

    if [[ "$installed" == "$version" && -x "$DEST/bedrock_server" ]]; then
        echo "Already up to date: $version"
        return 0
    fi

    extract_server "$url"
    printf '%s\n' "$version" > "$VERSION_FILE"
    printf '%s\n' "$url" > "$URL_FILE"
    echo "Installed Bedrock Dedicated Server: $version"
}

start_server() {
    if [[ ! -x "$DEST/bedrock_server" ]]; then
        echo "Server binary not found. Run: $0 install" >&2
        exit 1
    fi

    cd "$DEST"
    export LD_LIBRARY_PATH="$DEST:${LD_LIBRARY_PATH:-}"

    rm -f "$STDIN_FIFO"
    mkfifo "$STDIN_FIFO"
    trap 'rm -f "$STDIN_FIFO"' EXIT
    trap 'printf "stop\n" > "$STDIN_FIFO" 2>/dev/null || true' TERM INT

    tail -f "$STDIN_FIFO" | ./bedrock_server
}

stop_server() {
    if [[ ! -p "$STDIN_FIFO" ]]; then
        echo "Server input pipe not found: $STDIN_FIFO" >&2
        exit 1
    fi

    send_server_command "stop"
}

validate_non_negative_integer() {
    local name="$1"
    local value="$2"

    case "$value" in
        ''|*[!0-9]*)
            echo "$name must be a non-negative integer: $value" >&2
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
        echo "Backup archive not found: $archive" >&2
        exit 1
    fi

    archive_contains_worlds "$archive" || validation_status=$?
    if [[ "$validation_status" -eq 2 ]]; then
        echo "Backup archive is not readable: $archive" >&2
        exit 1
    fi

    if [[ "$validation_status" -ne 0 ]]; then
        echo "Backup archive does not contain worlds/: $archive" >&2
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

    echo "Creating backup: $archive"
    notify_discord "バックアップを開始します。"

    local resume_trap_set=0
    if server_accepts_commands; then
        send_server_command "say バックアップを開始します。短時間サーバーが重くなる場合があります。"
        send_server_command "save hold"
        trap 'send_server_command "save resume"' INT TERM EXIT
        resume_trap_set=1
        echo "Waiting ${BACKUP_HOLD_SECONDS}s after save hold before archiving."
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
        echo "Backup failed." >&2
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
        echo "Backup verification failed." >&2
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
    echo "Backup completed: $archive"
}

restore_server() {
    require_backup_deps

    local archive="${1:-}"
    if [[ -z "$archive" ]]; then
        echo "Usage: $0 restore <backup-archive>" >&2
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
        echo "Moved existing worlds to: $restore_backup_dir"
    fi

    if ! tar -xzf "$archive" -C "$DEST"; then
        echo "Restore failed while extracting: $archive" >&2
        if [[ -d "$restore_backup_dir" && ! -e "$current_worlds" ]]; then
            mv "$restore_backup_dir" "$current_worlds"
            echo "Restored previous worlds from: $restore_backup_dir"
        fi
        exit 1
    fi

    if [[ ! -d "$current_worlds" ]]; then
        echo "Restore failed: worlds/ was not extracted." >&2
        if [[ -d "$restore_backup_dir" && ! -e "$current_worlds" ]]; then
            mv "$restore_backup_dir" "$current_worlds"
        fi
        exit 1
    fi

    chown_for_service "$DEST"

    notify_discord "バックアップの復元が完了しました: $(basename "$archive")"
    echo "Restore completed from: $archive"
    if [[ -d "$restore_backup_dir" ]]; then
        echo "Previous worlds kept at: $restore_backup_dir"
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
        echo "Bedrock Linux server download URL not found." >&2
        exit 1
    fi

    version="$(version_from_url "$url")"
    installed="$(current_version || true)"

    if [[ "$installed" == "$version" && -x "$DEST/bedrock_server" ]]; then
        echo "No update: $version"
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
    chown_for_service "$BASE_DIR" "$DEST"
    echo "Updated Bedrock Dedicated Server: ${installed:-none} -> $version"

    if [[ "$service_was_active" -eq 1 ]]; then
        systemctl_run start "$APP_NAME.service"
        notify_discord "アップデートが完了しました: $version。サーバーを再起動しました。"
    else
        notify_discord "アップデートが完了しました: $version。"
    fi
}

install_systemd() {
    if ! is_root; then
        echo "Run as root: sudo $0 install-systemd" >&2
        exit 1
    fi

    local run_user run_group interval backup_on_calendar game8_post_interval
    run_user="${SUDO_USER:-$(id -un)}"
    run_group="$(id -gn "$run_user")"
    interval="${CHECK_INTERVAL:-6h}"
    backup_on_calendar="${BACKUP_ON_CALENDAR:-*-*-* 04:30:00}"
    game8_post_interval="${GAME8_POST_INTERVAL:-8h}"

    install_server
    chown -R "$run_user:$run_group" "$BASE_DIR" "$DEST"

    cat >"$(systemd_unit_path "$APP_NAME.service")" <<EOF
[Unit]
Description=Minecraft Bedrock Dedicated Server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
EnvironmentFile=-/etc/default/$APP_NAME
User=$run_user
Group=$run_group
WorkingDirectory=$DEST
Environment=LD_LIBRARY_PATH=$DEST
ExecStart=$SCRIPT_PATH start
ExecStartPost=$SCRIPT_PATH notify "サーバーが起動しました。"
ExecStop=$SCRIPT_PATH stop
ExecStopPost=$SCRIPT_PATH notify "サーバーが停止しました。"
Restart=always
RestartSec=10
TimeoutStopSec=60

[Install]
WantedBy=multi-user.target
EOF

    cat >"$(systemd_unit_path "$APP_NAME-update.service")" <<EOF
[Unit]
Description=Update Minecraft Bedrock Dedicated Server
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
EnvironmentFile=-/etc/default/$APP_NAME
Environment=SERVICE_USER=$run_user
Environment=SERVICE_GROUP=$run_group
Nice=10
IOSchedulingClass=idle
TimeoutStartSec=30min
ExecStart=$SCRIPT_PATH auto-update
EOF

    cat >"$(systemd_unit_path "$APP_NAME-update.timer")" <<EOF
[Unit]
Description=Periodic Minecraft Bedrock Dedicated Server update check

[Timer]
OnBootSec=10min
OnUnitActiveSec=$interval
Persistent=true

[Install]
WantedBy=timers.target
EOF

    cat >"$(systemd_unit_path "$APP_NAME-backup.service")" <<EOF
[Unit]
Description=Backup Minecraft Bedrock Dedicated Server worlds
After=$APP_NAME.service

[Service]
Type=oneshot
EnvironmentFile=-/etc/default/$APP_NAME
Environment=SERVICE_USER=$run_user
Environment=SERVICE_GROUP=$run_group
Nice=10
IOSchedulingClass=idle
TimeoutStartSec=2h
ExecStart=$SCRIPT_PATH backup
EOF

    cat >"$(systemd_unit_path "$APP_NAME-backup.timer")" <<EOF
[Unit]
Description=Daily Minecraft Bedrock Dedicated Server backup

[Timer]
OnCalendar=$backup_on_calendar
Persistent=true

[Install]
WantedBy=timers.target
EOF

    systemctl_disable_now "$APP_NAME-comment-check.timer"
    remove_systemd_units \
        "$APP_NAME-comment-check.service" \
        "$APP_NAME-comment-check.timer"

    cat >"$(systemd_unit_path "$APP_NAME-game8-post.service")" <<EOF
[Unit]
Description=Periodic Game8 POST
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
EnvironmentFile=-/etc/default/$APP_NAME
Nice=10
IOSchedulingClass=idle
TimeoutStartSec=2min
ExecStart=$SCRIPT_PATH game8-post
EOF

    cat >"$(systemd_unit_path "$APP_NAME-game8-post.timer")" <<EOF
[Unit]
Description=Periodic Game8 POST

[Timer]
OnBootSec=15min
OnUnitActiveSec=$game8_post_interval
Persistent=true

[Install]
WantedBy=timers.target
EOF

    systemctl daemon-reload
    systemctl_enable_now \
        "$APP_NAME.service" \
        "$APP_NAME-update.timer" \
        "$APP_NAME-backup.timer" \
        "$APP_NAME-game8-post.timer"

    echo "Installed and started:"
    echo "  systemctl status $APP_NAME.service"
    echo "  systemctl status $APP_NAME-update.timer"
    echo "  systemctl status $APP_NAME-backup.timer"
    echo "  systemctl status $APP_NAME-game8-post.timer"
}

uninstall_systemd() {
    if ! is_root; then
        echo "Run as root: sudo $0 uninstall-systemd" >&2
        exit 1
    fi

    systemctl_disable_now \
        "$APP_NAME-game8-post.timer" \
        "$APP_NAME-comment-check.timer" \
        "$APP_NAME-backup.timer" \
        "$APP_NAME-update.timer" \
        "$APP_NAME.service"
    remove_systemd_units \
        "$APP_NAME.service" \
        "$APP_NAME-update.service" \
        "$APP_NAME-update.timer" \
        "$APP_NAME-backup.service" \
        "$APP_NAME-backup.timer" \
        "$APP_NAME-game8-post.service" \
        "$APP_NAME-game8-post.timer" \
        "$APP_NAME-comment-check.service" \
        "$APP_NAME-comment-check.timer"
    systemctl daemon-reload
    echo "Removed systemd units."
}

cmd="${1:-}"
case "$cmd" in
    install)
        install_server
        ;;
    start)
        start_server
        ;;
    stop)
        stop_server
        ;;
    auto-update)
        auto_update
        ;;
    backup)
        backup_server
        ;;
    game8-post)
        game8_post
        ;;
    restore)
        restore_server "${2:-}"
        ;;
    notify)
        notify_command "${2:-}"
        ;;
    install-systemd)
        install_systemd
        ;;
    uninstall-systemd)
        uninstall_systemd
        ;;
    -h|--help|help|"")
        usage
        ;;
    *)
        echo "Unknown command: $cmd" >&2
        usage >&2
        exit 1
        ;;
esac
