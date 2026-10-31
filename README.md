# bds

Ubuntu の systemd で Minecraft Bedrock Dedicated Server（BDS）を管理するスクリプトです。起動・停止、定期更新、停止中のバックアップ、復元、Discordへの参加・退出通知を扱います。

## 配置と初回導入

```text
/opt/bds/
├── bds.sh
├── bds.conf
├── bds.conf.example
├── backups/
└── bedrock-server/
    ├── bedrock_server
    ├── server.properties
    ├── allowlist.json
    ├── permissions.json
    ├── behavior_packs/
    ├── resource_packs/
    └── worlds/
```

Linux・systemd環境で実行します。Python 3はBDSの監督処理として常駐し、アーカイブ・パスの検証や容量集計にも使用します。Pythonの追加パッケージは不要です。BDS本体の実行に必要な共有ライブラリもサーバー側に用意してください。

```bash
sudo apt update
sudo apt install -y curl jq python3 util-linux coreutils tar
cd /opt/bds
chmod +x bds.sh
cp bds.conf.example bds.conf
nano bds.conf
sudo ./bds.sh install
sudo ./bds.sh configure
sudo ./bds.sh start
```

- `install`：最新版の本体を新規配置します。実行ファイルが既にある場合は中止します。サービスの設定・起動は行いません。
- `configure`：serviceとtimerを生成・再読み込みします。本体のダウンロードやBDSの再起動は行いません。更新・バックアップのtimerは有効化して起動します。
- `start` / `restart`：サービスの起動・再起動だけを行います。本体を変更しません。
- `update`：最新版を確認し、必要な場合に本体を更新します。

初回のサービス実行ユーザーは`root`です。`SERVICE_USER`・`SERVICE_GROUP`を設定すると、初回の配置先の所有者とサービス実行ユーザーに使用します。既存のサービスがある場合、`configure`はそのUser・Groupを引き継ぎます。管理コマンドは以下の例のように`sudo`で実行します。

`bds.conf`はシェルとして読み込まれます。信頼できる管理者だけが編集できる状態にし、Webhook URLを含む設定をGitへ登録しないでください。BDSの実行ユーザーにも、この設定ファイルとスクリプトを読み取る権限が必要です。

## 操作

```bash
sudo ./bds.sh start
sudo ./bds.sh stop
sudo ./bds.sh restart
sudo ./bds.sh update
sudo ./bds.sh backup
sudo ./bds.sh restore backups/bds-server-20261001-043000-12345.tar.gz

./bds.sh status
./bds.sh logs --follow
./bds.sh timers
./bds.sh status update-timer
./bds.sh status backup-timer
./bds.sh status player-notify
./bds.sh logs backup --lines 100
./bds.sh logs player-notify --follow

sudo ./bds.sh command list
sudo ./bds.sh command say メンテナンスを開始します。
```

`status`は`server`・`update`・`backup`・`player-notify`・`update-timer`・`backup-timer`を指定できます。`logs`はサービス名の別名と`--follow` / `--lines N`に対応します。`auto-update`は`update`の別名です。

`status`（対象を省略、または`server`）はサービス状態に加えて、サーバー・バックアップ・退避データの使用容量、バックアップと退避データの件数、最新バックアップの名前・ファイル更新日時、保存先の空き容量を表示します。集計はこのコマンドを実行したときだけ行い、常駐監視は追加しません。

最新バックアップは正式名の`.tar.gz`の更新日時で判断します。`.partial`とシンボリックリンクは件数・最新ファイルの対象外です。アーカイブ内容の検証はこの表示では行いません。権限不足などで集計できない項目は不明として表示します。停止中でも容量情報を表示し、終了コードは`systemctl status`の値を維持します。退避データを自動削除する機能はありません。

`run`・`send-stop`・`notify`は主にsystemdから呼ばれます。通常の起動・停止には`start`・`stop`を使ってください。`run`を直接起動したり、メンテナンス中に別の手段でBDSを起動したりすると、排他制御の対象外になります。

## 自動実行と設定

