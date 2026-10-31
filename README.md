# bdsctl

LinuxのMinecraft Bedrock Dedicated Serverを管理するRust製の単一実行ファイルです。BDSの監督とDiscord受付だけが常駐し、バックアップ・更新・復元は必要なときだけ実行します。対象は1台・1ワールド環境、systemd、cgroup v2、NetherNetのTCP受付を備えたBDSです。

## 構成

| 保存先 | 内容・所有者 |
| --- | --- |
| Gitチェックアウト | Rustソース・設定例・設置手順。ワールドと秘密情報を置かない |
| `/usr/local/bin/bdsctl` | 検証済みのLinux用バイナリ。root所有 |
| `/etc/bds/config.toml` | 公開可能な設定。root所有・644 |
| `/etc/bds/webhook` | Discord Webhook URLのみ。root:bds-notify・640 |
| `/opt/bds/releases` | 公式BDSの版別ディレクトリと`current`リンク。root所有 |
| `/var/lib/bds/data` | ワールド・設定・独自パック。bds所有 |
| `/var/lib/bds/backups` | 停止中に作成した差分スナップショット。root専用 |
| `/var/lib/bds/state` | 操作の中断記録・更新候補・Discord操作結果。root所有 |
| `/var/lib/bds-supervisor` | 起動失敗・応答異常の診断記録。最新20件 |
| `/var/lib/bds-discord` | Discordの確認・操作受付記録。bds-discord専用 |
| `/run/bds` | 現在の起動状態・正常終了記録・コンソール用ソケット |

`bds.service`が一般ユーザー`bds`でBDSと監督プロセスを起動します。`bds-discord.service`は別ユーザーでHTTP要求を受け付けます。Discord受付がrootで実行できるのは、固定された2つの操作サービスの開始だけです。

## 毎日の動作

日本時間04:30に、ゲーム内へ60秒前・10秒前の予告を送り、BDSを正常停止します。今回起動したBDSの終了コード0と、サービス内のプロセスが残っていないことを確認してからバックアップします。停止できない場合はコピーや強制終了を行いません。

バックアップは`rsync --checksum --link-dest --fsync`で作成します。既存のバックアップと同じファイルだけを共有し、稼働中のワールドとは共有しません。変更のないファイルも比較のため読み取るため、差分量が少なくてもディスクの読み取りは発生します。初回は全量分の容量が必要で、以後も安全側に全量相当＋予約容量の空きを確認します。更新時は失敗後の復旧コピーも含めて現在データの2倍、復元時は復元対象のコピー＋現在データの2倍を追加で確保できるか確認します。

6時間ごとに公式配布元を確認し、更新版を先に取得します。毎日の停止中に更新候補があれば適用し、その後にBDSの起動を確認します。更新失敗時は、更新直前のワールド・設定・BDS本体を戻します。同じ失敗版は自動で再適用しません。管理者が`update`を実行すると再試行できます。

起動完了には、今回の起動ログ、**BDS自身が所有するTCP待受**、コンソール`list`への応答、15秒の安定確認が必要です。稼働中も30秒ごとに確認し、連続3回の異常で正常停止を要求します。systemdによる起動は最初を含め最大3回です。停止が完了するまで別のBDSは起動しません。TCP・コンソールの確認は、実際のMinecraftクライアントによるログイン試験を代替するものではありません。

バックアップ失敗時も、停止前に稼働していたBDSの復帰を試みます。手動で停止していたサーバーは、日次バックアップによって勝手に起動しません。日次タイマーは停止中の取り逃し分を起動直後に実行しません。

通常のバックアップは最新14世代を保持します。保護した世代と復元直前の安全用バックアップは自動削除しません。保持中のバックアップに必要なBDS本体も残します。復元に失敗したデータは`/var/lib/bds/failures`へ退避するため、調査後に管理者が整理してください。同一VPS内のバックアップだけではVPS・ディスク全体の障害に備えられないため、必要な世代は`export`して外部へ保管します。

## 操作

