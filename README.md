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
CURL_RETRY=5
CURL_RETRY_DELAY=5
CURL_SPEED_TIME=60
CURL_SPEED_LIMIT=1024
CURL_MAX_TIME=1800
```

設定変更後は systemd を読み直します。

```bash
sudo systemctl daemon-reload
```

`curl: (92) HTTP/2 stream ... INTERNAL_ERROR` のような CDN 側の一時エラーが出た場合でも、ダウンロードは自動 retry し、失敗時は HTTP/1.1、さらに IPv4 + HTTP/1.1 で再試行します。転送が止まったままにならないよう、低速状態が続く場合も中断して次の方式へ進みます。必要に応じて `/etc/default/bds` で retry 回数やタイムアウトを調整できます。

```text
CURL_RETRY=5
CURL_RETRY_DELAY=5
CURL_SPEED_TIME=60
CURL_SPEED_LIMIT=1024
CURL_MAX_TIME=1800
```

## Discord 通知

Discord Webhook URL を設定すると、通常のサーバー起動・停止、アップデート、バックアップ、復元のタイミングで通知します。

```text
DISCORD_WEBHOOK_URL=https://discord.com/api/webhooks/...
```

Webhook URL は秘匿情報なので、README や Git には入れず `/etc/default/bds` にだけ保存してください。

既に `install-systemd` 済みの環境で通知設定や通知対象が変わった場合は、systemd unit を再生成します。

```bash
sudo ./bds.sh install-systemd
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

復元する場合は `restore` を使います。復元前にバックアップを検証し、`bds.service` が起動中なら停止します。既存の `worlds/` は削除せず、`worlds.pre-restore-YYYYMMDD-HHMMSS/` に退避します。復元前にサーバーが起動していた場合は、復元後に再起動します。

```bash
sudo ./bds.sh restore backups/bds-worlds-YYYYMMDD-HHMMSS.tar.gz
```

重要なサーバーでは、`backups/` を別ディスクや外部ストレージにもコピーすることを推奨します。

定期更新 timer を確認します。

```bash
systemctl list-timers bds-update.timer
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
