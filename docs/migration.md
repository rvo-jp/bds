# 本番への移行

本番にGitリポジトリやRustソースは配置しません。開発PCでコミット・pushし、CI成功後のバイナリをVPSへ配布します。インストーラーはSSHの標準入力から実行するため、VPSに設置用ソースを残す必要もありません。

## 1. 停止前に配布物を準備する

開発PCで実行します。GitHub CLIでリポジトリへアクセスでき、SSH接続できることが前提です。以下のIPアドレス・ドメイン・SSH鍵のパスは例です。実際の環境へ置き換えてください。

```sh
export BDS_SSH_KEY="$HOME/.ssh/bds_ed25519"
commit=$(git rev-parse HEAD)
deploy/stage.sh root@192.0.2.10 "$commit"
```

指定コミットの成功したCIから配布物を取得し、VPSの専用一時ディレクトリへ転送します。チェックサムと、バイナリに埋め込まれたコミットIDを検証します。この段階ではサービスも設定も変更しません。表示された配布先とコミットIDを次の手順で使います。

必要なVPS側のソフトウェアは、systemd、cgroup v2、rsync 3.x、zstd、Polkit、BDSが要求する共有ライブラリです。既存のCaddyを利用します。Rust、Cargo、Git、GitHub TokenはVPSに不要です。

### Minecraft本体と旧パックの差分を停止前に検証する

`/etc/bds/config.toml`をroot所有・644で用意します。配布物の`bds.toml.example`、または開発PCのGit対象外の`bds.toml`を使い、実際の設定に合わせます。秘密情報は公開設定に入れません。停止前に、手順1で表示された配布先のバイナリで準備を実行します。

```sh
sudo /var/tmp/bdsctl-deploy-XXXXXXXX/bdsctl-linux-x86_64/bdsctl prepare-legacy /opt/bds/bedrock-server 1.26.23.1
```

末尾は追加ファイルの由来を確認するための旧公式版です。移行元の`.installed-version`と同じ本体、および参照する旧公式版を取得し、公式パックを照合します。現行版のファイルの欠落・変更、旧公式版と内容が一致しない追加ファイルは拒否します。同じパス・同じSHA-256で一致する旧公式ファイルだけを、`/var/lib/bds/state/legacy-plan.json`へ記録します。ワールドのコピー・変更、ユーザー作成、サービスの停止・設置は行いません。この段階で失敗した場合は稼働を続けたまま原因を解決します。

記録は移行元のパス・版・取得済み本体に結び付けます。停止後に同じファイル一覧と内容を再照合し、差分が変わっていれば取り込みを拒否します。参照版は追加ファイルの証明用であり、起動するBDSの版は変えません。準備の後に旧サーバーが自動更新された場合は、停止前に準備をやり直します。以降の取り込みでは取得済みの本体を使い、準備済みの本体が消失・変更していれば停止して再準備を要求します。

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

停止前に用意した`/etc/bds/config.toml`を使います。Webhook URLは既存の通知先を引き継ぎ、`/etc/bds/webhook`へ分離します。TokenやWebhookを公開設定へ入れないでください。

開発PCから、手順1で表示された配布先とコミットIDを指定します。

```sh
deploy/apply.sh root@192.0.2.10 /var/tmp/bdsctl-deploy-XXXXXXXX/bdsctl-linux-x86_64 "$commit"
```

停止状態・チェックサム・コミットIDを再確認し、バイナリとサービス定義を設置します。インストールだけでは起動も自動起動の有効化も行いません。設定ファイルは上書きしません。

VPS側で通知ファイルの権限を設定し、停止した旧BDSからデータを取り込みます。

```sh
sudo chown root:bds-notify /etc/bds/webhook
sudo chmod 640 /etc/bds/webhook
sudo bdsctl import-legacy /opt/bds/bedrock-server
```

取り込み先は、データが`/var/lib/bds/data`、Minecraft本体が`/opt/bedrock/releases`です。旧チェックアウトとは独立しています。既存データのある移行先へは上書きしません。停止した旧BDSディレクトリ、または`server/`と`metadata.json`を持つ全体バックアップのtar.gzが対象です。ワールドだけのtar.gzは直接取り込めません。

停止前に検証した記録を使い、旧公式版と一致した残存ファイルは新配置へ持ち込みません。現在の公式パックは現行版の配布物を使い、独立した独自パックとdevelopmentパック、ワールド・設定は保全します。元の旧サーバーディレクトリは編集・削除しません。記録がない場合は従来どおり現行公式版との完全一致を要求します。追加ファイルを無条件で無視する仕組みは持ちません。

リンク・特殊ファイル、現行公式ファイルの欠落・内容変更、未確認の追加ファイルは拒否します。取り込みが途中で失敗した場合は、停止状態で原因を直し、`sudo bdsctl recover`または同じ取り込みコマンドで再開します。既存データを削除してやり直さないでください。確定後は移行準備の記録も削除します。取り込み後にワールド名、server.properties、permissions、allowlist、アドオンを確認します。TCP/UDPの既存の許可設定を引き継いでください。