`bds.conf`の既定の場所はスクリプトと同じディレクトリです。`BDS_CONFIG`で別の設定ファイルを指定できます。以下の`BASE_DIR`はスクリプトの配置先を表します。

| 設定 | 既定値 | 用途 |
|---|---|---|
| `BEDROCK_DIR` | `$BASE_DIR/bedrock-server` | 本体・設定・ワールドの配置先。管理用ディレクトリを含まない絶対パスの実ディレクトリ |
| `CHECK_INTERVAL` | `6h` | 定期更新確認の間隔 |
| `UPDATE_NOTICE_SECONDS` | `300` | 更新前の停止予告から待つ秒数 |
| `SERVER_START_TIMEOUT_SECONDS` | `120` | 1回の起動で受付・コンソール応答を待つ上限秒数。1以上 |
| `SERVER_STABLE_SECONDS` | `15` | 全起動で受付・コンソール応答の安定を確認する秒数 |
| `CONTROL_NOTICE_SECONDS` | `60` | Discord経由のMinecraft再起動・OS再起動の予告から待つ秒数 |
| `BACKUP_DIR` | `$BASE_DIR/backups` | バックアップ保存先。`BEDROCK_DIR`の外に指定 |
| `BACKUP_ON_CALENDAR` | `*-*-* 04:30:00` | 日次バックアップの時刻。サーバーのタイムゾーン基準 |
| `BACKUP_RETENTION_DAYS` | `14` | 成功後に古いバックアップを削除する際の保持日数 |
| `BACKUP_NOTICE_SECONDS` | `60` | バックアップ前の停止予告から待つ秒数 |
| `BACKUP_LOCK_WAIT_SECONDS` | `1800` | 他のメンテナンスの終了を待つ上限秒数 |
| `BACKUP_MIN_FREE_MB` | `1024` | コピー先に残す空き容量の目安（MiB単位） |
| `SERVER_NAME_FORMAT` | 空 | 更新・インストール時の`server-name`。`%v`は短縮表記、`%V`はBDSのフルバージョン。空なら変更しない |
| `DISCORD_WEBHOOK_URL` | 空 | 通知先。空ならDiscordへ送らない |
| `DISCORD_TIMEOUT_SECONDS` | `30` | 1通知あたりの通信・再試行・待機の合計上限秒数。1以上 |
| `PLAYER_NOTIFY_ENABLED` | `false` | 参加・退出通知の有効・無効 |
| `PLAYER_NOTIFY_STATE_DIR` | `$BASE_DIR/.player-notify` | ログの読み取り位置の保存先 |
| `SERVICE_USER` / `SERVICE_GROUP` | 初回は`root` / その主グループ | 初回のサービス実行ユーザー・所有者 |

秒数・保持日数・空き容量は整数で指定します。`SERVER_STABLE_SECONDS=0`は追加の安定確認を省略します。配置先の末尾の`/`は取り除いてから利用し、途中の`.`・`..`などを含む正規化されていないパスはメンテナンス前に拒否します。`BACKUP_LOCK_WAIT_SECONDS=0`は待機なし、`UPDATE_NOTICE_SECONDS=0`・`BACKUP_NOTICE_SECONDS=0`は予告待機なしです。バージョン`1.21.100.7`の短縮表記は`21.100`です。設定例では`SERVER_NAME_FORMAT="RvoSMP %v"`を指定しています。

timer・配置先などのサービス設定や、入退室通知の設定を変えたら反映します。

```bash
sudo ./bds.sh configure
```

`configure`は入退室通知を有効にした場合にその監視サービスを再起動し、無効にした場合には停止します。BDS本体には再起動をかけません。稼働中に`BEDROCK_DIR`を変更する場合は、先に停止してデータを移動してから設定を反映してください。

生成するunitは以下です。