```sh
sudo bdsctl status
sudo bdsctl console 'list'
sudo bdsctl restart
sudo bdsctl backup
sudo bdsctl check-update   # 取得だけ。BDSは停止しない
sudo bdsctl update         # 停止・バックアップ・更新・起動確認
sudo bdsctl snapshots
sudo bdsctl verify BACKUP_ID
sudo bdsctl protect BACKUP_ID
sudo bdsctl unprotect BACKUP_ID
sudo bdsctl restore BACKUP_ID
sudo bdsctl export BACKUP_ID /外部への転送用/backup.tar.zst
sudo bdsctl reboot
```

`restore`はファイルの検証とコピーを済ませてから停止し、現在のワールドを保護バックアップへ保存して置き換えます。復元先はバックアップと別のファイルになります。処理の競合は共通ロックで防止し、手動操作は待たずに拒否、日次操作は最大5分待機します。

異常時は`journalctl -u bds.service`、`/var/lib/bds-supervisor`、`/var/lib/bds/state/transaction.json`を確認してください。中断記録があるときは、停止を確認した上で`sudo bdsctl recover`を実行します。起動前にも同じ復旧処理を実行します。自動起動の上限に達した場合は、原因を直してから`systemctl reset-failed bds.service`で解除します。

## Discord

署名検証付きのHTTP方式を使います。常時接続するDiscord Botは不要です。Caddyで`https://smp.rvo.jp/interactions`を`127.0.0.1:8765/interactions`へ転送します。公開鍵・Application ID・サーバーID・許可ユーザーとロールは`config.toml`に設定します。

`/restart`はMinecraft、`/reboot`はOSの再起動です。両方とも同じ許可設定を使い、操作を始めた本人が60秒以内に確認する必要があります。許可を実行のたびに確認し、古い確認の再利用、同時操作、5分以内の連続操作を拒否します。Discord側でコマンドを表示できても、サーバー側の許可設定を満たさなければ実行できません。

コマンド登録時だけBot Tokenを標準入力から渡します。Tokenは設定ファイルや常駐プロセスに保存しません。シェル履歴にTokenを書かないでください。

```sh
read -r -s -p 'Bot Token: ' BDS_REGISTER_TOKEN; printf '\n'
printf '%s' "$BDS_REGISTER_TOKEN" | sudo bdsctl register-commands
unset BDS_REGISTER_TOKEN
```

これは対象サーバーに登録する**このアプリのコマンド一覧を`restart`と`reboot`に置き換える操作**です。同じアプリの別コマンドがある場合は先に統合してください。登録時に管理者限定の既定権限は付けません。Discordのサーバー設定で対象アプリの表示・実行権限も設定します。

受付後にプロセスが終了して実行結果を確定できなくなっても、同じ操作を自動で送り直しません。結果不明の操作は管理者がサービス状態と操作結果ファイルを確認し、[運用上の復旧手順](docs/operations.md)に従って解除します。

入退室通知は`Steve がログインしました。`・`Steve がログアウトしました。`です。通知は上限付きの別キューで送り、Discordの遅延でBDSの監督を止めません。通知先の障害やキュー超過では通知を失うことがあります。

## ビルド・設置

Rust 1.96以上。実行先はx86_64 Linux、systemd、cgroup v2、rsync 3.x、zstd、Polkit、BDSに必要なシステムライブラリを用意します。HTTPS終端は既存のCaddyを利用します。

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked --bins --examples
sudo python3 tests/linux_acceptance.py target/release/bdsctl target/release/examples/fake_bds
deploy/package.sh
```

CIは同じ検査をLinuxで実行し、Gitコミットを埋め込んだバイナリとチェックサムを成果物にします。VPSではコンパイルしません。`git pull`で更新したソースと同じCI成果物を取得し、`deploy/install.sh ARTIFACT_DIR`で検証して設置します。ソースとバイナリが一致しない場合は設置を拒否します。設置スクリプトはサービスを起動・有効化しません。

常駐する2つのRustプロセスについて、合計PSS20MiB以下・待機時CPU0.1%以下を目標にしています。BDS本体、Caddy、バックアップ処理は別に測定します。検証用の模擬BDSを用いた測定と、本番環境の負荷は区別してください。

初回の切り替えは[移行手順](docs/migration.md)、構造と失敗時の扱いは[設計](docs/design.md)を参照してください。
