# 停止してから移行する

この手順を実行するまでは、新版を本番サービスとして起動しません。切り替え前に、実際のBDSとアドオンを使った停止・バックアップ・再起動・接続確認の時間を確保してください。

## 1. ソースと成果物をそろえる

ローカルで変更を確認し、Gitへコミット・送信します。同じコミットのCIを通し、`bdsctl-linux-x86_64-コミットID`の成果物を取得します。VPS側の`git pull`は、次の手順で旧BDSを正常停止した後に行います。起動・停止に旧スクリプトを使うサービスがあるため、稼働中に旧ファイルを置き換えないでください。

必要なソフトウェアはrsync 3.x、zstd、Polkit、CaddyとBDSが要求する共有ライブラリです。`/var/lib/bds`と`/opt/bds/releases`に十分な空きを確保してください。初回移行は既存ワールドの独立コピー、本体の展開、初回バックアップが必要です。旧バックアップや切り戻し用データを先に削除しないでください。

`bds.toml.example`を`/etc/bds/config.toml`へコピーし、root所有・644にします。旧`bds.conf`の設定は自動で読みません。通知先は現在のbds.shのWebhookと同じURLを`/etc/bds/webhook`に移します。TokenやWebhookをGitへ入れないでください。

## 2. 旧サービスを停止する

現在のバックアップ・更新・入退室通知・Discord操作のサービスとタイマーを一覧で確認します。移行中に旧タイマーがサーバーを起動しないよう、対象タイマーを先に停止・無効化し、Discord受付も停止します。Game8など別目的のタイマーは対象外です。

ゲーム内に停止を予告してから、旧BDSを正常停止します。停止完了とBDSプロセスが残っていないことを確認してください。停止できない状態で移行を続けないでください。旧サービス定義・設定・BDSディレクトリを切り戻し用として保管します。

インストール先と旧チェックアウトが同じ`/opt/bds`でも、新しいデータは`/var/lib/bds`に作ります。旧`/opt/bds/bedrock-server`は、新しいデータを検証するまで残します。

## 3. 新版を設置・取り込む

旧BDSと旧タイマーの停止を確認してから、VPS側で`git pull`し、CI成果物と同じコミットに合わせます。

```sh
sudo deploy/install.sh /取得したCI成果物のディレクトリ
sudo chown root:bds-notify /etc/bds/webhook
sudo chmod 640 /etc/bds/webhook
sudo bdsctl import-legacy /opt/bds/bedrock-server
```

`import-legacy`は既存データのない移行先専用です。停止した旧BDSディレクトリ、または`server/`と`metadata.json`を持つ全体バックアップのtar.gzを取り込めます。旧ディレクトリの`.installed-version`と同じ版を公式配布元から取得します。その版が配布されていない場合は停止し、別版へ勝手に更新しません。ワールドだけのtar.gzは直接の取り込み対象外です。

旧データのリンクや特殊ファイル、直接変更された公式パックを検出した場合も移行を止めます。独自パックはデータ側へ分離されます。旧パックと公式パックの名前が同じ場合は、別名の独自パックへ整理してください。

取り込み後、`/var/lib/bds/data`の`server.properties`、権限、allowlist、ワールド名、使用アドオンを確認します。TCP/UDP 19132の既存の許可設定と、NetherNetの設定を引き継いでください。

## 4. 起動と実際の接続を確認する

```sh
sudo systemctl start bds.service
sudo bdsctl status
sudo bdsctl console 'list'
```

Minecraftアプリから接続し、ワールド、アドオン、権限、入退室通知を確認します。次に`sudo bdsctl backup`で停止・バックアップ・起動の一巡を実施し、再度アプリから接続します。起動ログが出るだけでは移行完了としません。

Caddyの既存設定に`deploy/Caddyfile.example`相当を統合し、検証してから反映します。既存の別サイト・証明書設定を上書きしないでください。旧Discord受付が同じ8765番ポートを使用していないことを確認し、新受付を起動します。

```sh
sudo systemctl start bds-discord.service
```

DiscordのInteractions Endpoint URLは引き続き`https://smp.rvo.jp/interactions`です。必要ならコマンドを再登録します。許可ユーザー・許可ロールの各アカウントで確認画面を出し、キャンセルができること、許可されていないアカウントは実行できないことを確認します。OS再起動の実行試験は、別途停止できる時間帯に実施します。

## 5. 自動起動を有効にする

接続・バックアップ・通知を確認した後に有効化します。

```sh
sudo systemctl enable bds.service bds-discord.service
sudo systemctl enable --now bds-daily.timer bds-update-check.timer
sudo systemctl list-timers 'bds-*'
```

翌朝の日次処理後もアプリから接続できることを確認してください。新旧のバックアップ・更新タイマーを同時に有効化しないでください。

## 切り戻し

新版のタイマーとDiscord受付を止め、新版BDSを正常停止します。新版起動後にワールドが進んでいれば、旧版のワールドへ単純に戻すと進行を失います。切り戻し元の版とデータの対応を確認してください。移行直後で新版に変更がなければ、保管した旧サービス定義・旧設定・旧BDSディレクトリを戻し、旧サービスを起動します。新旧を同時に起動しないでください。

