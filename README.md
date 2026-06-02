# bds

Ubuntu LTS の systemd を使って Minecraft Bedrock Dedicated Server を常時起動し、定期的に最新版の有無を確認するための運用スクリプトです。

更新 URL は次の API から `serverBedrockLinux` を取得します。

```text
https://net.web.minecraft-services.net/api/v1.0/download/links
```

## 構成

```text
bds/
├── update.sh
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
chmod +x update.sh
```

systemd service と timer をインストールします。

```bash
sudo ./update.sh install-systemd
```

これで次の状態になります。

- `bds.service` が Bedrock Dedicated Server を 24 時間稼働
- `bds-update.timer` が 1 時間ごとに更新確認
- 新版がある場合はサーバーを停止、更新、再起動

更新確認の間隔を変える場合は、初回インストール時に `CHECK_INTERVAL` を指定します。

```bash
sudo CHECK_INTERVAL=30min ./update.sh install-systemd
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
sudo ./update.sh auto-update
```

サーバーを停止、起動、再起動します。

```bash
sudo systemctl stop bds.service
sudo systemctl start bds.service
sudo systemctl restart bds.service
```

定期更新 timer を確認します。

```bash
systemctl list-timers bds-update.timer
```

systemd 登録を削除します。

```bash
sudo ./update.sh uninstall-systemd
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
