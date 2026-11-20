# MinecraftのIP履歴

VPSへ届いたMinecraft用TCP・UDP 19132の通信を、カーネルのnftablesで観測します。既存のsystemd-journaldへ記録するため、Rust管理プログラムの変更、定期ポーリング、追加の常駐プログラムはありません。プレイヤーとの対応付けは管理者が参加ログとの時刻照合で候補を絞ります。自動照合・自動BAN・DiscordへのIP送信は行いません。

## 記録内容と制限

記録には`[BDS-IP]`を付け、journalの時刻と、受信インターフェース、送信元・宛先IP、TCP/UDP、ポートなどのパケットヘッダー情報を残します。通信内容や認証情報は保存しません。

- IPv4・IPv6とも、**送信元IP＋TCP/UDP**をキーとして最初の観測から5分間は重複を記録しません。通信が続けば5分経過後の次のパケットで再記録します。重複通信で期限は延長しません。
- 記憶するキーはIPv4・IPv6それぞれ最大4,096件、期限切れの回収間隔は1分です。集合が満杯の場合はその観測を記録せず、通信はそのまま後段へ渡します。
- 全体で毎秒5件・バースト50件の共通制限があります。制限で記録できなかったIPは記憶せず、次の通信時に再判定します。パケット全件をユーザー空間へ転送しません。
- ループバック通信と他ポートは対象外です。実際のBDSのポートを変更した場合は観測設定の`minecraft_port`も変更します。
- IPBAN・通常UFWより前で観測するので、拒否された通信、一覧表示のアクセス、スキャンも含みます。通信を許可・拒否する責務は持ちません。

これは接続試行を含む通信履歴です。ログイン成功、参加・退出時刻、現在オンラインであること、アカウントとの対応を保証しません。NAT・VPN・中継で複数人が同じIPになり、回線変更で同じ人のIPも変わります。UDPでは送信元偽装の可能性もあり、時刻が近いだけで本人と断定しません。記録制限、集合満杯、カーネル・journal側の抑制、障害による欠落もあり得ます。

## 正本と保存先

観測設定の正本はVPSの`/etc/ufw/bds-ip-history.nft`（root所有・600）です。[設定例](../deploy/ufw-ip-history.nft.example)を使い、IPBANの`bds-ipban.nft`とは別に管理します。専用表`inet bds_ip_history`のinput優先度は-20、IPBANは-10、通常UFWは0です。観測表のacceptポリシーは後段の拒否を迂回しません。

永続履歴の保存先は既存の`/var/log/journal`だけです。rsyslogへ転送された同じメッセージを`syslog`・`kern.log`へ重複保存しないよう、[専用フィルター](../deploy/rsyslog-bds-ip-history.conf)を`/etc/rsyslog.d/19-bds-ip-history.conf`へ配置します。カーネルの一時的なリングバッファにも存在します。journal全体の保存設定は変更しません。

IP履歴専用の日数保証や容量枠はありません。既存journalの共有容量上限に応じて古いアーカイブが自動削除されます。`SystemMaxUse`の既定値はファイルシステムの10%・最大4GiBですが、稼働中のファイルは削除対象外のため厳密なディスク上限ではありません。実際の設定と使用量は次で確認します。長く保全する必要がある範囲だけ、管理者専用ファイルへ書き出してください。実IPを含むログや書き出しファイルはGitへ入れません。

```sh
sudo systemd-analyze cat-config systemd/journald.conf
sudo journalctl --disk-usage
```

## 設置・起動・再読込

既存UFWとnftablesを使います。`destroy table`に対応したnftが必要です。独立した`nftables.service`は有効化せず、`flush ruleset`は使いません。手元のチェックアウトから設定ファイルだけをVPSの一時領域へ転送し、以下の転送元パスを置き換えます。既存ファイルを確認してから設定例を配置し、構文確認します。VPSへソース一式を置く必要はありません。

