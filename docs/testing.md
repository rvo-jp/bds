# 検証方法

Rustテスト、静的検査、Linuxのsystemdを使った隔離試験をCIで実行します。結果はコミットごとのCIと配布成果物で確認します。過去の結果をソースディレクトリへ蓄積しません。

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked --bins --examples
sudo python3 tests/linux_acceptance.py target/release/bdsctl target/release/examples/fake_bds
```

隔離試験は一時ユーザー・一時サービス・模擬BDS・一時ワールド・ループバックの空きポートを使います。実際のMinecraftサービスを停止・変更しません。起動確認、停止バックアップ、コピー失敗からの復帰、コピー親プロセス終了時のロック保持、復元、更新失敗時のワールドと本体の巻き戻し、中断後の復旧、root操作結果の読み取り、同じ操作の再実行拒否を検証します。

常駐するRustの2プロセスについて合計PSSと待機時CPUを30秒測定します。BDS本体・Caddy・実際のDiscordへの通知送信はこの測定に含めません。本番切り替え時には、実際のワールド・Minecraftクライアント・Discordでの通知と操作も確認してください。
