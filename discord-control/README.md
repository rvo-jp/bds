# Discordからの再起動

指定サーバーの許可メンバーだけが、`/restart`でMinecraftを、`/reboot`でVPSのOSを再起動できます。両コマンドの許可リストは共通です。Discordへの常時接続は使わず、署名付きHTTP Interactionを受信します。

## 動作

1. Application ID・DiscordサーバーID・ユーザーIDまたはロールIDを確認します。DMや別のサーバーからは使えません。
2. 本人にだけ確認ボタンを表示します。期限は60秒です。確認時にも権限を再確認します。
3. 要求をディスクへ保存してから受け付けます。処理中の重複要求を拒否し、受付後は既定で300秒間、両コマンドの次の要求を拒否します。失敗した要求にも待機時間が適用されます。
4. 専用unitが`bds.sh control-restart`または`control-reboot`を実行します。更新・バックアップなどと競合した場合は中止し、後から勝手に実行するキューへは入れません。
5. root側の専用unitが開始時と終了時に実行ID・OS起動ID・終了結果を記録します。systemdが終了済みunitの履歴を破棄した後も、この記録で今回の実行結果を確認し、本人の確認メッセージへ反映します。実行者のユーザーID・要求ID・結果はjournalへ記録します。

受付の再起動やHTTP要求の再送で、OS再起動を繰り返し実行しません。受付中断後は保存済みの実行結果だけを確認し、実行を確認できなければその旨を返します。OSの起動IDが変わった場合は、BDSの監督付きサービスと、今回の起動IDに一致する新しい正常状態が15秒間継続することを確認します。

`/restart`の停止中に異常・中断が起きた場合、元が稼働中で、プロセス・FIFOが残らず完全停止を確認できたときだけ復旧起動を1回試みます。通常の起動を既に試して失敗した場合には繰り返しません。復旧できても、元の要求は失敗として通知します。

Discordの応答用トークンは有効期限が短いため、停止が長引いた場合は確認メッセージを更新できません。その場合はjournalへ記録します。OSが停止したまま戻らない状態を検知・通知するには、別ホストの監視が必要です。このサービス自体では検知できません。

## 配置と権限

対象はLinux/systemd、配置先は`/opt/bds`です。BDS用の`configure`を実行済みで、`bds.service`が自動起動することが前提です。

```text
/opt/bds/bds.sh                         root所有・実行可能
/opt/bds/bds.conf                       BDSの運用設定
/opt/bds/discord-control/               このディレクトリ
/etc/bds-discord-control.json           受付設定（root:bds-discord-control、0640）
/var/lib/bds-discord-control/            受付記録（専用ユーザー、0700）
/run/bds-control-restart/result.json    Minecraft再起動の結果（root所有、0644）
/run/bds-control-reboot/result.json     OS再起動要求の結果（root所有、0644）
```

`/run`の結果記録にはトークンを含めず、受付ユーザーには読み取りだけを許可します。受信サービスの再起動では残り、OS再起動では消えます。OS再起動後は、`/var/lib`に保存した受付記録のOS起動IDとBDSの復帰状態で結果を確認します。

受付は`bds-discord-control`ユーザーで動きます。sudoで許可するのは、2つの固定されたsystemd unitの起動だけです。rootが実行するスクリプト・設定・unitを受付ユーザーが編集できる状態にしないでください。

受付のメモリ上限は128 MiB、CPU上限は1コアの20%です。root側の停止・起動処理は別のunitで動きます。上限は平常時の使用量を表すものではありません。

## 設定

Ubuntuの依存ライブラリを用意します。

```bash
sudo apt install python3-aiohttp python3-cryptography sudo
sudo install -o root -g root -m 0600 /opt/bds/discord-control/config.example.json /etc/bds-discord-control.json
sudoedit /etc/bds-discord-control.json
```

`application_id`と`public_key`はDiscord Developer PortalのGeneral Informationから取得します。`guild_id`はDiscordサーバーIDです。`allowed_user_ids`と`allowed_role_ids`は、少なくとも一方に許可対象のIDを入れてください。両方を指定すると、いずれかに該当するメンバーを許可します。

