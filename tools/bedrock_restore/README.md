# オフラインの部分復元

移行時だけ使用する補助ツールです。停止時バックアップの独立コピーから、指定範囲と明示した2人の持ち物を戻した候補を作成します。実行にはPython 3.11、Mojang形式のLevelDBライブラリ、NBTライブラリ、NumPyを使用します。常駐・定期実行は行いません。Rust管理バイナリの実行依存にも追加しません。

今回の対象・候補コミット・進捗・準備物の場所は、リポジトリ直下の `TODO.md` を正本とします。XUID・保存レコードIDを含む実行計画、ワールド、検証ログ、配布物はGitに保存しません。

## 復元するデータ

- 標準オーバーワールドの高さ全域（Y=-64〜319）と指定X/Z範囲。ブロック座標の両端を含み、エンティティは足元のブロック座標で範囲を判定します。
- 地形・建築・各ストレージ層、ブロックエンティティと収納、範囲内の高さ・バイオーム、予定されたブロック更新。予定時刻は現在側のチャンク時刻に合わせて残り時間を維持します。
- 範囲内で参照されているエンティティとチャンク参照。参照のない古いレコードを復活させません。現在側のプレイヤーと `rvo:flag` は維持し、バックアップのフラッグを追加しません。
- 明示した2人の `Inventory`・`Armor`・`Offhand`・`EnderChestInventory`。`Mainhand` は現在の選択スロットにある復元後のアイテムから再構築します。それ以外のプレイヤーデータは現在のままです。

ワールドの時間・設定・チーム情報・生成メタデータ・構造物の領域情報・他プレイヤーのデータは現在側を維持します。共有の `BiomeData`（バイオーム別の積雪量）も現在のままです。`MetaDataHash` 等を過去の値へ戻して、現在の共有辞書との対応を壊すことは避けます。範囲外へ移動済みの同じ個体は、その場所に残します。

地図・追跡先などの共有データも現在側を維持します。復元アイテムが必要とする共有レコードが復元元に存在し、現在側から失われている場合は、自動で共有データを巻き戻さず拒否します。地図の描画内容等を過去の状態へ戻す機能は含みません。

永続ストレージ形式のsubchunk v8/v9と、24/25セクションのData3Dに対応します。未知の変更形式、対象内の曖昧なエンティティ参照、境界をまたいで変化する乗客・リード・ラージチェスト等は、候補を完成扱いにせず拒否します。対象外の既存の重複・欠損参照は、そのまま保持します。

地形が保存されていないチャンクは空気として扱いません。調査結果と `missing_source_chunks` の明示指定が一致した部分だけを除外し、現在の状態を残します。

## 環境とテスト

作業用PCまたは隔離した一時環境で使用します。Linuxでソースから依存を構築する場合は、GCC/G++とzlibのヘッダー・リンク用ファイルが必要です。本番のシステムPythonやサービス依存は変更しません。

```sh
python3.11 -m venv /private/tmp/bedrock-restore-env
/private/tmp/bedrock-restore-env/bin/python -m pip install -r tools/bedrock_restore/requirements-build.txt
/private/tmp/bedrock-restore-env/bin/python -m pip install --no-build-isolation -r tools/bedrock_restore/requirements.txt
/private/tmp/bedrock-restore-env/bin/python -m unittest tools.bedrock_restore.test_restore -v
```

依存バージョンの正本は `constraints.txt` です。CIでは合成したデータだけを使用し、座標境界・複数層・NBTの保持・参照・本人対応・正常停止条件・実際のLevelDBへの候補書き込みを検証します。実ワールドはCIへ送信しません。

## コピーと計画の準備

以下のコマンドはリポジトリのルートから実行します。`python` は上記の作業用環境のPythonを指します。ワークスペースは公開リポジトリ外の非公開ディレクトリに置きます。

```sh
python -m tools.bedrock_restore.workspace WORKSPACE source SOURCE_SNAPSHOT_DIRECTORY
python -m tools.bedrock_restore.workspace WORKSPACE baseline BASELINE_SNAPSHOT_DIRECTORY
```

入力は `snapshot.json` と `data/` を持つ完了済みのバックアップです。全ファイルのSHA-256・構成・サイズを照合し、独立したinodeの `source-copy/`・`baseline-copy/` を作成します。既存コピーへの上書き、シンボリックリンク、作業コピーのハードリンクは拒否します。ネイティブDBライブラリはコピー上でも管理ファイルを変更し得るため、保存用の復元元は別に保持し、直接開きません。

`restore-plan.json` の形式は次のとおりです。値は実データで確認したものへ置き換えます。

```json
{
  "world": "World",
  "dimension": "minecraft:overworld",
  "region": {"x_min": 0, "x_max": 31, "z_min": 0, "z_max": 31},
  "players": [
    {"name": "PlayerA", "key": "player_server_UUID_A", "xuid": "123456789"},
    {"name": "PlayerB", "key": "player_server_UUID_B", "xuid": "987654321"}
  ],
  "excluded_entities": ["minecraft:player", "rvo:flag"],
  "missing_source_chunks": []
}
```

本人対応は参加ログの名前・XUIDと、DB内のXUID由来のMsaId・ServerIdを照合します。計画に記載した2人の対応が復元元・現在側の双方で一致しなければ拒否します。