| unit | 動作 |
|---|---|
| `bds.service` | BDSを起動。監督処理が起動・受付・応答を確認。異常時は制限内で自動復旧 |
| `bds-failure.service` | 異常終了・復旧上限のDiscord通知 |
| `bds-update.timer` | サーバー起動から10分後、以後`CHECK_INTERVAL`ごとに更新を確認 |
| `bds-update.service` | 更新処理。CPU・I/O優先度を下げ、実行上限30分 |
| `bds-backup.timer` | `BACKUP_ON_CALENDAR`で実行。電源断などで逃した時刻はtimer起動後に実行 |
| `bds-backup.service` | バックアップ処理。CPU・I/O優先度を下げ、待機を含む実行上限2時間 |
| `bds-player-notify.service` | 参加・退出ログを監視。監視終了時は5秒後に再起動 |

## 起動・監視・自動復旧

BDSは`supervisor.py`（Python標準ライブラリのみ）から起動します。対象はLinux・systemd・NetherNetです。`server.properties`の`transport`省略時はNetherNetと扱い、`server-port`（省略時19132）を読み取ります。旧RakNetのUDP pingは使用しません。

`bds.service`は`Type=notify`です。今回起動したBDSの`Server started.`、そのプロセス自身が所有するTCP待受、コンソールの`list`への応答を確認し、`SERVER_STABLE_SECONDS`の安定確認後に起動完了を通知します。TCP待受の確認には`/proc`を使い、定期的なTCP接続は作りません。起動通知はこの完了後に送ります。

稼働中は既定30秒間隔で待受・コンソール応答を確認し、3回連続で失敗した場合に診断を保存して正常停止を依頼します。監督処理はBDSを強制終了せず、子プロセスの終了を待ちます。停止できない場合は追加のBDSを起動しません。手動停止・バックアップによる予定停止は障害として扱いません。Discord通知の送信失敗だけでBDSを停止・再起動することもありません。

再試行の担当はsystemdだけです。`Restart=on-failure`で既定30秒待ち、10分間に初回を含め3回まで起動できます。上限に達した場合は自動起動を止め、Discordへ通知します。手動の短時間の連続起動もこの回数に含みます。原因修正後に管理者が再開する場合は`sudo systemctl reset-failed bds.service`で制限を解除してから起動してください。バックアップやDiscord受付が独自の再試行ループを持つことはありません。コピー失敗などで予定の起動まで進めなかった場合に限り、終了処理が元の稼働状態への復帰を依頼します。

| 設定 | 既定値 | 内容 |
|---|---:|---|
| `SERVER_HEALTH_INTERVAL_SECONDS` | 30 | 監視間隔 |
| `SERVER_HEALTH_RESPONSE_TIMEOUT_SECONDS` | 10 | `list`応答の待ち時間 |
| `SERVER_HEALTH_FAILURES` | 3 | 復旧開始までの連続失敗回数 |
| `SERVER_RESTART_DELAY_SECONDS` | 30 | systemdの再起動間隔 |
| `SERVER_RESTART_LIMIT` | 3 | 10分間の起動回数上限。1〜3 |

設定変更後は`configure`でunitへ反映します。監視の状態は`/run/bds-health/status.json`、異常時の記録は`/var/lib/bds-health/failure-*.json`（最新20件）に保存します。診断には直近のサーバーログを含むため、プレイヤー名などの公開範囲に注意してください。状態ファイルには今回のsystemd起動IDと更新時刻を含め、DiscordのOS再起動後の判定にも使います。

この確認はサーバー内部の応答確認です。外部ファイアウォール、経路障害、クライアントのバージョン差や実際のログイン成功まで保証するものではありません。NetherNetではTCP接続受付とゲーム通信用UDPの両方が必要です。UDPを固定する場合は`server.properties`の`server-udp-ports=19132`などとファイアウォールを一致させてください。

管理スクリプトをGitで更新するときは、最初に設定・未コミット差分を保全し、稼働中なら計画停止します。`git pull --ff-only`後、`sudo ./bds.sh configure`を実行し、元が稼働中なら`sudo ./bds.sh start`で起動します。`configure`自体はBDSを停止・起動しません。Discord連携を使用する場合は、進行中の操作要求がない状態で`sudo bash discord-control/install.sh`も実行して受付サービスと専用unitを更新します。この処理はBDS本体を再起動しません。`supervisor.py`と`discord-control/`もソースとしてGitへ登録してください。診断とゲームデータはコミットしません。

