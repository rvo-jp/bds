# bdsctl

LinuxのMinecraft Bedrock Dedicated Serverを管理するRust製の単一実行ファイルです。BDSの監督とDiscord受付だけが常駐し、バックアップ・更新・復元は必要なときだけ実行します。対象は1台・1ワールド環境、systemd、cgroup v2、NetherNetのTCP受付を備えたBDSです。

## 構成

| 保存先 | 内容・所有者 |
| --- | --- |
| Gitチェックアウト | Rustソース・設定例・設置手順。ワールドと秘密情報を置かない |
| `/usr/local/bin/bdsctl` | 検証済みのLinux用バイナリ。root所有 |
| `/etc/bds/config.toml` | 公開可能な設定。root所有・644 |
| `/etc/bds/webhook` | Discord Webhook URLのみ。root:bds-notify・640 |
| `/opt/bedrock/releases` | 公式BDSの版別ディレクトリと`current`リンク。root所有 |
| `/var/lib/bds/data` | ワールド・設定・独自パック。bds所有 |
| `/var/lib/bds/backups` | 停止中に作成した差分スナップショット。root専用 |
| `/var/lib/bds/state` | 操作の中断記録・更新候補・Discord操作結果。root所有 |
| `/var/lib/bds-supervisor` | 起動失敗・予期しない終了の診断記録。最新20件 |
| `/var/lib/bds-discord` | Discordの確認・操作受付記録。bds-discord専用 |
| `/run/bds` | 現在の起動状態・正常終了記録・コンソール用ソケット |

`bds.service`が一般ユーザー`bds`でBDSと監督プロセスを起動します。`bds-discord.service`は別ユーザーでHTTP要求を受け付けます。Discord受付がrootで実行できるのは、固定された2つの操作サービスの開始だけです。

## 毎日の動作

日本時間04:30に、ゲーム内へ60秒前・10秒前の予告を送り、BDSを正常停止します。今回起動したBDSの終了コード0と、サービス内のプロセスが残っていないことを確認してからバックアップします。停止できない場合はコピーや強制終了を行いません。

バックアップは`rsync --checksum --link-dest --fsync`で作成します。既存のバックアップと同じファイルだけを共有し、稼働中のワールドとは共有しません。変更のないファイルも比較のため読み取るため、差分量が少なくてもディスクの読み取りは発生します。初回は全量分の容量が必要で、以後も安全側に全量相当＋予約容量の空きを確認します。停止前の容量見積もりではLevelDBによるファイルの作成・削除を許容し、停止後に全件を厳密に再確認します。リンク・特殊ファイル・読み取り権限のエラーは無視しません。更新時は失敗後の復旧コピーも含めて現在データの2倍、復元時は復元対象のコピー＋現在データの2倍を追加で確保できるか確認します。

毎時00分を基準に最大5分ずらして公式配布元を確認します。現在より新しい版がある場合は、その場で取得 → ゲーム内に60秒前・10秒前の予告 → 正常停止 → バックアップ → 更新適用 → 起動確認まで実行します。新しい版がなければ停止もバックアップもしません。確認・取得に失敗した場合も、稼働中のBDSは停止しません。

更新失敗時は、更新直前のワールド・設定・BDS本体を戻します。同じ失敗版は自動で再適用しません。管理者が`update`を実行すると再試行できます。手動停止中や起動・停止の途中では、更新版の取得までで適用を保留し、次の定期確認で稼働中なら適用します。定期確認が古い版を返しても自動でダウングレードしません。毎朝04:30のバックアップは更新の有無に関係なく別に実行します。

起動前の中断復旧は全件検証・コピーが完了するまで待ち、systemdの起動制限時間で打ち切りません。Minecraft本体の起動確認は監督内の期限（既定120秒、確認後の安定待ち15秒）で管理します。起動完了には、今回の起動ログ、**BDS自身が所有するTCP待受**、コンソール`list`への応答、15秒の安定確認が必要です。起動完了後はログ・コンソール操作・OSからの停止要求・BDSの終了通知を待ちます。定期的なTCP確認や`list`送信、状態ファイルの定期更新は行いません。起動失敗やBDSの予期しない終了時は、systemdが最初を含め最大3回の起動を試みます。停止が完了するまで別のBDSは起動しません。起動時のTCP・コンソール確認は、実際のMinecraftクライアントによるログイン試験を代替するものではありません。プロセスが残ったまま応答しなくなる状態は自動検知しません。

バックアップ失敗時も、停止前に稼働していたBDSの復帰を試みます。手動で停止していたサーバーは、日次バックアップによって勝手に起動しません。停止済みのデータを通常バックアップに保存するには、同じOS起動中の正常終了記録が必要です。異常停止・記録不明の場合は処理を失敗として終了し、正常な保存世代を削除しません。日次タイマーは停止中の取り逃し分を起動直後に実行しません。更新タイマーは取り逃した場合、起動後にも確認します。

毎朝のバックアップは最新14回分、手動・更新前のバックアップは別枠で最新14回分を保持します（`retention`と`extra_retention`）。実行をスキップ・失敗した日は含まれず、14日連続の保存を保証するものではありません。保護した世代と復元直前の安全用バックアップは自動削除しません。保持中のバックアップに必要なBDS本体も残します。復元に失敗したデータは`/var/lib/bds/failures`へ退避するため、調査後に管理者が整理してください。バックアップはVPS内で保存・管理し、外部保存先への転送は行いません。必要時には`export`で手動書き出しができます。アーカイブには対応するMinecraft本体、ワールド・設定・独自パック、公開管理設定、SHA-256一覧を含めます。Webhookなどの秘密情報、Caddy、通信許可設定と管理ツール自体は含めません。別VPSへの復元は[運用手順](docs/operations.md)を参照してください。