```sh
python -m tools.bedrock_restore WORKSPACE WORKSPACE/restore-plan.json
```

候補は `candidate-copy/` へ新規作成します。境界内の座標ごとのブロック・収納・バイオームの保持、変更キーの読み戻し、変更対象外の全論理レコードのハッシュを確認し、`restore-report.json` を作成します。エラー・中断時の候補は適用しません。再実行で既存候補を自動削除することもありません。

## 隔離した起動リハーサル

Linux/root環境では `rehearsal.py` を使用します。ワークスペースには検証済みの候補、計画、ビルド済みアドオンのアーカイブとファイル別ハッシュを用意します。フラッグは毎回その候補から列挙し、過去の調査リストには依存しません。このコマンドは今回の移行準備で使用する配置・JSON形式に対応したものです。

```sh
python -m tools.bedrock_restore.rehearsal WORKSPACE STAGED_BDSCTL EXPECTED_MANAGER_COMMIT
```

`/var/lib/bds-prep-*` の使い捨てコピーと専用の一時unitを作成し、検証済み管理バイナリ・現在のBDS・更新BP/RPで起動します。本番データ・設定・状態のパスを不可視にし、PrivateNetworkで通信を分離し、失敗通知unitへの連携を外します。CPUは50%、メモリは2GiBを上限とします。使い捨てunitに限り開始・停止・実行時間の上限を設け、停止が進まない場合はそのunit内だけを終了させます。

中心と四隅を読み込ませ、ログ・正常終了・フラッグの位置/HP/所属・チーム情報・2人のデータを確認します。ゲーム内での表示・プレイヤー操作・端末ごとのリソース取得は別途確認が必要です。検証後のワールドは使用せず、一時unitと使い捨てコピーを削除します。結果は `rehearsal-report.json` と `isolated-bds.log` に保存します。開始時に古い成功記録を無効化し、計画とオフライン照合結果のハッシュに結び付けて、後片付けまで成功した場合だけ `passed` を保存します。

## 本番停止後の作業順序

ここからは移行・停止の了承後だけ実施します。事前に作った候補は、準備後のプレイ内容を含まないため本番へ配置しません。

1. 停止直前のサービス・タイマーの稼働/有効化状態、採用バイナリ、各準備物のハッシュを記録する。Discordの案内後に `bdsctl maintenance-begin` で予告・正常停止する。日次/更新タイマーとDiscord受付も停止する。
2. 保守中の通常の `bdsctl backup` は、意図的に拒否される。代わりに以下で、保守状態・同一起動での正常終了・子プロセスの消滅・共通操作ロックを確認し、安全退避を作成する。

   ```sh
   python -m tools.bedrock_restore.safety --capture-after-authorized-stop
   ```

   出力先は `/var/backups/bds-migration-safety-*`。`data/`、ハッシュ付き `snapshot.json`、現行バイナリ/サービス定義等の `manager.tar` を含む。通常の世代管理外で自動削除されず、`bdsctl snapshots` には表示されない。部分コピーやmanifest未完成の退避は使用しない。
3. 保存用の復元元と、直前の安全退避を入力に**新しいワークスペース**を作る。直前の退避をbaselineとして候補を再構築し、対象外の照合と更新アドオン込みの隔離起動を再実施する。
4. 検証済み管理バイナリの設置には、停止済み状態で `deploy/apply.sh` を使う。`deploy/upgrade.sh` は自動的に保守終了まで進むため、複数作業をまとめる今回は使用しない。設定・保守ガード・元の有効化状態を維持する。
5. 検証済み候補のワールドとビルド済みBP/RPを停止中に配置する。古いパックを重複配置せず、ワールドの登録をmanifestへ合わせ、所有権を整える。起動リハーサル用のコピーを配置しない。
6. Discord受付・タイマーを元の稼働状態へ戻し、最後に `maintenance-end` でゲームを起動する。実機でワールド・持ち物・フラッグ・パック・Discord受付を確認する。
7. 失敗時は保守状態でゲームを停止したまま、同じ安全退避のデータ・パック登録・必要な管理ファイルを一式で戻す。再開後に問題が判明した場合は、改めて `maintenance-begin` と受付・タイマーの停止を済ませてから戻す。管理ファイルを戻した場合はdaemon-reloadも行う。状態ファイルを削除して無理に起動しない。

既存の復元元世代が残っていれば、移行時に保護します。全確認が終わったら不要な検証コピー・一時Python・一時配布物を整理します。安全退避の削除は復旧不要と判断できてから行います。

## 保存形式の参照

- [Microsoft: Actor storage](https://learn.microsoft.com/en-us/minecraft/creator/documents/actorstorage)
- [Amulet LevelDB](https://github.com/Amulet-Team/Amulet-LevelDB)
- [Amulet NBT](https://github.com/Amulet-Team/Amulet-NBT)
- [AmuletのBedrockチャンク実装](https://github.com/Amulet-Team/Amulet-Core/tree/1.9.49/amulet/level/formats/leveldb_world/interface/chunk)
- [XUID由来UUIDの実装仕様](https://powernukkitx.github.io/PowerNukkitX/org/powernukkitx/Server.html)