## 更新の動作

1. 公式のdownload links APIからLinux版のURLを取得します。同じバージョンなら終了します。
2. 稼働を続けたままZIPをダウンロードし、一時ディレクトリへ展開します。ZIPの破損・不正なパス・本体不足はここで検出します。
3. 必要な容量とサービス設定を確認し、稼働中ならゲーム内で停止を予告します。
4. BDSの正常終了を確認してから、実行ディレクトリ全体をコピーして更新候補を作ります。
5. 候補へ新版を反映し、元のディレクトリを退避して候補と入れ替えます。
6. 元が稼働中なら監督付きサービスを起動し、起動ログ・TCP受付・コンソール応答の確認後に確定します。更新・復元では確定直前にも同じ起動回の稼働継続を確認します。元が停止中なら停止したまま終了します。

`worlds`、`server.properties`、許可・権限設定、開発用パック、`config`、`keys`は既存データを優先します。`behavior_packs`・`resource_packs`・`world_templates`は新版の同梱ファイルを反映し、同梱ファイルとパスが重ならない独自データは残します。同梱パックそのものを直接編集した箇所は更新で置き換わります。その他の同梱ファイル・ディレクトリは新版へ置き換えます。コピーした候補から外へ出るリンクや絶対パスのリンクは、元データを変更する恐れがあるため拒否します。

APIは`net-secondary.web.minecraft-services.net`を先に試し、失敗時に`net.web.minecraft-services.net`を試します。通信には接続・全体時間の上限と再試行を設定しています。ZIPの検証は破損・構造の検査であり、別途公開された署名による検証ではありません。

起動の受付・応答待ちと、その後の安定確認には別々の時間枠を使います。起動完了をsystemdへ通知するのは監督処理だけです。バックアップ・更新・復元・Discord操作は同じ起動結果を利用します。更新・復元の確定前に異常終了した場合は失敗として扱います。

切り替えや起動・安定確認で失敗した場合は、候補の完全停止を確認してから、実行ファイル・設定・ワールドをまとめて切り替え前へ戻します。新版が変換したワールドと旧版の実行ファイルが混ざるのを避けるためです。完全停止後に旧構成へ戻せた場合は、新版の起動失敗カウンターをリセットして旧構成の起動枠を確保します。復旧後も、更新自体の終了コードは失敗になります。

## バックアップの動作と保存内容

日次・手動ともに同じ処理です。

1. 他のメンテナンスがあれば、最大`BACKUP_LOCK_WAIT_SECONDS`待ちます。期限内に開始できなければ、失敗をログとDiscordへ通知します。
2. 保存先と空き容量を確認し、稼働中なら停止を予告します。
3. 今回の起動IDに一致するBDS正常終了記録と、全プロセスの停止を確認してから、`BEDROCK_DIR`全体を一時領域へコピーします。
4. 元が稼働中なら監督付きBDSサービスを起動し、接続受付・応答まで確認します。起動失敗時の再試行はsystemdが担当します。
5. 停止中に取得したコピーを圧縮し、gzipの破損とアーカイブ構造を検証します。
6. 検証済みのファイルだけを正式名へ変更し、保持期限を過ぎたバックアップを削除します。

停止時間の中心は正常終了・全体コピー・起動です。圧縮はコピーを使って起動後に行います。途中のコピー失敗でも、元が稼働中なら復帰を試みます。再起動に失敗した場合も取得済みのコピーは保存を続け、バックアップ保存と再起動失敗を区別して通知します。

保存名は`bds-server-YYYYMMDD-HHMMSS-PID.tar.gz`です。

```text
metadata.json              # 形式・作成日時（UTC）・BDSバージョン
server/
├── bedrock_server         # 実行ファイルと同梱ライブラリ
├── .installed-version
├── .installed-url
├── server.properties
├── allowlist.json
├── permissions.json
├── behavior_packs/
├── resource_packs/
└── worlds/
```

