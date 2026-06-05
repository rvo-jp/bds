# bds

Ubuntu LTS の systemd を使って Minecraft Bedrock Dedicated Server を常時起動し、定期的に最新版の有無を確認するための運用スクリプトです。

更新 URL は Minecraft の download links API から `serverBedrockLinux` を取得します。

```text
https://net-secondary.web.minecraft-services.net/api/v1.0/download/links
https://net.web.minecraft-services.net/api/v1.0/download/links
```

`net-secondary` を優先して使い、失敗した場合は `net` に fallback します。

## 構成

```text
bds/
├── bds.sh
├── bds.conf
├── bds.conf.example
├── backups/
└── bedrock-server/
    ├── allowlist.json
    ├── bedrock_server
    ├── permissions.json
    ├── server.properties
    └── worlds/
```

`bedrock-server/` は自動作成されます。更新時も `allowlist.json`、`permissions.json`、`server.properties`、`worlds/`、`resource_packs/`、`behavior_packs/` は上書きしません。`bds.conf` はローカル設定ファイルのため Git 管理しません。

## 初回セットアップ

Ubuntu LTS サーバーで依存パッケージを入れます。

```bash
sudo apt update
sudo apt install -y curl jq libarchive-tools unzip
```

スクリプトに実行権限を付け、設定ファイルを作ります。

```bash
chmod +x bds.sh
cp bds.conf.example bds.conf
nano bds.conf
```

BDS 本体、systemd service、timer を準備して起動します。

```bash
sudo ./bds.sh start
```

これで次の状態になります。

- `bds.service` が Bedrock Dedicated Server を 24 時間稼働
- `bds-update.timer` が 6 時間ごとに更新確認
- 新版がある場合はゲーム内へ警告し、5 分待ってからサーバーを停止、更新、再起動
- 更新処理は低 CPU/I/O 優先度で実行され、サーバー本体への影響を抑制
- 更新 service は警告待機とダウンロード時間を見込んで最大 30 分まで実行可能
- `bds-backup.timer` が 1 日 1 回、ワールドをバックアップ
- `bds-game8-post.timer` が 8 時間ごとに Game8 POST を実行可能

設定はプロジェクト配下の `bds.conf` に書きます。`bds.conf` は shell として読み込まれるため、通常の変数代入や heredoc が使えます。

```bash
nano bds.conf
```

更新確認やバックアップ、Game8 POST の timer 間隔を変えた場合は、`restart` で systemd unit を再生成します。Minecraft 側へ性能を寄せるため、更新確認は通常 6 時間以上を推奨します。

```bash
sudo ./bds.sh restart
```

Minecraft 公式の ZIP ダウンロードでは、ブラウザ相当の `User-Agent` と `Referer` ヘッダーを付けて取得します。

## Discord 通知

Discord Webhook URL を設定すると、通常のサーバー起動・停止、アップデート、バックアップ、復元のタイミングで日本語通知します。

```text
DISCORD_WEBHOOK_URL=https://discord.com/api/webhooks/...
```

通知される主な内容は次の通りです。

- サーバー起動、停止
- アップデート検知、開始、完了
- バックアップ開始、完了、失敗、空き容量不足
- バックアップ復元の開始、完了、復元後の再起動


Webhook URL は秘匿情報なので、README や Git には入れず `bds.conf` にだけ保存してください。`bds.conf` は `.gitignore` 済みです。

通知設定や通知対象を変えた場合は、`restart` で systemd unit を再生成して反映します。

```bash
sudo ./bds.sh restart
```

## Game8 POST

Game8 への POST は `bds-game8-post.timer` で定期実行できますが、デフォルトでは POST しません。利用する場合は `bds.conf` で明示的に有効化します。

```bash
GAME8_POST_ENABLED=1
GAME8_POST_NAME=backend-receive-check
GAME8_POST_BODY=$(cat <<'EOF'
1行目
2行目
3行目
EOF
)
```

`GAME8_POST_BODY` が未指定なら本文は空文字列です。`GAME8_POST_NAME` が未指定なら投稿者名は空文字列です。

投稿先は `https://game8.jp/216448` と `/api/archive_comments` で固定しています。通常の設定項目からは変更できないため、誤設定で投稿先が変わることはありません。

実行頻度を変える場合は、`bds.conf` の `GAME8_POST_INTERVAL` を変更して systemd timer を作り直します。初期値は `8h` です。

```bash
sudo ./bds.sh restart
```

成功や失敗は journal にだけ簡潔に記録します。レスポンス本文の詳細分析は行わず、ワールド内の `say` や Discord にも通知しません。

```bash
./bds.sh logs game8
./bds.sh status game8-timer
```


## 操作