## 4. 実際の接続とバックアップを確認する

```sh
sudo systemctl start bds.service
sudo bdsctl status
sudo bdsctl console 'list'
```

Minecraftアプリから接続し、ワールド・アドオン・権限を確認します。`sudo bdsctl backup`で停止・バックアップ・起動の一巡を実施し、再度アプリから接続します。起動ログだけでは完了と判定しません。移行直後のバックアップは`protect`で保護します。

Caddyで既存の`https://minecraft.example.com/interactions`を`127.0.0.1:8765/interactions`へ転送します。配布物の`Caddyfile.example`を参考に既存設定へ統合し、設定を検証してから反映します。他のサイト設定を置き換えないでください。設定例の読み取り期限はサーバー全体に適用されるため、他サイトと共有する場合はその用途にも適切か確認します。受付はContent-Length付きのHTTP/1.x要求を使用し、本文64KiB・ヘッダー8KiB・同時4接続・接続受付から読み取り完了まで2秒の上限を持ちます。

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

検証後、旧ワールドやtar.gzバックアップの必要分をデータ保管先へ移します。旧形式のtar.gzを、新形式の`/var/lib/bds/backups`直下へ混在させないでください。例えば旧形式は`/var/backups/bds-tar`へまとめます。新形式のバックアップはVPS内で保存・管理します。必要時には`export`で対応するMinecraft本体も含めて手動書き出しできます。

旧チェックアウトを参照するサービスが残っていないことを確認し、旧サービス定義・旧Discordのsudoers設定・旧チェックアウト`/opt/bds`を撤去します。旧実装のコピーやGitチェックアウトを切り戻し用としてVPSに残しません。配布用一時ディレクトリも設置後に削除します。

## 通常の更新と切り戻し

初回移行を完了した後のbdsctl更新は、開発PCでcommit/push → CI成功 → `stage.sh`で転送・検証 → `upgrade.sh`で適用します。

```sh
commit=$(git rev-parse HEAD)
deploy/stage.sh root@192.0.2.10 "$commit"
deploy/upgrade.sh root@192.0.2.10 /var/tmp/bdsctl-deploy-XXXXXXXX/bdsctl-linux-x86_64 "$commit"
```

`upgrade.sh`は停止前に成果物・設定・必要コマンドを検証し、実行中の保守処理がないことを確認します。保守開始後にタイマー・Discord受付を停止し、管理プログラムとサービス定義を入れ替えます。正常終了時は元から稼働していたサービスだけを再開します。有効化設定は変更せず、手動停止中のMinecraftも起動しません。標準の保存先・サービス名専用で、初回移行や既に保守中の状態では使用しません。

設置途中の通常エラーでは、変更前のバイナリ・サービス定義・ビルド情報へ戻します。起動検証に失敗した場合は保守状態と作業記録を残し、勝手に先へ進みません。ワールドは管理ツールの配布更新では置き換えません。VPS上で`git pull`やコンパイルは行いません。日々のMinecraft本体更新は別の処理です。

### 配布の途中で失敗・切断した場合

`sudo bdsctl status`と`/var/lib/bds/state/deployment.txt`を確認します。記録には変更前の版、今回のコミット・配布先、タイマー・Discord受付の元の状態、最後に進んだ段階を残します。処理を再送する前に、前のSSH処理・インストーラーが終了していることを確認してください。

通常エラー時のファイル復旧と異なり、電源断・強制終了では設置ファイルが一部だけ変わっている可能性があります。その場合は保守を維持し、必要なサービスを停止して、検証済み配布物を`apply.sh`で再設置します。採用版へ戻す場合も、以前のGitHub Releaseから同じ手順で再設置します。`maintenance-end`に成功した後、記録にある元の稼働状態に従ってDiscord受付・タイマーを再開します。アプリから接続を確認してから`deployment.txt`を削除してください。記録の削除だけでは復旧になりません。

### 採用版の保管

本番に使う版は、検証済みコミットへ`v*`タグを付けてpushし、同じ検査に通った配布物をGitHub Releaseへ保存します。通常のCI成果物は90日保存なので、採用版の保管先にはしません。採用版はGitHub Releaseで管理し、本番のコミットIDは`/usr/local/share/bdsctl/BUILD.json`と照合します。

以前の採用版をReleaseから取得するときは、コミットとタグを指定します。バイナリに埋め込まれたコミットIDを再検証するため、別版の取り違えを拒否します。

```sh
deploy/stage.sh root@192.0.2.10 FULL_COMMIT vVERSION
```

管理ツールの切り戻し後はワールド・設定の互換性も確認し、必要なら対応するバックアップを復元します。旧版のソースや実行ファイルの複製をVPS内へ積み重ねません。使用済みの配布一時ディレクトリは作業後に削除します。
