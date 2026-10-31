#!/bin/sh
# Build machine only. Produces a source-free, commit-bound bundle.
set -eu
cd "$(dirname "$0")/.."
output=${1:-dist}
binary=${2:-target/x86_64-unknown-linux-musl/release/bdsctl}
commit=$(git rev-parse HEAD)
[ -z "$(git status --porcelain --untracked-files=normal)" ] || { echo 'コミット済みのクリーンなソースから配布してください' >&2; exit 1; }
version=$("$binary" --version)
case "$version" in bdsctl\ *" ($commit)") ;; *) echo 'バイナリとHEADのコミットが一致しません' >&2; exit 1;; esac
[ ! -e "$output" ] || [ -z "$(ls -A "$output")" ] || { echo '出力先を空にしてください' >&2; exit 1; }
mkdir -p "$output"
staging=$(mktemp -d)
trap 'rm -rf "$staging"' EXIT HUP INT TERM
bundle="$staging/bdsctl-linux-x86_64"
mkdir "$bundle"
install -m 0755 "$binary" "$bundle/bdsctl"
install -m 0644 LICENSE "$bundle/LICENSE"
install -m 0644 bds.toml.example "$bundle/bds.toml.example"
install -m 0644 deploy/Caddyfile.example "$bundle/Caddyfile.example"
if [ -n "${BDS_VALIDATION_REPORT:-}" ]; then install -m 0644 "$BDS_VALIDATION_REPORT" "$bundle/validation.json"; fi
python3 - "$bundle" "$commit" "$version" <<'PY'
import sys,pathlib,json,hashlib,os
root=pathlib.Path(sys.argv[1]);metadata={'schema':1,'commit':sys.argv[2],'version':sys.argv[3],'target':'x86_64-unknown-linux-musl','binary_sha256':hashlib.sha256((root/'bdsctl').read_bytes()).hexdigest()}
if os.environ.get('GITHUB_RUN_ID'):metadata['workflow_run']='https://github.com/'+os.environ['GITHUB_REPOSITORY']+'/actions/runs/'+os.environ['GITHUB_RUN_ID']
(root/'BUILD.json').write_text(json.dumps(metadata,indent=2)+'\n')
(root/'SHA256SUMS').write_text(''.join(hashlib.sha256(p.read_bytes()).hexdigest()+'  '+p.name+'\n' for p in sorted(root.iterdir()) if p.is_file() and p.name!='SHA256SUMS'))
PY
COPYFILE_DISABLE=1 tar -czf "$output/bdsctl-linux-x86_64.tar.gz" -C "$staging" bdsctl-linux-x86_64
(cd "$output" && sha256sum bdsctl-linux-x86_64.tar.gz > bdsctl-linux-x86_64.tar.gz.sha256)