IDはすべてJSONの文字列で指定します。`bedrock_port`は実際のBDSのIPv4ポートと一致させてください。`port`はループバックのHTTP待受ポート、`cooldown_seconds`は受付から次の要求までの秒数です。ロール方式では、そのロールを付与できる人も再起動権限を配布できることになります。

ゲーム内予告の待機時間は`bds.conf`の`CONTROL_NOTICE_SECONDS`（既定60）で設定します。10秒を超える場合は10秒前にも予告します。`0`は予告待機を省略します。

```bash
sudo bash /opt/bds/discord-control/install.sh
```

インストーラーは受付用ユーザー・sudoers・3つのunitを作り、受付を起動します。BDSの停止・再起動、HTTPS設定、ファイアウォール変更、Discordコマンド登録は行いません。ソースと設定はroot所有・グループやその他ユーザーの書き込み不可にしてください。

## HTTPS

DiscordのInteractions Endpointには、有効な証明書で公開したHTTPS URLが必要です。受付は`127.0.0.1:8765`だけで待受けるため、Caddyなどのリバースプロキシを使います。8765番ポートは外部へ開放しません。

`Caddyfile.example`は`smp.rvo.jp`の例です。DNSのAレコードをVPSへ向け、HTTP/HTTPSの80・443番TCPポートをOSとVPS管理側のファイアウォールで許可します。既存のWeb設定がある場合は上書きせず統合します。Caddyの通常の自動HTTPS設定では証明書の取得・更新も行われます。[Caddy公式仕様](https://caddyserver.com/docs/automatic-https)

```text
https://smp.rvo.jp/interactions
```

このURLをDeveloper PortalのInteractions Endpoint URLへ設定します。保存時の署名付きPINGへ応答できる状態が必要です。ブラウザで開いた場合や署名のないPOSTには成功応答を返しません。

## Discordコマンドの登録

アプリを対象のDiscordサーバーへインストールし、`applications.commands`スコープを許可してください。コマンド登録にはアプリのBot Tokenを使います。受付サービスの常駐処理にはBot Tokenを保存・使用しません。

```bash
sudo python3 /opt/bds/discord-control/register.py
```

Tokenは非表示の入力欄で入力します。引数・設定ファイル・チャットには貼り付けないでください。この登録処理は`/restart`と`/reboot`を追加または更新し、その他のコマンドは削除しません。

登録直後のコマンドは一般メンバーに許可されていません。Discordのサーバー設定 → 連携サービス → 対象アプリで、両コマンドを許可するユーザーまたはロールを設定してください。Discord側の設定と受付設定の両方を通った場合だけ実行できます。Discordの管理者でも、受付の許可リストに含まれていなければ実行できません。

## 運用と停止

```bash
sudo systemctl status bds-discord-control.service
sudo journalctl -u bds-discord-control.service -u bds-control-restart.service -u bds-control-reboot.service
```

設定を変更したら、実行中の再起動要求がないことを確認して受付サービスを再起動します。

```bash
sudo systemctl restart bds-discord-control.service
```

受付を停止するには次を実行します。既に受け付けて専用unitが実行中の処理は、受付を停止しただけでは取り消しません。

```bash
sudo systemctl disable --now bds-discord-control.service
```

完全に削除する場合は、実行中の要求がない状態で、3つのunit、`/etc/sudoers.d/bds-discord-control`、HTTPSの受付設定、Discordのコマンド登録も管理者が削除してください。保存済み要求には短時間有効な応答トークンを含むため、`/var/lib/bds-discord-control`の内容を公開しないでください。処理完了時にはトークンを削除します。

## テスト

```bash
python3 -m venv .venv
.venv/bin/python -m pip install -r discord-control/requirements.txt
.venv/bin/python -m unittest discover -s tests -v
```

実際の署名・HTTP受信を検証します。OS再起動、systemd制御、Discordへの送信は模擬処理を使います。

Linux/systemd上では、次の追加検証で実際のunit終了結果と履歴破棄後の判定を確認できます。一時unitは`true`/`false`だけを実行し、Minecraft・OSの再起動やDiscord送信は行いません。検証ファイルは終了時に削除します。

```bash
sudo python3 tests/systemd_control_smoke.py
```