`server/`には実際の実行ディレクトリ全体が入ります。管理用の`bds.sh`・`bds.conf`、systemd unit、入退室通知の読み取り位置は含みません。これらの管理設定は別に保管してください。外部ディレクトリへ出るシンボリックリンクや特殊ファイルを含むアーカイブは扱いません。

空き容量は全体コピーと圧縮ファイルの両方を見込み、実行ディレクトリの使用量の2倍＋`BACKUP_MIN_FREE_MB`を目安に確認します。コピー中の増加や圧縮率によっては、この確認後でも容量不足になることがあります。

作成途中の`.partial`は正常終了・通常の失敗処理で削除します。古いバックアップは新しい検証済みファイルの保存後にのみ削除します。削除対象は保存先直下の`bds-server-*.tar.gz`と`bds-worlds-*.tar.gz`です。日数判定は`find -mtime +N`のため、24時間単位の切り捨てを含み、厳密な「N日前の時刻」ではありません。

## 復元

```bash
sudo ./bds.sh restore /opt/bds/backups/bds-server-20261001-043000-12345.tar.gz
```

対応する形式は以下の2つです。

- `metadata.json`＋`server/`：バックアップ時の実行ファイル・設定・パック・ワールドを一式復元します。
- `worlds/`：現在の実行ファイル・設定を保持し、ワールドだけを復元します。利用中のBDSとワールドの互換性は管理者が確認してください。

アーカイブは停止前に一時ディレクトリへ展開・検証します。リンクは文字列上のパスだけでなく、複数のリンクを解決した実際の参照先を確認します。`worlds/`のみの形式は復元するワールドの外、一式の形式は復元するサーバーの外への参照を拒否し、候補の組み立て後にも再検証します。破損や展開失敗で、稼働中のワールドを変更することはありません。展開先には展開サイズに加えて1GiB以上の空きが必要です。

稼働中なら停止してから実行ディレクトリ全体を退避し、復元した候補に切り替えます。`worlds/`だけの場合は、停止後に現在の実行ディレクトリ全体をコピーして候補を作ります。起動ログと、その後`SERVER_STABLE_SECONDS`の安定稼働を確認します。確認に失敗した場合は、元の実行ディレクトリ全体へ戻して起動を試みます。もともと停止していたサーバーは起動しません。

復元したバージョンが最新版でなければ、その後の定期更新で更新対象になります。復元した版を維持して調査したい場合は、更新timerを停止してから復元してください。

```bash
sudo systemctl stop bds-update.timer
```

## 排他制御・復旧・退避データ

起動・停止・更新・バックアップ・復元・設定変更は、配置先の`.maintenance.lock`を共有します。このファイルは削除しないでください。バックアップは指定時間待機し、更新は競合中ならその回をスキップします。その他の管理操作は競合時に失敗終了します。入退室通知の監視はこのロックと独立しています。

停止処理では`KillMode=mixed`・`SendSIGKILL=no`を使用します。停止タイムアウト後に強制終了してコピーを進めず、状態・PID・入力用FIFO・利用可能なcgroupの残存プロセスを確認します。正常終了が確認できない場合は切り替え・コピーを中止します。

更新・復元が成功しても、切り替え前のディレクトリは残します。

```text
bedrock-server.pre-update-日時-PID/
bedrock-server.pre-restore-日時-PID/
```

起動失敗後に元へ戻した場合は、失敗した候補を`bedrock-server.failed-update-日時-PID/`または`bedrock-server.failed-restore-日時-PID/`へ保存します。復旧の停止・ディレクトリ移動にも失敗した場合は、退避先と`.bds-transaction.*`を残し、ログ・Discordで知らせます。

**これらの退避ディレクトリにはワールド全体が含まれ、自動削除しません。** 動作と必要なバックアップを確認したうえで、不要な退避先を管理者が削除してください。バックアップの保持日数はこれらには適用されません。

電源断や`SIGKILL`では終了時の復旧処理を実行できません。ディレクトリの退避と配置の間に停止した場合は、現在の配置先がない状態で退避先だけ残る可能性があります。ログ・サービスの停止状態・各ディレクトリの内容を確認し、完全停止した状態で必要な一式を戻してください。停止を確認できない場合、データを移動しないでください。

