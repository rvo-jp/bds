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
├── backups/
└── bedrock-server/
    ├── allowlist.json
    ├── bedrock_server
    ├── permissions.json
    ├── server.properties
    └── worlds/
```

`bedrock-server/` は自動作成されます。更新時も `allowlist.json`、`permissions.json`、`server.properties`、`worlds/`、`resource_packs/`、`behavior_packs/` は上書きしません。

## 初回セットアップ

Ubuntu LTS サーバーで依存パッケージを入れます。

```bash
sudo apt update
sudo apt install -y curl jq libarchive-tools unzip
```

スクリプトに実行権限を付けます。

```bash
chmod +x bds.sh
```

systemd service と timer をインストールします。

```bash
sudo ./bds.sh install-systemd
```

これで次の状態になります。

- `bds.service` が Bedrock Dedicated Server を 24 時間稼働
- `bds-update.timer` が 6 時間ごとに更新確認
- 新版がある場合はゲーム内へ警告し、5 分待ってからサーバーを停止、更新、再起動
- 更新処理は低 CPU/I/O 優先度で実行され、サーバー本体への影響を抑制
- 更新 service は警告待機とダウンロード時間を見込んで最大 30 分まで実行可能
- `bds-backup.timer` が 1 日 1 回、ワールドをバックアップ
- `bds-game8-post.timer` が 8 時間ごとに Game8 POST を実行可能

更新確認の間隔を変える場合は、初回インストール時に `CHECK_INTERVAL` を指定します。Minecraft 側へ性能を寄せるため、通常は 6 時間以上を推奨します。

```bash
sudo CHECK_INTERVAL=12h ./bds.sh install-systemd
```

更新前の待機時間を変える場合は `/etc/default/bds` に設定します。

```bash
sudo nano /etc/default/bds
```

```text
UPDATE_NOTICE_SECONDS=300
BACKUP_RETENTION_DAYS=14
GAME8_POST_ENABLED=0
GAME8_POST_NAME=backend-receive-check
GAME8_POST_BODY_FILE=/etc/bds/game8-post-body.txt
```

設定変更後は systemd を読み直します。

```bash
sudo systemctl daemon-reload
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

手動で Discord 通知を送ることもできます。

```bash
sudo ./bds.sh notify "メンテナンスを開始します。"
```

Webhook URL は秘匿情報なので、README や Git には入れず `/etc/default/bds` にだけ保存してください。

既に `install-systemd` 済みの環境で通知設定や通知対象が変わった場合は、systemd unit を再生成します。

```bash
sudo ./bds.sh install-systemd
```

## Game8 POST

`check_comment_post.sh` と同等の curl 処理を `bds.sh game8-post` として実行できます。systemd 登録後は `bds-game8-post.timer` が 8 時間ごとに起動しますが、デフォルトでは POST しません。利用する場合は `/etc/default/bds` で明示的に有効化します。

```text
GAME8_POST_ENABLED=1
GAME8_POST_NAME=backend-receive-check
GAME8_POST_BODY_FILE=/etc/bds/game8-post-body.txt
```

本文は複数行になることが多いため、`GAME8_POST_BODY_FILE` で別ファイルに置く方針です。

```bash
sudo mkdir -p /etc/bds
sudo nano /etc/bds/game8-post-body.txt
```

`GAME8_POST_BODY_FILE` が未指定の場合だけ、`GAME8_POST_BODY` または元の `check_comment_post.sh` と同じ `BODY` を使います。どれも未指定なら本文は空文字列です。`GAME8_POST_NAME` は元の `NAME` も利用できますが、どちらも未指定なら空文字列です。両方ある場合は `GAME8_POST_NAME` を優先します。

投稿先は `https://game8.jp/216448` と `/api/archive_comments` で固定しています。通常の Environment からは変更できないため、誤設定で投稿先が変わることはありません。

実行頻度を変える場合は、systemd timer を作り直します。初期値は `8h` です。