```sh
sudo install -o root -g root -m 600 deploy/ufw-ip-history.nft.example /etc/ufw/bds-ip-history.nft
sudo nft --check --file /etc/ufw/bds-ip-history.nft
sudo install -o root -g root -m 644 deploy/rsyslog-bds-ip-history.conf /etc/rsyslog.d/19-bds-ip-history.conf
sudo rsyslogd -N1
sudo systemctl restart rsyslog
sudo nft --file /etc/ufw/bds-ip-history.nft
```

rsyslogの再起動はフィルターを読み込むためです。journaldやMinecraftは停止しません。rsyslogを使用しない環境では専用フィルターは不要ですが、他の転送・ログ出力があれば重複保存の有無を確認します。

既存の`/etc/ufw/before.init`（root所有・750）の内容を保ち、`start)`にIPBANの読み込みと並べて次を追加します。UFWがOS起動時に有効であることを確認します。

```sh
/usr/sbin/nft --file /etc/ufw/bds-ip-history.nft
```

OS起動・`ufw reload`で再適用されます。`stop`では表を削除しません。**`ufw disable`・`systemctl stop ufw`だけでは観測もIPBANも止まりません。** 変更時は上記の`nft --check`に成功してから`nft --file`で適用します。専用表だけを原子的に交換するので、入力エラーでは旧表を維持します。再適用・OS再起動で重複抑制の集合とカウンターはリセットされ、次の通信が新たに記録されます。永続履歴は消えません。

## 履歴の確認と手動照合

```sh
# 時刻はVPSのローカル時刻（本環境は日本時間）。-kだけでは現起動分に限定される。
sudo journalctl _TRANSPORT=kernel --grep '\[BDS-IP\]' --since 'today' -o short-iso --no-pager
# 参加ログを同じ時間帯で確認する。
sudo journalctl -u bds.service --since 'today' -o short-iso --no-pager
# 記録処理の累計と、一時的な重複抑制用の集合。
sudo nft list table inet bds_ip_history
```

`candidates`は重複を除いた観測候補、`rate_limited`は頻度制限で記録を省略したパケット、`set_add_failed`は集合への追加失敗、`log_attempted`はカーネルへのログ出力を試みた回数です。カウンターはパケット数であり、人数・ユニークIP数ではありません。`log_attempted`もjournalへの保存成功数を保証しません。集合は現在オンラインの一覧でも、永続履歴でもありません。

候補を調べる場合は時間帯を狭め、IP履歴の時刻とBDSの参加ログを並べて確認します。5分間の重複抑制中の再接続は新規記録されない点を考慮してください。journalは管理権限で読み取り、一般プレイヤーや公開サービスへ提供しません。

## 無効化・削除

観測を停止するには、UFW起動フックから**観測用の1行だけ**を外し、`sudo nft destroy table inet bds_ip_history`を実行します。IPBAN表と通常UFWは変更しません。撤去する場合は観測設定を削除し、rsyslogフィルターも削除して`rsyslogd -N1`・rsyslog再起動を行います。

既存の履歴はjournalの通常ローテーションまで残ります。journalはメッセージ単位の削除に対応しないため、IP履歴だけを選択削除する手順は設けません。IP履歴を消す目的でjournal全体をvacuumすると、BDSや他サービスの診断記録も消えるため使用しません。手動の書き出しファイルは管理者が不要になった時点で削除します。

## 検証

Linux root環境で、設定をホストのネットワークへ適用しない試験を実行します。vethでつないだ2つの隔離ネットワークを使い、終了時に削除します。

```sh
sudo python3 tests/ip_history_acceptance.py deploy/ufw-ip-history.nft.example deploy/ufw-ipban.nft.example
```

IPv4/IPv6・TCP/UDPの記録対象、重複抑制、期限の非延長、頻度制限・集合満杯でも通信が通ること、BANの維持、原子的な再適用と撤去を確認します。ホスト名前空間での内部実行とPythonの最適化モードを拒否します。隔離試験はカウンターで出力条件を検証し、本番反映後に実際のjournalへの保存と重複ファイルがないことを別に確認します。

仕様の根拠は[nftマニュアル](https://netfilter.org/projects/nftables/manpage.html)と[journaldの保存設定](https://www.freedesktop.org/software/systemd/man/latest/journald.conf.html)です。
