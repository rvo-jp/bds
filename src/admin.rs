//! On-demand administration. No resident tasks or periodic status writes.
use crate::state::Record;
use crate::{config::Config, operations, releases, snapshots, supervisor, util};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::fs;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MaintenanceMode {
    pub created: u64,
    pub was_active: bool,
    pub phase: MaintenancePhase,
}
crate::model::text_enum! {
    pub enum MaintenancePhase {
        Stopping => "stopping",
        Ready => "ready",
    }
}
const MODE: Record<MaintenanceMode> = Record::new("maintenance-mode.json", 0o644);

pub fn mode_path(c: &Config) -> std::path::PathBuf {
    MODE.path(c)
}
pub fn in_maintenance(c: &Config) -> bool {
    MODE.exists(c)
}
pub fn ensure_available(c: &Config) -> Result<()> {
    ensure!(
        !in_maintenance(c),
        "保守中です。管理者がmaintenance-endを実行するまで起動・予約処理・再起動を受け付けません"
    );
    Ok(())
}
pub fn begin(c: &Config) -> Result<()> {
    let _lock = crate::state::lock(c, 0)?;
    operations::ensure_no_interruption(c)?;
    let mut m = if in_maintenance(c) {
        MODE.load(c)?
    } else {
        let m = MaintenanceMode {
            created: util::now(),
            was_active: operations::active(c)?,
            phase: MaintenancePhase::Stopping,
        };
        // Inhibit starts before attempting to stop. A failed stop never releases this guard.
        MODE.save(c, &m)?;
        m
    };
    if operations::active(c)? {
        operations::maintenance_notice(c, "保守作業")?;
        operations::stop(c)?;
    }
    operations::ensure_stopped(c)?;
    m.phase = MaintenancePhase::Ready;
    MODE.save(c, &m)?;
    println!("保守中です。ワールド編集が終わるまでmaintenance-endを実行しないでください。");
    Ok(())
}
pub fn end(c: &Config) -> Result<()> {
    let _lock = crate::state::lock(c, 0)?;
    let m: MaintenanceMode = MODE.load(c).context("保守モードではありません")?;
    ensure!(
        m.phase == MaintenancePhase::Ready,
        "停止が完了していません。maintenance-beginを再実行してください"
    );
    operations::ensure_stopped(c)?;
    operations::prepare_after_maintenance(c)?;
    let result = (|| {
        MODE.remove(c)?;
        if m.was_active {
            operations::start(c)?;
        }
        Ok(())
    })();
    if let Err(e) = result {
        // Includes a directory-sync failure after removing the guard.
        let saved = MODE.save(c, &m);
        if m.was_active {
            let _ = util::systemctl(&["stop", &c.server.unit]);
        }
        saved.context("保守状態の再保存にも失敗しました")?;
        return Err(e);
    }
    println!(
        "保守を終了しました。{}",
        if m.was_active {
            "Minecraftを起動しました。"
        } else {
            "元から停止中のため起動していません。"
        }
    );
    Ok(())
}
pub fn jst(seconds: u64) -> String {
    i64::try_from(seconds)
        .ok()
        .and_then(|n| chrono::DateTime::from_timestamp(n, 0))
        .map(|t| {
            t.with_timezone(&chrono::FixedOffset::east_opt(9 * 3600).unwrap())
                .format("%Y-%m-%d %H:%M:%S JST")
                .to_string()
        })
        .unwrap_or_else(|| "不明".into())
}
pub fn snapshot_line(s: &snapshots::Snapshot) -> String {
    let bytes: u64 = s.files.values().map(|f| f.bytes).sum();
    format!(
        "{}\t{}\t{}\t{}\t{:.1} MiB\t{}",
        s.id,
        jst(s.created),
        s.reason,
        s.release,
        bytes as f64 / 1048576.0,
        if s.protected { "保護" } else { "通常" }
    )
}
pub fn status(c: &Config) -> Result<()> {
    let state = util::prop(&c.server.unit, "ActiveState")?;
    println!(
        "Minecraft: {state}（起動完了記録: {}）",
        if supervisor::running(c) {
            "一致"
        } else {
            "未確認"
        }
    );
    println!(
        "バージョン: {}",
        releases::current(c).unwrap_or_else(|_| "未設定".into())
    );
    if in_maintenance(c) {
        let m: MaintenanceMode = MODE.load(c)?;
        println!(
            "保守: {} / {} から / 終了時{}",
            m.phase,
            jst(m.created),
            if m.was_active {
                "起動"
            } else {
                "停止を維持"
            }
        );
    } else {
        println!("保守: 解除済み");
    }
    let jobs = [
        "bds-daily.service".to_string(),
        "bds-update.service".to_string(),
    ]
    .into_iter()
    .chain(
        crate::discord::Action::ALL
            .iter()
            .map(|action| action.unit()),
    );
    for unit in jobs {
        let v = util::prop(&unit, "ActiveState")?;
        if !matches!(v.as_str(), "inactive" | "") {
            println!("実行中: {unit} ({v})");
        }
    }
    let deployment = c.state().join("deployment.txt");
    if deployment.exists() {
        println!(
            "配布作業の記録（未完了）: {}",
            fs::read_to_string(deployment)?
        );
    }
    if operations::TRANSACTION.exists(c) {
        let t = operations::TRANSACTION.load(c)?;
        println!(
            "操作記録: {} / {} / {}（中断時はrecover）",
            t.id, t.kind, t.phase
        );
    }
    if let Some(s) = snapshots::list(c)?.last() {
        println!("最後のバックアップ作成: {}", snapshot_line(s));
    } else {
        println!("バックアップ: 未作成");
    }
    let success = operations::SUCCESS.path(c);
    if success.exists() {
        println!(
            "最後のバックアップ・復帰成功: {}",
            fs::read_to_string(success)?
        );
    }
    for (label, record) in [
        ("取得済み更新", operations::PENDING_RELEASE),
        ("自動再試行しない版", operations::REJECTED_RELEASE),
    ] {
        if record.exists(c) {
            let r = record.load(c)?;
            println!("{label}: {}", r.id);
        }
    }
    for unit in ["bds-daily.timer", "bds-update.timer"] {
        let state = util::prop(unit, "ActiveState")?;
        let next = util::prop(unit, "NextElapseUSecRealtime")?;
        println!("次回 {unit}: {next} ({state})");
    }
    println!(
        "空き容量: データ {:.1} GiB / 本体 {:.1} GiB",
        util::free(&c.paths.store)? as f64 / 1073741824.0,
        util::free(&c.paths.releases)? as f64 / 1073741824.0
    );
    println!("現在の接続可否の検査は行っていません。");
    Ok(())
}
pub fn inventory(c: &Config) -> Result<()> {
    println!("バックアップ（サイズは世代ごとの論理量。同じファイルの共有分を含みます）:");
    for s in snapshots::list(c)? {
        println!("{}", snapshot_line(&s));
    }
    let failures = c.paths.store.join("failures");
    if failures.exists() {
        for e in fs::read_dir(failures)? {
            let e = e?;
            ensure!(
                e.file_type()?.is_dir(),
                "退避領域に不明なファイルがあります"
            );
            println!(
                "障害調査用: {}\t{:.1} MiB（自動削除なし）",
                e.path().display(),
                util::tree_bytes(&e.path())? as f64 / 1048576.0
            );
        }
    }
    Ok(())
}

pub fn deployment_check(c: &Config) -> Result<()> {
    ensure_available(c)?;
    ensure!(
        c.paths.store == std::path::Path::new("/var/lib/bds") && c.server.unit == "bds.service",
        "自動配布は標準の保存先・サービス名専用です"
    );
    operations::ensure_no_interruption(c)?;
    let current = releases::current(c)?;
    util::reserve(c, releases::execution_copies_bytes(c, &[&current])?)?;
    Ok(())
}