まず確認するコマンドは以下です。

```bash
./bds.sh status
./bds.sh logs --lines 100
./bds.sh logs update --lines 100
./bds.sh logs backup --lines 100
```

## Discord通知と参加・退出監視

`DISCORD_WEBHOOK_URL`を設定すると、起動・停止、メンテナンスの完了・失敗などを通知します。通常の起動通知はサービス開始時点の通知です。メンテナンスの起動確認では、別途その起動回の`Server started.`ログを確認し、更新・復元の候補は追加の安定確認も行います。

通常通知・入退室通知は同じ送信処理を使います。通信エラーとHTTP 408/5xx/429を最大3回試行し、通信・再試行待機の合計を`DISCORD_TIMEOUT_SECONDS`で制限します。1回の通信は接続5秒・全体20秒が上限で、残り時間が短ければ全体の上限を短縮します。429は応答で指定された待機時間、それ以外は2秒・4秒待ちます。指定待機時間を残り時間に収められない場合は、その通知の再試行を打ち切ります。

失敗ログにはHTTPステータス・curl終了コード・試行数を残し、Webhook URL・応答本文・curlの詳細は出力しません。通常通知の送信失敗だけを理由にBDSやメンテナンスを失敗させません。通常通知の再送キューはありません。

参加・退出通知は追加で有効にします。

```bash
# bds.conf
DISCORD_WEBHOOK_URL=https://discord.com/api/webhooks/...
PLAYER_NOTIFY_ENABLED=true
```

```bash
sudo ./bds.sh configure
./bds.sh logs player-notify --follow
```

通知例は「Steve がログインしました。」「Steve がログアウトしました。」です。名前に含まれるDiscordのメンションは発火させません。送信先は他の運用通知と共通です。

監視サービスはシェル・`journalctl`・`jq`で構成され、`curl`は送信時だけ起動します。BDSのログを独立して読むため、Discordの応答待ちでBDSのログ出力を待たせません。BDSが停止している間も監視は待機します。

- 初回はjournalの現在位置から開始し、過去の参加・退出をまとめて送りません。
- 処理したログの位置を`PLAYER_NOTIFY_STATE_DIR/cursor`へ保存し、監視の再起動後は続きから読みます。
- 1通知の試行は共通の時間上限に従います。429の待機が上限を超えた場合、その通知はスキップし、独立した監視側が指定時間を待ってから後続イベントの送信へ進みます。この追加待機はBDSの処理や通常通知を停止させません。
- 404などは再試行しません。送信できなかった通知はログへ記録してスキップします。失敗分の自動再送キューはありません。
- 送信後、位置保存前に停止すると重複通知の可能性があります。journalから削除された履歴は復元できません。
- 監視処理が終了するとsystemdが5秒後に再起動します。設定不備や二重起動は終了コード78となり、自動再起動しません。

読み取り位置が無効で再開できない場合は、監視サービスを停止し、`cursor`を退避してから再起動します。現在位置からの再開になるため、未処理分は通知されません。通常の再起動では削除しないでください。

無効化は`PLAYER_NOTIFY_ENABLED=false`にして`configure`を実行します。読み取り位置は保持されるため、再び有効にすると保存済み位置から読みます。

## Discordからの再起動（任意）

`discord-control/`の受付サービスを追加すると、指定したDiscordサーバーの許可メンバーが`/restart`（Minecraftのみ）と`/reboot`（VPSのOS全体）を使用できます。両方で同じユーザー・ロールの許可リストを使います。通常の`configure`ではこの機能を公開・有効化しません。

署名付きのHTTP要求を一般ユーザー権限で受け、本人だけが使える60秒有効の確認ボタンを表示します。確定後は固定されたsystemd unitがroot権限で`control-restart`または`control-reboot`を実行します。任意のシェルコマンドは受け付けません。受付はループバックだけで待受け、外部公開にはHTTPSが必要です。

