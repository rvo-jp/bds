#!/usr/bin/env bash
# Downloads a successful CI run and verifies its bundle on the target. No service changes.
set -euo pipefail
cd "$(dirname "$0")/.."
[[ $# -ge 1 && $# -le 2 ]] || { echo 'Usage: deploy/stage.sh SSH_HOST [COMMIT]' >&2; exit 1; }
host=$1
[[ $host != -* && $host != *$'\n'* ]] || exit 1
commit=$(git rev-parse "${2:-HEAD}^{commit}")
[[ $commit =~ ^[0-9a-f]{40}$ ]] || exit 1
repository=rvo-jp/bds
run=$(gh run list --repo "$repository" --workflow ci.yml --commit "$commit" --status success --json databaseId --jq '.[0].databaseId // empty')
[[ -n $run ]] || { echo '指定コミットのCI成功を先に確認してください' >&2; exit 1; }
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT
gh run download "$run" --repo "$repository" --name "bdsctl-linux-x86_64-$commit" --dir "$temporary"
(cd "$temporary" && shasum -a 256 --check bdsctl-linux-x86_64.tar.gz.sha256)
ssh_options=(-o BatchMode=yes -o IPQoS=none)
if [[ -n ${BDS_SSH_KEY:-} ]]; then ssh_options+=(-i "$BDS_SSH_KEY" -o IdentitiesOnly=yes); fi
remote=$(ssh "${ssh_options[@]}" "$host" 'mktemp -d /var/tmp/bdsctl-deploy-XXXXXXXX')
[[ $remote =~ ^/var/tmp/bdsctl-deploy-[A-Za-z0-9]+$ ]] || { echo '転送先が不正です' >&2; exit 1; }
scp -O -l "${BDS_SCP_KBIT:-1024}" "${ssh_options[@]}" "$temporary/bdsctl-linux-x86_64.tar.gz" "$host:$remote/"
ssh "${ssh_options[@]}" "$host" "tar -xzf '$remote/bdsctl-linux-x86_64.tar.gz' -C '$remote'"
git show "$commit:deploy/install.sh" | ssh "${ssh_options[@]}" "$host" "sh -s -- --check '$remote/bdsctl-linux-x86_64' '$commit'"
printf '\n%s\n' "配置・検証済み: $remote/bdsctl-linux-x86_64" "コミット: $commit" 'サービスと設定は変更していません。停止後にdeploy/apply.shで適用してください。'