```bash
sudo GAME8_POST_INTERVAL=12h ./bds.sh install-systemd
```

成功や失敗は journal にだけ簡潔に記録します。レスポンス本文の詳細分析は行わず、ワールド内の `say` や Discord にも通知しません。

```bash
journalctl -u bds-game8-post.service -n 100 --no-pager
systemctl list-timers bds-game8-post.timer
```

手動実行する場合は次の通りです。

```bash
sudo GAME8_POST_ENABLED=1 ./bds.sh game8-post
```

## 操作

サーバー状態を確認します。

```bash
systemctl status bds.service
```

ログを確認します。

```bash
journalctl -u bds.service -f
```

手動で更新確認します。

```bash
sudo ./bds.sh auto-update
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
sudo systemctl stop bds.service
sudo systemctl start bds.service
sudo systemctl restart bds.service
```

## ワールド内コマンド送信

`bds.service` で起動している間は、次の FIFO に Bedrock server の stdin コマンドを送れます。

```text
/opt/bds/bedrock-server/.server.stdin
```

例:

```bash
printf 'say メンテナンスを5分後に開始します。\n' > /opt/bds/bedrock-server/.server.stdin
printf 'list\n' > /opt/bds/bedrock-server/.server.stdin
printf 'save hold\n' > /opt/bds/bedrock-server/.server.stdin
printf 'save resume\n' > /opt/bds/bedrock-server/.server.stdin
```

停止は `bds.sh` 経由でも送れます。

```bash
sudo ./bds.sh stop
```

FIFO はサーバー起動中だけ存在します。存在確認:

```bash
ls -l /opt/bds/bedrock-server/.server.stdin
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

`/etc/default/bds` に設定すると、systemd のバックアップ service に反映されます。`BACKUP_DIR` を指定する場合は絶対パスを使ってください。

```bash
sudo nano /etc/default/bds
```

```text
BACKUP_DIR=/home/ubuntu/bds/backups
BACKUP_RETENTION_DAYS=14
BACKUP_HOLD_SECONDS=10
BACKUP_MIN_FREE_MB=1024
```

バックアップ実行時刻を変える場合は、systemd timer を作り直します。

```bash
sudo BACKUP_ON_CALENDAR="*-*-* 03:30:00" ./bds.sh install-systemd
```

バックアップ timer を確認します。

```bash
systemctl list-timers bds-backup.timer
```

バックアップ先ディレクトリだけ作成されて中身が空の場合は、バックアップ service が途中で失敗しています。原因は journal に出ます。

```bash
journalctl -u bds-backup.service -n 100 --no-pager
```

よくある原因は、空き容量不足、`bedrock-server/worlds/` が存在しない、または `tar.gz` の作成や検証に失敗したケースです。

復元する場合は `restore` を使います。復元前にバックアップを検証し、`bds.service` が起動中なら停止します。既存の `worlds/` は削除せず、`worlds.pre-restore-YYYYMMDD-HHMMSS/` に退避します。復元前にサーバーが起動していた場合は、復元後に再起動します。

```bash
sudo ./bds.sh restore backups/bds-worlds-YYYYMMDD-HHMMSS.tar.gz
```

重要なサーバーでは、`backups/` を別ディスクや外部ストレージにもコピーすることを推奨します。

定期更新 timer と Game8 POST timer を確認します。

```bash
systemctl list-timers bds-update.timer bds-game8-post.timer
```

systemd 登録を削除します。

```bash
sudo ./bds.sh uninstall-systemd
```

## ポート

Bedrock の標準ポートは UDP `19132` です。必要に応じてファイアウォールを開けます。

```bash
sudo ufw allow 19132/udp
```

## 注意

`server.properties` の `server-name`、`gamemode`、`difficulty`、`max-players` などは `bedrock-server/server.properties` で変更します。変更後は再起動してください。

```bash
sudo systemctl restart bds.service
```