## 操作

```sh
sudo bdsctl status          # 稼働状態・保守・バックアップ・次回予定・空き容量
sudo bdsctl maintenance-begin # 予告・正常停止し、予約処理と再起動を抑止
# ワールド・アドオンなどを編集
sudo bdsctl maintenance-end   # 保守前に稼働していた場合だけ起動
sudo bdsctl inventory       # 保護バックアップ・障害調査用データも一覧
sudo bdsctl console 'list'
sudo bdsctl restart
sudo bdsctl backup
sudo bdsctl auto-update    # 定期更新と同じ。新しい版がある場合だけ更新
sudo bdsctl check-update   # 取得だけ。BDSは停止しない
sudo bdsctl update         # 停止・バックアップ・更新・起動確認
sudo bdsctl snapshots
sudo bdsctl verify BACKUP_ID
sudo bdsctl protect BACKUP_ID
sudo bdsctl unprotect BACKUP_ID
sudo bdsctl restore BACKUP_ID
sudo bdsctl export BACKUP_ID /var/backups/bds-recovery.tar.zst
# 新規VPSへの復元（既存ワールドには上書き不可）
sudo bdsctl import-package /var/backups/bds-recovery.tar.zst
sudo bdsctl reboot
```

`maintenance-begin`が正常に完了してから編集してください。保守状態はVPS再起動後も残り、Minecraftの起動、予約バックアップ・更新、Discordからの再起動を抑止します。日次・更新タイマーは保守中の実行を正常スキップします。終了時に起動できなければ保守状態を残します。保守中も管理者の`restore`・`recover`・`export`は使用できます。

`status`は要求時だけ情報を取得し、停止中でも取得に成功すれば終了コード0です。起動状態記録のJSONが必要な場合は`status-json`を使います。定期監視や定期書き込みは追加しません。

`restore`はファイルの検証とコピーを済ませてから停止し、現在のワールドを保護バックアップへ保存して置き換えます。復元先はバックアップと別のファイルになります。処理の競合は共通ロックで防止し、手動操作は待たずに拒否、日次バックアップと定期更新は最大5分待機します。待機後に現在の版を再確認するため、同じ版を二重に適用しません。

異常時は`journalctl -u bds.service`、`/var/lib/bds-supervisor`、`/var/lib/bds/state/transaction.json`を確認してください。中断記録があるときは、停止を確認した上で`sudo bdsctl recover`を実行します。手動の`recover`はBDSが完全停止している場合だけ許可し、稼働中にはリンクやデータを変更しません。起動前にも同じ復旧処理を実行しますが、サービスの起動前処理自身だけが動いていることを確認します。自動起動の上限に達した場合は、原因を直してから`systemctl reset-failed bds.service`で解除します。

## Discord

署名検証付きのHTTP方式を使います。常時接続するDiscord Botは不要です。Caddyで`https://smp.rvo.jp/interactions`を`127.0.0.1:8765/interactions`へ転送します。受付は読み取り2秒・同時4接続までに制限し、遅い要求で他の操作が詰まるのを防ぎます。公開鍵・Application ID・サーバーID・許可ユーザーとロールは`config.toml`に設定します。

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

## 新規ワールドの初期化

`sudo bdsctl init`はデータのない保存先にだけ新規構築します。初期化・旧データ移行・アーカイブ取り込みは共通の確定処理を使い、`state/import.json`に処理中の版と配置先を記録します。途中で失敗した場合は、停止状態で原因を直し、`init`の再実行または`recover`で再開できます。記録のない既存データは上書き・自動採用しません。

## 開発・配布・設置

開発環境はRust 1.96以上です。ソースは開発PCとGitHubで管理し、本番VPSへは配布しません。VPSに必要なのは、x86_64 Linux、systemd、cgroup v2、rsync 3.x、zstd、Polkit、BDSが要求するシステムライブラリです。HTTPS終端はCaddyを使います。

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
python3 -m unittest discover -s tests -p 'test_distribution.py' -v
```

CIで同じ検査とLinux隔離試験を実行し、静的リンクしたLinuxバイナリ、設定例、ビルド情報、検証結果、チェックサムを1つの配布物にします。ソースや開発ツールを含めません。バイナリには完全なGitコミットIDを埋め込み、配布・設置時に照合します。手元での通常ビルドは`development`として区別し、そのまま本番用に包装できません。

開発PCの`deploy/stage.sh`で、指定コミットの成功したCI成果物をVPSの一時領域へ転送・検証します。ここではサービスを変更しません。初回移行では停止後に`deploy/apply.sh`、移行後の通常更新では`deploy/upgrade.sh`を使います。通常更新は停止前の検証、保守開始、配布物の入れ替え、元の稼働状態への復帰をまとめて行います。VPS上のGitチェックアウトや`git pull`は不要です。設置処理は設定を上書きしません。通常更新でも、有効化設定と元から停止中だったサービスは維持します。設置途中の通常エラーでは管理プログラム・サービス定義を戻し、保守状態を残します。

本番に常設する管理プログラムは`/usr/local/bin/bdsctl`だけです。現在のビルド情報は`/usr/local/share/bdsctl/BUILD.json`へ配置します。旧実装や過去の測定記録をソースディレクトリに保存せず、履歴はGit、検査結果と配布物は対応するCI・Releaseで確認します。

常駐する2つのRustプロセスの目標は、合計PSS20MiB以下・待機時CPU0.1%以下です。BDS本体・Caddy・実行中のバックアップは別に測定します。

初回切り替えと通常更新は[移行・配布手順](docs/migration.md)、検査の内容は[検証方法](docs/testing.md)、構造と失敗時の扱いは[設計](docs/design.md)を参照してください。
