#!/usr/bin/env bash

set -euo pipefail

APP_NAME="bds"
SCRIPT_PATH="$(readlink -f "$0" 2>/dev/null || realpath "$0")"
BASE_DIR="$(cd "$(dirname "$SCRIPT_PATH")" && pwd)"
DEST="${BEDROCK_DIR:-$BASE_DIR/bedrock-server}"
API_URL="https://net.web.minecraft-services.net/api/v1.0/download/links"
VERSION_FILE="$DEST/.installed-version"
URL_FILE="$DEST/.installed-url"
STDIN_FIFO="$DEST/.server.stdin"

usage() {
    cat <<EOF
Usage: $0 <command>

Commands:
  install          Download/update Bedrock Dedicated Server files.
  start            Start bedrock_server in the foreground.
  stop             Send "stop" to a running server started by this script.
  auto-update      Check for updates. If updated, restart the systemd service.
  install-systemd  Install systemd service and timer for 24/7 operation.
  uninstall-systemd
                   Remove installed systemd service and timer.

Environment:
  BEDROCK_DIR      Install directory. Default: $BASE_DIR/bedrock-server
  CHECK_INTERVAL   systemd timer interval. Default: 1h
EOF
}

need_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "Missing command: $1" >&2
        echo "Install dependencies on Ubuntu: sudo apt update && sudo apt install -y curl jq libarchive-tools unzip" >&2
        exit 1
    fi
}

require_deps() {
    need_cmd curl
    need_cmd jq
    if ! command -v bsdtar >/dev/null 2>&1 && ! command -v unzip >/dev/null 2>&1; then
        echo "Missing command: bsdtar or unzip" >&2
        echo "Install dependencies on Ubuntu: sudo apt update && sudo apt install -y libarchive-tools unzip" >&2
        exit 1
    fi
}

systemctl_run() {
    if [[ "$(id -u)" -eq 0 ]]; then
        systemctl "$@"
    else
        sudo systemctl "$@"
    fi
}

download_url() {
    curl -fsSL "$API_URL" \
        | jq -r '.result.links[] | select(.downloadType == "serverBedrockLinux") | .downloadUrl'
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
    trap 'rm -f "$tmp_zip"' RETURN

    echo "Downloading: $url"
    curl -fL "$url" -o "$tmp_zip"

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

    timeout 5s bash -c 'printf "stop\n" > "$1"' _ "$STDIN_FIFO"
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
        systemctl_run stop "$APP_NAME.service"
    fi

    extract_server "$url"
    printf '%s\n' "$version" > "$VERSION_FILE"
    printf '%s\n' "$url" > "$URL_FILE"
    if [[ "$(id -u)" -eq 0 && -n "${SERVICE_USER:-}" && -n "${SERVICE_GROUP:-}" ]]; then
        chown -R "$SERVICE_USER:$SERVICE_GROUP" "$BASE_DIR" "$DEST"
    fi
    echo "Updated Bedrock Dedicated Server: ${installed:-none} -> $version"

    if [[ "$service_was_active" -eq 1 ]]; then
        systemctl_run start "$APP_NAME.service"
    fi
}

install_systemd() {
    if [[ "$(id -u)" -ne 0 ]]; then
        echo "Run as root: sudo $0 install-systemd" >&2
        exit 1
    fi

    local run_user run_group interval
    run_user="${SUDO_USER:-$(id -un)}"
    run_group="$(id -gn "$run_user")"
    interval="${CHECK_INTERVAL:-1h}"

    install_server
    chown -R "$run_user:$run_group" "$BASE_DIR" "$DEST"

    cat >/etc/systemd/system/$APP_NAME.service <<EOF
[Unit]
Description=Minecraft Bedrock Dedicated Server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$run_user
Group=$run_group
WorkingDirectory=$DEST
Environment=LD_LIBRARY_PATH=$DEST
ExecStart=$SCRIPT_PATH start
ExecStop=$SCRIPT_PATH stop
Restart=always
RestartSec=10
TimeoutStopSec=60

[Install]
WantedBy=multi-user.target
EOF

    cat >/etc/systemd/system/$APP_NAME-update.service <<EOF
[Unit]
Description=Update Minecraft Bedrock Dedicated Server
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
Environment=SERVICE_USER=$run_user
Environment=SERVICE_GROUP=$run_group
ExecStart=$SCRIPT_PATH auto-update
EOF

    cat >/etc/systemd/system/$APP_NAME-update.timer <<EOF
[Unit]
Description=Periodic Minecraft Bedrock Dedicated Server update check

[Timer]
OnBootSec=10min
OnUnitActiveSec=$interval
Persistent=true

[Install]
WantedBy=timers.target
EOF

    systemctl daemon-reload
    systemctl enable --now "$APP_NAME.service"
    systemctl enable --now "$APP_NAME-update.timer"

    echo "Installed and started:"
    echo "  systemctl status $APP_NAME.service"
    echo "  systemctl status $APP_NAME-update.timer"
}

uninstall_systemd() {
    if [[ "$(id -u)" -ne 0 ]]; then
        echo "Run as root: sudo $0 uninstall-systemd" >&2
        exit 1
    fi

    systemctl disable --now "$APP_NAME-update.timer" 2>/dev/null || true
    systemctl disable --now "$APP_NAME.service" 2>/dev/null || true
    rm -f \
        "/etc/systemd/system/$APP_NAME.service" \
        "/etc/systemd/system/$APP_NAME-update.service" \
        "/etc/systemd/system/$APP_NAME-update.timer"
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
