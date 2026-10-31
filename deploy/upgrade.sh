#!/usr/bin/env bash
# Routine upgrades only. Initial migration uses apply.sh after an explicit stop.
set -euo pipefail
cd "$(dirname "$0")/.."
[[ $# = 3 ]] || { echo 'Usage: deploy/upgrade.sh SSH_HOST BUNDLE_DIR COMMIT' >&2; exit 1; }
host=$1; bundle=$2; commit=$3
[[ $host != -* && $host != *$'\n'* && $bundle =~ ^/var/tmp/bdsctl-deploy-[A-Za-z0-9]+/bdsctl-linux-x86_64$ && $commit =~ ^[0-9a-f]{40}$ ]] || { echo '引数が不正です' >&2; exit 1; }
ssh_options=(-o BatchMode=yes -o IPQoS=none)
if [[ -n ${BDS_SSH_KEY:-} ]]; then ssh_options+=(-i "$BDS_SSH_KEY" -o IdentitiesOnly=yes); fi
# Both procedures come from the same commit as the binary, and live only in stdin.
{
    printf 'install_bundle() {\n'
    git show "$commit:deploy/install.sh"
    printf '\n}\n'
    git show "$commit:deploy/upgrade-remote.sh"
} | ssh "${ssh_options[@]}" "$host" "sh -s -- '$bundle' '$commit'"