サーバー状態を確認します。

```bash
./bds.sh status
```

ログを確認します。

```bash
./bds.sh logs --follow
```

手動でワールドをバックアップします。

```bash
sudo ./bds.sh backup
```

バックアップから復元します。

```bash
sudo ./bds.sh restore backups/bds-worlds-YYYYMMDD-HHMMSS.tar.gz
```

サーバーを停止、起動、再起動します。

```bash
sudo ./bds.sh stop
sudo ./bds.sh start
sudo ./bds.sh restart
```

## ワールド内コマンド実行

`bds.service` で起動している間は、`command` で Bedrock server の stdin にコマンドを送れます。

```bash
./bds.sh command say メンテナンスを5分後に開始します。
./bds.sh command list
./bds.sh command save hold
./bds.sh command save resume
```

bedrock_server の stdin に `stop` を直接送りたい場合は `send-stop` を使います。通常の停止は `sudo ./bds.sh stop` を使ってください。

```bash
sudo ./bds.sh send-stop
```

## メンテナンス方針

定期メンテナンスはした方が安全です。ただし頻繁な再起動より、ワールド保護を優先します。

推奨は次の運用です。

- ワールドバックアップ: 1 日 1 回、プレイヤーが少ない時間帯
- サーバー再起動: 週 1 回程度、またはメモリ使用量や挙動が不安定なとき
- Bedrock 更新: このスクリプトで検知し、ゲーム内警告後に実行
- Discord 通知: 複数人で遊ぶサーバーなら設定推奨

メンテナンスでサーバーを閉じる場合も、Discord とゲーム内 `say` の両方で事前通知するのが快適です。

## バックアップ

バックアップはデフォルトで毎日 `04:30` に実行され、プロジェクト配下の `backups/` に保存されます。サーバーが起動している場合は `save hold` でワールド保存を一時固定し、`worlds/` を `tar.gz` にまとめたあと `save resume` で通常保存に戻します。

バックアップ前には空き容量を確認し、作成後には `tar.gz` を読み取れるか検証します。空き容量不足や検証失敗が起きた場合は成功扱いにせず、Discord Webhook URL が設定されていれば通知します。

デフォルト設定は次の通りです。

```text
BACKUP_RETENTION_DAYS=14
BACKUP_HOLD_SECONDS=10
BACKUP_MIN_FREE_MB=1024
```

`bds.conf` に設定すると反映されます。`BACKUP_DIR` を指定する場合は絶対パスを使ってください。

```bash
nano bds.conf
```

```bash
BACKUP_DIR=/home/ubuntu/bds/backups
BACKUP_RETENTION_DAYS=14
BACKUP_HOLD_SECONDS=10
BACKUP_MIN_FREE_MB=1024
```

バックアップ実行時刻を変える場合は、`bds.conf` の `BACKUP_ON_CALENDAR` を変更して systemd timer を作り直します。

```bash
sudo ./bds.sh restart
```

バックアップ timer を確認します。

```bash
./bds.sh status backup-timer
```

バックアップ先ディレクトリだけ作成されて中身が空の場合は、バックアップ service が途中で失敗しています。原因は journal に出ます。

```bash
./bds.sh logs backup
```

よくある原因は、空き容量不足、`bedrock-server/worlds/` が存在しない、または `tar.gz` の作成や検証に失敗したケースです。

復元する場合は `restore` を使います。復元前にバックアップを検証し、`bds.service` が起動中なら停止します。既存の `worlds/` は削除せず、`worlds.pre-restore-YYYYMMDD-HHMMSS/` に退避します。復元前にサーバーが起動していた場合は、復元後に再起動します。

```bash
sudo ./bds.sh restore backups/bds-worlds-YYYYMMDD-HHMMSS.tar.gz
```

重要なサーバーでは、`backups/` を別ディスクや外部ストレージにもコピーすることを推奨します。

定期更新 timer と Game8 POST timer を確認します。

```bash
./bds.sh timers
```

systemd 登録を削除します。

```bash
sudo ./bds.sh uninstall
```

## ポート

Bedrock の標準ポートは UDP `19132` です。必要に応じてファイアウォールを開けます。

```bash
sudo ufw allow 19132/udp
```

## 注意

`server.properties` の `gamemode`、`difficulty`、`max-players` などは `bedrock-server/server.properties` で変更します。変更後は再起動してください。

`server-name` は `bds.conf` の `SERVER_NAME_FORMAT` で管理できます。`%v` はプレイヤー向け表記、`%V` は BDS のフル表記に置き換わります。空文字列にすると自動変更しません。

```bash
SERVER_NAME_FORMAT="RvoSMP %v"
```

```bash
sudo ./bds.sh restart
```
