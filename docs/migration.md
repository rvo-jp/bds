# 本番への移行

本番にGitリポジトリやRustソースは配置しません。開発PCでコミット・pushし、CI成功後のバイナリをVPSへ配布します。インストーラーはSSHの標準入力から実行するため、VPSに設置用ソースを残す必要もありません。

## 1. 停止前に配布物を準備する

開発PCで実行します。GitHub CLIでリポジトリへアクセスでき、SSH接続できることが前提です。

```sh
export BDS_SSH_KEY="$HOME/.ssh/rvo.pem"
commit=$(git rev-parse HEAD)
deploy/stage.sh root@162.43.15.136 "$commit"
```

指定コミットの成功したCIから配布物を取得し、VPSの専用一時ディレクトリへ転送します。チェックサムと、バイナリに埋め込まれたコミットIDを検証します。この段階ではサービスも設定も変更しません。表示された配布先とコミットIDを次の手順で使います。

必要なVPS側のソフトウェアは、systemd、cgroup v2、rsync 3.x、zstd、Polkit、BDSが要求する共有ライブラリです。既存のCaddyを利用します。Rust、Cargo、Git、GitHub TokenはVPSに不要です。

## 2. 旧サービスを停止する

実行中のバックアップ・更新・再起動処理が終わるのを待ち、旧タイマーとDiscord受付・入退室監視を止めます。

```sh
sudo systemctl disable --now bds-backup.timer bds-update.timer
sudo systemctl disable --now bds-discord-control.service bds-player-notify.service
```

ゲーム内に予告してから、旧BDSを正常停止します。

```sh
sudo systemctl stop bds.service
```

BDSのプロセスと子プロセスが残っていないことを確認します。停止が終わらなければ先へ進みません。旧スクリプトは、正常停止が終わるまで削除・置換しないでください。旧ソースの履歴はGitから取り出せます。秘密設定とワールドはGitの対象外なので、別途保全します。

## 3. 設定と新版を設置する

`/etc/bds/config.toml`をroot所有・644で用意します。配布物の`bds.toml.example`、または開発PCにあるGit対象外の`bds.toml`を使い、実際の設定に合わせます。Webhook URLは既存の通知先を引き継ぎ、`/etc/bds/webhook`へ分離します。TokenやWebhookを公開設定へ入れないでください。

開発PCから、手順1で表示された配布先とコミットIDを指定します。

```sh
deploy/apply.sh root@162.43.15.136 /var/tmp/bdsctl-deploy-XXXXXXXX/bdsctl-linux-x86_64 "$commit"
```

停止状態・チェックサム・コミットIDを再確認し、バイナリとサービス定義を設置します。インストールだけでは起動も自動起動の有効化も行いません。設定ファイルは上書きしません。

VPS側で通知ファイルの権限を設定し、停止した旧BDSからデータを取り込みます。

```sh
sudo chown root:bds-notify /etc/bds/webhook
sudo chmod 640 /etc/bds/webhook
sudo bdsctl import-legacy /opt/bds/bedrock-server
```

取り込み先は、データが`/var/lib/bds/data`、Minecraft本体が`/opt/bedrock/releases`です。旧チェックアウトとは独立しています。既存データのある移行先へは上書きしません。停止した旧BDSディレクトリ、または`server/`と`metadata.json`を持つ全体バックアップのtar.gzが対象です。ワールドだけのtar.gzは直接取り込めません。

`.installed-version`と同じBDSを公式配布から取得します。取得できない場合、別版へ勝手に更新せず停止します。リンク・特殊ファイル・直接編集された公式パックも拒否します。独自パックはデータ側へ分離します。取り込みが途中で失敗した場合は、停止状態で原因を直し、`sudo bdsctl recover`または同じ取り込みコマンドで再開します。既存データを削除してやり直さないでください。取り込み後にワールド名、server.properties、permissions、allowlist、アドオンを確認します。TCP/UDPの既存の許可設定を引き継いでください。

## 4. 実際の接続とバックアップを確認する

```sh
sudo systemctl start bds.service
sudo bdsctl status
sudo bdsctl console 'list'
```

Minecraftアプリから接続し、ワールド・アドオン・権限を確認します。`sudo bdsctl backup`で停止・バックアップ・起動の一巡を実施し、再度アプリから接続します。起動ログだけでは完了と判定しません。移行直後のバックアップは`protect`で保護します。

Caddyで既存の`https://smp.rvo.jp/interactions`を`127.0.0.1:8765/interactions`へ転送します。配布物の`Caddyfile.example`を参考に既存設定へ統合し、設定を検証してから反映します。他のサイト設定を置き換えないでください。設定例の読み取り期限はサーバー全体に適用されるため、他サイトと共有する場合はその用途にも適切か確認します。受付はContent-Length付きのHTTP/1.x要求を使用し、本文64KiB・ヘッダー8KiB・同時4接続・接続受付から読み取り完了まで2秒の上限を持ちます。

```sh
sudo systemctl start bds-discord.service
```

許可ユーザーと許可ロールで確認画面・キャンセルを確認し、許可されていない人は実行できないことを確認します。実際の再起動試験は停止可能な時間帯に行います。入退室通知も確認します。コマンドの再登録が必要な場合だけ、開発PCのドキュメントにある登録手順を使います。

## 5. 自動運転を有効にし、旧配置を撤去する

```sh
sudo systemctl enable bds.service bds-discord.service
sudo systemctl enable --now bds-daily.timer bds-update.timer
sudo systemctl list-timers 'bds-*'
```

旧タイマーが無効で、新しい日次処理が日本時間04:30、更新が毎時00分＋最大5分の遅延になっていることを確認します。更新タイマーは新バージョンの取得だけでなく、予告・停止・バックアップ・適用・起動確認まで行います。翌朝の処理後もアプリから接続できることを確認してください。

検証後、旧ワールドやtar.gzバックアップの必要分をデータ保管先へ移します。旧形式のtar.gzを、新形式の`/var/lib/bds/backups`直下へ混在させないでください。例えば旧形式は`/var/backups/bds-tar`へまとめます。新形式のバックアップは復旧に必要なMinecraft本体と合わせて保管します。

旧チェックアウトを参照するサービスが残っていないことを確認し、旧サービス定義・旧Discordのsudoers設定・旧チェックアウト`/opt/bds`を撤去します。旧実装のコピーやGitチェックアウトを切り戻し用としてVPSに残しません。配布用一時ディレクトリも設置後に削除します。

## 通常の更新と切り戻し

bdsctl自身の更新も、開発PCでcommit/push → CI成功 → `stage.sh`で転送・検証 → タイマー・Discord受付・Minecraft停止 → `apply.sh`で置換 → 起動・接続確認の順です。VPS上で`git pull`やコンパイルは行いません。日々のMinecraft本体更新はbdsctlが別に管理します。

本番に使う版は、検証済みコミットへ`v*`タグを付けてpushすると、同じ検査に通った配布物をGitHub Releaseへ保存できます。通常のCI成果物は90日保存です。タグの作成・公開は、採用する版を決めた時点で行います。

bdsctlの切り戻しには、以前の採用版の検証済み配布物を再設置します。ワールド・設定の互換性も確認し、必要なら対応するバックアップを復元します。旧版のソースや実行ファイルの複製をVPS内へ積み重ねて管理しません。
