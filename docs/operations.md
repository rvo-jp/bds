# 異常時の確認

- `sudo bdsctl status`：今回の起動ID、BDSのPID、起動・異常・正常終了の記録。
- `systemctl status bds.service`と`journalctl -u bds.service`：起動回数上限、停止待ち、起動前の復旧エラー。
- `/var/lib/bds-supervisor/*.json`：最新20件の異常理由と直近のBDS標準出力。
- `/var/lib/bds/state/transaction.json`：中断した更新・復元の段階と復旧元。
- `/var/lib/bds/state/discord-outcome.json`：最後にroot側で受けたDiscord操作のIDと結果。

## 停止が終わらない

停止が完了するまでバックアップや二重起動を行いません。`stop`への反応、BDSのプロセス状態、ディスクI/O、空き容量を調べてください。管理者が強制終了を選ぶ場合、ワールドの保存が完了しているとは扱えません。正常終了記録がない状態で作られたコピーを正常バックアップと呼ばないでください。

## 更新・復元が中断した

BDSの停止を確認してから`sudo bdsctl recover`を実行します。起動前にも復旧処理が走ります。記録やバックアップが壊れていれば、無理に起動せず停止します。`transaction.json`の削除だけで復旧したことにはなりません。

更新版の起動失敗で巻き戻した場合は、その版を`rejected-release.json`に記録します。自動では同じ版を適用しません。原因を直して明示的に再試行するときは`sudo bdsctl update`を実行します。

## Discord操作が「結果不明」で次の操作を受け付けない

受付したIDは`/var/lib/bds-discord/job.json`、実行結果は`/var/lib/bds/state/discord-outcome.json`です。結果不明の操作を再送しない設計なので、サービス終了と受付側の再起動が重なると、管理者の確認が必要になる場合があります。

Minecraft・OSの実際の状態と、`bds-discord-restart.service`・`bds-discord-reboot.service`の稼働状態を調べてください。OS再起動の場合は記録の`boot`と`/proc/sys/kernel/random/boot_id`が異なれば再起動済みです。処理が残っていないことを確認した後、受付サービスを停止し、`job.json`を管理者用の退避先へ移して受付サービスを起動すると、新しい確認を受け付けられます。実行中の記録を消したり、同じ確認を再送したりしないでください。

OS再起動要求が受理された後は`reboot-pending.json`に起動IDを残し、同じOS起動中の別操作を拒否します。OS起動IDが変われば自動で解除します。OS再起動が実際には進んでいない場合は、systemdのジョブとBDS停止を調べた上で、この記録を退避してからBDSを起動してください。

## 外部へ書き出したバックアップ

`export`のtar.zstにはワールド・設定・独自パックと検証用の一覧が入ります。BDS本体は含みません。別マシンへ移す場合は、同じ版の`/opt/bds/releases/バージョン`も別途保管してください。公式配布が終了した版を後から取得できるとは限りません。外部アーカイブからの新規サーバー再構築は、自動の一括取り込みではなく、管理者が同じ版の本体とバックアップを配置・検証してから行います。