両操作とも、既存のメンテナンスロックを取得できなければ中止します。既定ではゲーム内に60秒前・10秒前の予告を送ります。予告コマンドの送信やBDSの正常停止に失敗した場合、OS再起動へ進みません。`/restart`は起動ログと安定稼働を確認し、`/reboot`はタイマーと新規メンテナンスを抑止してからOS再起動を要求します。OS再起動要求が拒否された場合は、停止が確認できたBDSと、それまで稼働していたタイマーの復帰を試みます。

`/restart`の停止中に異常が起きた場合は、元が稼働中かつ完全停止を確認できる場合に限り、1回だけ復旧起動を試みます。通常の起動を既に試していた場合は繰り返しません。復旧起動でも安定稼働まで確認し、元の要求の失敗はそのまま通知します。

Discordへの完了通知には、root側のunitが`/run/bds-control-restart/`または`/run/bds-control-reboot/`へ保存した実行ID付きの結果を使います。終了済みunitの履歴破棄や受付サービスの再起動で結果を失わず、過去の結果を今回の成功とは扱いません。

OS再起動の待機マーカーは`/run/bds-reboot-pending`です。通常はOS再起動で消えます。要求が受理された後もOSが再起動しなかった場合は、ログと状態を確認してから管理者が解除・復帰してください。強制再起動は行いません。

導入・HTTPS・Discord側の設定・停止方法は[Discord操作の設定](discord-control/README.md)を参照してください。入退室通知は独立した機能で、Discord操作受付を追加しても変更しません。

## サービス登録の削除

```bash
sudo ./bds.sh uninstall
```

BDS・入退室通知サービスとtimerを停止・無効化し、BDSの停止状態を確認してからunitを削除します。停止要求に失敗した場合や、BDSのプロセス・入力FIFOが残る場合は削除を中止し、失敗終了します。存在しないunitはスキップします。本体・設定・ワールド・バックアップ・退避データは残します。

Discord操作受付を導入している場合は、その受付サービスを先に無効化してください。`uninstall`は独立したDiscord操作用unit・sudoers・HTTPS設定を削除しません。

## ローカル編集と検証

ローカルで編集した`bds.sh`をSFTPなどでサーバーへ配置し、サービス設定を反映します。実行中のメンテナンスを避けてアップロードしてください。`bds.conf.example`は設定例であり、運用中の`bds.conf`を上書きするものではありません。

```bash
sudo /opt/bds/bds.sh configure
```

ローカルのテストは実サーバーへ接続しません。サービス・通信・一部の外部コマンドを模擬し、実際のシェル・JSON解析・アーカイブ・ファイル操作で、正常動作と途中失敗時の復旧を検証します。

```bash
bash -n bds.sh
python3 -m unittest discover -s tests -v
```

テストにはBash・Python 3・`jq`と標準的なUnixコマンドが必要です。復元するリンク、停止失敗、復旧中の停止・移動失敗、起動ログ後の異常終了、通知の時間上限、容量表示も検証します。

Discord操作の署名・HTTPテストも実行する場合は、Python仮想環境に`discord-control/requirements.txt`の依存ライブラリを入れてから同じテストを実行します。依存がない場合はこの部分だけ明示的にスキップします。テスト用HTTPはループバックの一時ポートで待受けます。OS再起動・Discord送信は模擬処理です。

Linuxの実際のsystemdを使う検証は、明示的に別コマンドで実行します。

```bash
sudo python3 tests/systemd_smoke.py --run
```

この検証は、一意の`bds-smoke-*`という一時unitと`/tmp`の専用データを作り、生成したunitの検査、起動・正常停止・再起動、バックアップ、更新、起動直後の異常終了からの復旧、復元、登録削除を確認します。サービスにはCPU・メモリ・プロセス数の制限を付けます。使用するサーバーは模擬プログラムで、配布ファイルの取得もローカルの試験データです。Minecraftのポートは開かず、Discordへ通知しません。

既存の`bds.service`や`/opt/bds`は変更せず、正常終了・通常の失敗時に試験用unitとデータを削除します。試験用サービスの停止を確認できない場合はデータを残して報告します。実BDS本体・実ワールドとの互換性確認は別途必要です。
