# MinecraftのIPBAN

IPによる拒否はVPSのカーネルで実施します。BDSやDiscord受付にBAN処理や常駐監視を追加しません。通常のポート許可は既存UFW、MinecraftのIPBANはUFW起動時に読み込む専用のnftables表`inet bds_ipban`が担当します。独立した`nftables.service`は起動しません。

## 正本と適用範囲

BAN対象の正本はVPSの`/etc/ufw/bds-ipban.nft`（root所有・600）だけです。実際の対象IPやプレイヤー名をGitへ記載しません。[設定例](../deploy/ufw-ipban.nft.example)には文書用アドレスを使っています。

`blocked_v4`・`blocked_v6`の集合を受信と送信で共有します。対象は、指定IPからTCP・UDP 19132への受信と、TCP・UDP 19132から指定IPへの送信です。SSH、HTTP、HTTPSや他のポートはこの表では拒否しません。ポートを変える場合は正本の`minecraft_port`も変更してください。

専用表のinput/output優先度は-10で、既存UFWのfilter優先度0より先にDROPします。既存接続も例外にせず拒否します。BDSが認識する退室は接続タイムアウトまで遅れる場合があります。IPとプレイヤー名の対応は別の情報であり、同じ回線の利用者やVPN・中継利用者をIPだけで断定しません。

## 起動・再読込

初回は設定例をもとに正本を新規作成し、実際の対象IPとポートを設定します。既存の正本を設定例で上書きしません。VPSのUFWがnftablesバックエンドで動き、専用表の優先度が既存の許可より先になることを確認します。設定例には`destroy table`を使うため、対応するnftが必要です。構文確認に成功してから適用します。

```sh
sudo nft --check --file /etc/ufw/bds-ipban.nft
sudo nft --file /etc/ufw/bds-ipban.nft
```

ファイルを一つのバッチとして読み込み、専用表だけを原子的に入れ替えます。入力エラーでは稼働中の表を残します。適用時に表のカウンターはリセットされます。通常のUFWルールや他の表を消す`flush ruleset`は使いません。

既存の`/etc/ufw/before.init`の内容を保ち、`start)`内に次の処理を追加して、ファイルをroot所有・750で実行可能にします。

```sh
/usr/sbin/nft --file /etc/ufw/bds-ipban.nft
```

UFWがOS起動時に有効であることを確認します。これで起動・`ufw reload`時に正本を再適用します。再読込途中の`stop`では専用表を削除せず、遮断の空白を作りません。**`ufw disable`・`systemctl stop ufw`だけではIPBANは解除しません。** BAN解除は集合から対象IPを削除して同じファイルを適用します。全件解除は両集合を空にします。サービス全体を撤去する場合だけ、起動フックを外して`sudo nft destroy table inet bds_ipban`を実行し、正本を削除します。

既にUFW通常ルールへ同じ拒否がある場合は、専用表を適用・確認した後で古いルールを削除します。対象IPを通常UFWルールや別のファイルへ重複管理しません。

## 確認と追加・解除

```sh
sudoedit /etc/ufw/bds-ipban.nft
sudo nft --check --file /etc/ufw/bds-ipban.nft
sudo nft --file /etc/ufw/bds-ipban.nft
sudo nft list table inet bds_ipban
sudo ufw status numbered
```

`ufw status`は通常のUFWルールだけを表示します。IPBANの集合・処理順・カウンターは`nft list table`で確認します。ルール適用後は、新しいSSH接続、外部からのゲーム用HTTP応答、BDSとDiscord受付の稼働も確認します。実際のBAN対象の回線から遮断されるかは、対象回線を利用できる人だけが確認できます。Xserver側のポート許可を変更する必要はありません。

## 検証

専用表の変更は、本番と分離したネットワーク名前空間でTCP・UDPの新規通信と確立済み通信、受信と送信、対象外IPと管理ポート、再適用・不正設定・解除を検証します。確認のために本番BDSを停止したり、BAN対象へ外部パケットを送ったりしません。

```sh
sudo python3 tests/firewall_acceptance.py deploy/ufw-ipban.nft.example
```

試験はホストのネットワークでの内部実行とPythonの最適化モードを拒否します。UFW後段の拒否が迂回されないことも確認します。

UFWの拡張フックと通常ルールの順序は[UFWマニュアル](https://manpages.ubuntu.com/manpages/noble/man8/ufw-framework.8.html)、表・集合・優先度は[nftマニュアル](https://netfilter.org/projects/nftables/manpage.html)に従います。
