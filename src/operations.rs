use crate::{
    config::{Config, identifier, properties},
    releases, snapshots, supervisor, util,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transaction {
    pub id: String,
    pub kind: String,
    pub phase: String,
    pub old_release: String,
    pub snapshot: Option<String>,
    pub was_active: bool,
    pub owner: u32,
    pub boot: String,
    pub owner_start: String,
}
fn transaction_path(c: &Config) -> PathBuf {
    c.state().join("transaction.json")
}
#[derive(Serialize, Deserialize)]
struct InitialPlacement {
    release: String,
    stage: String,
}
fn placement_path(c: &Config) -> PathBuf {
    c.state().join("import.json")
}
fn ensure_placement_complete(c: &Config) -> Result<()> {
    ensure!(
        !placement_path(c).exists(),
        "初期化またはデータの取り込みが中断しています。停止状態でrecoverを実行してください"
    );
    Ok(())
}
pub(crate) fn finish_placement(c: &Config) -> Result<()> {
    let tx: InitialPlacement = util::read_json(&placement_path(c))?;
    identifier(&tx.release)?;
    identifier(&tx.stage)?;
    ensure!(tx.stage.starts_with(".import-"), "初回配置の記録が不正です");
    let stage = c.paths.store.join(&tx.stage);
    match fs::symlink_metadata(c.current()) {
        Ok(_) => ensure!(
            releases::current(c)? == tx.release,
            "初回配置の記録と現在のBDS版が異なります"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    ensure!(
        !(stage.exists() && c.data().exists()),
        "取り込み元と配置先が両方存在します。管理者が確認してください"
    );
    if !c.data().exists() {
        util::files(&stage)?;
        fs::rename(&stage, c.data())?;
        fs::File::open(&c.paths.store)?.sync_all()?;
    }
    releases::prepare(c, &tx.release)?;
    chown_data(c)?;
    releases::activate(c, &tx.release)?;
    fs::remove_file(placement_path(c))?;
    fs::File::open(c.state())?.sync_all()?;
    Ok(())
}
fn tx_save(c: &Config, t: &Transaction) -> Result<()> {
    util::json(&transaction_path(c), t, 0o600)
}
fn tx_clear(c: &Config) -> Result<()> {
    if transaction_path(c).exists() {
        fs::remove_file(transaction_path(c))?;
        fs::File::open(c.state())?.sync_all()?;
    }
    Ok(())
}
pub fn active(c: &Config) -> Result<bool> {
    let state = util::prop(&c.server.unit, "ActiveState")?;
    ensure!(
        matches!(state.as_str(), "active" | "inactive" | "failed"),
        "起動・停止の途中です: {state}"
    );
    Ok(state == "active")
}
pub fn ensure_stopped(c: &Config) -> Result<()> {
    let state = util::prop(&c.server.unit, "ActiveState")?;
    ensure!(
        matches!(state.as_str(), "inactive" | "failed"),
        "BDSの完全停止を確認できません: {state}"
    );
    ensure!(
        util::prop(&c.server.unit, "MainPID")? == "0",
        "BDSのプロセスが残っています"
    );
    let cg = util::prop(&c.server.unit, "ControlGroup")?;
    if !cg.is_empty() {
        let p = PathBuf::from(format!("/sys/fs/cgroup{cg}/cgroup.procs"));
        if p.exists() {
            ensure!(
                fs::read_to_string(p)?.trim().is_empty(),
                "BDSの子プロセスが残っています"
            );
        }
    }
    Ok(())
}
/// A stopped process alone is not proof that its world was saved normally.
pub fn ensure_clean_backup_source(c: &Config) -> Result<()> {
    ensure_stopped(c)?;
    ensure!(
        util::prop(&c.server.unit, "ActiveState")? == "inactive",
        "異常停止中のデータを通常バックアップには保存しません。診断記録を確認してください"
    );
    let s = supervisor::status(c)
        .context("正常終了記録がありません。通常バックアップには起動・正常停止の確認が必要です")?;
    ensure!(
        s.stopped
            && !s.failed
            && s.child_exit == Some(0)
            && s.boot == util::boot()
            && !s.invocation.is_empty(),
        "正常終了を確認できないデータを通常バックアップには保存しません。診断記録を確認してください"
    );
    Ok(())
}
pub fn stop(c: &Config) -> Result<()> {
    let invocation = util::prop(&c.server.unit, "InvocationID")?;
    util::systemctl(&["stop", &c.server.unit])?;
    ensure_stopped(c)?;
    let s = supervisor::status(c)?;
    ensure!(
        !invocation.is_empty()
            && s.invocation == invocation
            && s.stopped
            && !s.failed
            && s.child_exit == Some(0),
        "今回のBDSの正常終了記録がありません"
    );
    Ok(())
}
#[derive(Serialize, Deserialize)]
struct StartPermit {
    owner: u32,
    owner_start: String,
    boot: String,
}
fn valid_permit(c: &Config) -> bool {
    util::read_json::<StartPermit>(&c.state().join("start-permit.json")).is_ok_and(|p| {
        p.boot == util::boot()
            && !p.owner_start.is_empty()
            && util::process_start(p.owner).as_deref() == Some(&p.owner_start)
    })
}
fn no_pending_reboot(c: &Config) -> Result<()> {
    let p = c.state().join("reboot-pending.json");
    if p.exists() {
        let boot: String = util::read_json(&p)?;
        ensure!(
            boot != util::boot(),
            "OS再起動要求済みです。再起動を待つか、管理者が状態を確認してください"
        );
        fs::remove_file(p)?;
    }
    Ok(())
}
pub fn start(c: &Config) -> Result<()> {
    crate::admin::ensure_available(c)?;
    ensure_placement_complete(c)?;
    let p = c.state().join("start-permit.json");
    util::json(
        &p,
        &StartPermit {
            owner: std::process::id(),
            owner_start: util::process_start(std::process::id()).unwrap_or_default(),
            boot: util::boot(),
        },
        0o600,
    )?;
    let result = start_wait(c);
    let _ = fs::remove_file(&p);
    result
}
fn start_wait(c: &Config) -> Result<()> {
    // systemctl waits for recovery too; only BDS retries use the deadline below.
    let _ = util::systemctl(&["start", &c.server.unit]);
    let deadline = Instant::now()
        + Duration::from_secs(
            (c.server.startup_seconds + c.server.stable_seconds + 60 + c.server.restart_seconds)
                * c.server.start_limit as u64
                + 30,
        );
    while Instant::now() < deadline {
        let state = util::prop(&c.server.unit, "ActiveState")?;
        if state == "active" {
            ensure!(supervisor::running(c), "サービスと起動記録が一致しません");
            return Ok(());
        }
        if matches!(state.as_str(), "inactive" | "failed") {
            bail!("BDSを起動できません。診断記録を確認してください")
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    bail!("起動確認が期限を超えました")
}
pub fn maintenance_notice(c: &Config, message: &str) -> Result<()> {
    notice(c, message)
}
fn notice(c: &Config, message: &str) -> Result<()> {
    if active(c)? {
        let n = c.maintenance.notice_seconds;
        supervisor::command(
            c,
            &format!("say {message}のため{n}秒後にサーバーを停止します。"),
        )?;
        if n > 10 {
            std::thread::sleep(Duration::from_secs(n - 10));
            supervisor::command(c, "say 10秒後にサーバーを停止します。")?;
            std::thread::sleep(Duration::from_secs(10));
        } else {
            std::thread::sleep(Duration::from_secs(n));
        }
    }
    Ok(())
}
fn new_transaction(c: &Config, kind: &str, was_active: bool) -> Result<Transaction> {
    ensure!(
        !transaction_path(c).exists(),
        "中断された操作を先にrecoverしてください"
    );
    let t = Transaction {
        id: util::id()?,
        kind: kind.into(),
        phase: "stopping".into(),
        old_release: releases::current(c)?,
        snapshot: None,
        was_active,
        owner: std::process::id(),
        boot: util::boot(),
        owner_start: util::process_start(std::process::id()).unwrap_or_default(),
    };
    tx_save(c, &t)?;
    Ok(t)
}
pub fn chown_data(c: &Config) -> Result<()> {
    util::run(
        "chown",
        &[
            "-hR",
            &format!("{}:{}", c.server.user, c.server.user),
            c.data().to_str().context("パスが不正です")?,
        ],
    )?;
    util::sync_tree(&c.data())
}
fn replace_data(c: &Config, prepared: &Path, label: &str) -> Result<PathBuf> {
    identifier(label)?;
    let quarantine = c.paths.store.join("failures").join(label);
    fs::create_dir_all(quarantine.parent().unwrap())?;
    ensure!(!quarantine.exists(), "退避先が存在します");
    if c.data().exists() {
        fs::rename(c.data(), &quarantine)?;
    }
    fs::rename(prepared, c.data())?;
    fs::File::open(&c.paths.store)?.sync_all()?;
    fs::File::open(quarantine.parent().unwrap())?.sync_all()?;
    chown_data(c)?;
    Ok(quarantine)
}
fn rollback(c: &Config, t: &Transaction) -> Result<()> {
    util::systemctl(&["stop", &c.server.unit])?;
    ensure_stopped(c)?;
    if let Some(id) = &t.snapshot {
        let prepared = snapshots::prepare_restore(c, id)?;
        replace_data(
            c,
            &prepared,
            &format!("{}-failed-{}", t.id, &util::nonce()?[..8]),
        )?;
    }
    releases::prepare(c, &t.old_release)?;
    releases::activate(c, &t.old_release)?;
    util::systemctl(&["reset-failed", &c.server.unit])?;
    let mut restored = t.clone();
    restored.phase = "copied".into();
    tx_save(c, &restored)?;
    if t.was_active {
        start(c)?;
    }
    tx_clear(c)
}
/// Manual recovery may only modify a fully stopped server.
pub fn recover(c: &Config) -> Result<()> {
    let _lock = util::Lock::acquire(&c.paths.store.join("maintenance.lock"), 0)?;
    ensure_stopped(c)?;
    recover_without_bds(c)
}

/// ExecStartPre runs inside an activating unit, so it has a different safety
/// condition from manual recovery: this process must be the only unit process.
pub fn start_check(c: &Config) -> Result<()> {
    crate::admin::ensure_available(c)?;
    ensure_starting_without_bds(c)?;
    if valid_permit(c) {
        // The maintenance owner holds the lock and has already prepared its data.
        no_pending_reboot(c)?;
        ensure_placement_complete(c)?;
        if transaction_path(c).exists() {
            let t: Transaction = util::read_json(&transaction_path(c))?;
            ensure!(
                transaction_running(&t)
                    && matches!(t.phase.as_str(), "copied" | "applying" | "restoring"),
                "操作中の停止期間には起動できません"
            );
        }
        prepare_current(c)
    } else {
        let _lock = util::Lock::acquire(&c.paths.store.join("maintenance.lock"), 0)?;
        recover_without_bds(c)
    }
}

fn ensure_starting_without_bds(c: &Config) -> Result<()> {
    let pid = std::process::id().to_string();
    ensure!(
        util::prop(&c.server.unit, "ActiveState")? == "activating"
            && util::prop(&c.server.unit, "MainPID")? == "0"
            && util::prop(&c.server.unit, "ControlPID")? == pid,
        "start-checkはBDS起動前のサービス処理専用です。手動復旧には停止後にrecoverを使用してください"
    );
    let group = util::prop(&c.server.unit, "ControlGroup")?;
    ensure!(!group.is_empty(), "起動前のプロセス領域を確認できません");
    let processes = fs::read_to_string(format!("/sys/fs/cgroup{group}/cgroup.procs"))?;
    ensure!(
        processes.lines().any(|p| p == pid) && processes.lines().all(|p| p == pid),
        "起動前処理以外のプロセスがサービス内に残っています"
    );
    Ok(())
}

fn transaction_running(t: &Transaction) -> bool {
    t.boot == util::boot()
        && !t.owner_start.is_empty()
        && util::process_start(t.owner).as_deref() == Some(&t.owner_start)
}

/// The caller holds the maintenance lock and has excluded a live BDS process.
fn recover_without_bds(c: &Config) -> Result<()> {
    no_pending_reboot(c)?;
    if placement_path(c).exists() {
        ensure!(
            !transaction_path(c).exists(),
            "取り込みと操作の記録が競合しています"
        );
        finish_placement(c)?;
    }
    if transaction_path(c).exists() {
        let t: Transaction = util::read_json(&transaction_path(c))
            .context("中断記録を読めません。復旧を中止します")?;
        ensure!(!transaction_running(&t), "操作の実行者がまだ動いています");
        if matches!(t.phase.as_str(), "applying" | "restoring") {
            let id = t.snapshot.as_ref().context("復旧元の記録がありません")?;
            let prepared = snapshots::prepare_restore(c, id)?;
            replace_data(
                c,
                &prepared,
                &format!("{}-recovery-{}", t.id, &util::nonce()?[..8]),
            )?;
            releases::activate(c, &t.old_release)?;
        }
        tx_clear(c)?;
    }
    cleanup_staging(c)?;
    prepare_current(c)
}

fn cleanup_staging(c: &Config) -> Result<()> {
    for (root, prefixes) in [
        (
            c.paths.store.clone(),
            vec![".restore-", ".import-", ".legacy-", ".recovery-"],
        ),
        (c.snapshots(), vec![".partial-"]),
        (c.paths.releases.clone(), vec![".recovery-"]),
    ] {
        if !root.is_dir() {
            continue;
        }
        for e in fs::read_dir(root)? {
            let e = e?;
            if prefixes
                .iter()
                .any(|p| e.file_name().to_string_lossy().starts_with(p))
            {
                ensure!(e.file_type()?.is_dir(), "不明な作業用ファイルがあります");
                fs::remove_dir_all(e.path())?;
            }
        }
    }
    Ok(())
}

fn prepare_current(c: &Config) -> Result<()> {
    match fs::symlink_metadata(c.current()) {
        Ok(_) => releases::prepare(c, &releases::current(c)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            ensure!(
                !c.data().exists(),
                "データがあるのに現在のBDS版と取り込み記録がありません。管理者が配置を確認してください"
            );
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

pub fn check(c: &Config) -> Result<Option<releases::Release>> {
    ensure_placement_complete(c)?;
    let (id, url) = releases::latest()?;
    check_release(c, &id, &url)
}
fn check_release(c: &Config, id: &str, url: &str) -> Result<Option<releases::Release>> {
    if util::read_json::<releases::Release>(&c.state().join("rejected-release.json"))
        .is_ok_and(|r| r.id == id)
    {
        return Ok(None);
    }
    if !releases::newer(id, &releases::current(c)?)? {
        return Ok(None);
    }
    let r = releases::download(c, id, url)?;
    util::json(&c.state().join("pending-release.json"), &r, 0o644)?;
    Ok(Some(r))
}
pub fn auto_update(c: &Config) -> Result<Option<snapshots::Snapshot>> {
    auto_update_with(c, releases::latest)
}
/// The version source is supplied separately so acceptance tests can exercise
/// the real maintenance workflow with cached releases and no external downloads.
pub fn auto_update_with(
    c: &Config,
    latest: impl FnOnce() -> Result<(String, String)>,
) -> Result<Option<snapshots::Snapshot>> {
    let _lock = util::Lock::acquire(
        &c.paths.store.join("maintenance.lock"),
        c.maintenance.lock_wait_seconds,
    )?;
    if crate::admin::in_maintenance(c) {
        eprintln!("保守中のため更新確認をスキップします");
        return Ok(None);
    }
    maintenance_preflight(c)?;
    let (id, url) = latest()?;
    let Some(candidate) = check_release(c, &id, &url)? else {
        return Ok(None);
    };
    // A timer must not undo a manual stop or interfere with startup/shutdown.
    if util::prop(&c.server.unit, "ActiveState")? != "active" {
        eprintln!(
            "更新版{}を取得しました。サーバーが稼働していないため適用を保留します",
            candidate.id
        );
        return Ok(None);
    }
    maintenance_locked(c, Some(candidate), "update").map(Some)
}
fn maintenance_preflight(c: &Config) -> Result<()> {
    crate::admin::ensure_available(c)?;
    no_pending_reboot(c)?;
    ensure_placement_complete(c)?;
    ensure!(
        !transaction_path(c).exists(),
        "中断記録があります。recoverを実行してください"
    );
    Ok(())
}
pub fn maintenance(c: &Config, update: bool, daily: bool) -> Result<snapshots::Snapshot> {
    let _lock = util::Lock::acquire(
        &c.paths.store.join("maintenance.lock"),
        if daily {
            c.maintenance.lock_wait_seconds
        } else {
            0
        },
    )?;
    maintenance_preflight(c)?;
    let candidate = if update {
        let rejected = c.state().join("rejected-release.json");
        if rejected.exists() {
            fs::remove_file(rejected)?;
        }
        check(c)?
    } else {
        None
    };
    maintenance_locked(
        c,
        candidate,
        if update {
            "update"
        } else if daily {
            "daily"
        } else {
            "manual"
        },
    )
}
fn maintenance_locked(
    c: &Config,
    candidate: Option<releases::Release>,
    reason: &str,
) -> Result<snapshots::Snapshot> {
    let was_active = active(c)?;
    if !was_active {
        ensure_clean_backup_source(c)?;
    }
    capacity_preflight(c, candidate.as_ref())?;
    notice(
        c,
        if candidate.is_some() {
            "更新・バックアップ"
        } else {
            "バックアップ"
        },
    )?;
    let mut tx = new_transaction(c, "maintenance", was_active)?;
    let result = (|| {
        if was_active {
            stop(c)?;
        }
        if candidate.is_some() {
            util::reserve(
                c,
                util::tree_bytes(&c.data())?
                    .checked_mul(2)
                    .context("必要容量が大きすぎます")?,
            )?;
        }
        let snapshot = snapshots::capture_reason(c, &tx.old_release, reason)?;
        tx.snapshot = Some(snapshot.id.clone());
        tx.phase = "copied".into();
        tx_save(c, &tx)?;
        if was_active {
            if let Some(ref release) = candidate {
                identifier(&release.id)?;
                ensure!(
                    c.paths
                        .releases
                        .join(&release.id)
                        .join("release.json")
                        .is_file(),
                    "更新候補がありません"
                );
                tx.phase = "applying".into();
                tx_save(c, &tx)?;
                releases::prepare(c, &release.id)?;
                releases::activate(c, &release.id)?;
            }
            start(c)?;
            if candidate.is_some() {
                fs::remove_file(c.state().join("pending-release.json"))?;
                fs::File::open(c.state())?.sync_all()?;
            }
        }
        tx_clear(c)?;
        Ok(snapshot)
    })();
    match result {
        Ok(s) => {
            util::json(
                &c.state().join("maintenance-success.json"),
                &serde_json::json!({"at": crate::admin::jst(util::now()), "snapshot": s.id, "reason": s.reason}),
                0o644,
            )?;
            if let Err(error) = snapshots::prune(c).and_then(|_| releases::prune(c, &[])) {
                eprintln!("世代整理に失敗しました: {error:#}");
                let _ = util::webhook(
                    &c.paths.webhook_file,
                    "バックアップ・起動は完了しましたが、古い世代の整理に失敗しました。",
                );
            }
            let _ = util::webhook(
                &c.paths.webhook_file,
                &if let Some(release) = candidate.filter(|_| was_active) {
                    format!(
                        "Minecraftの更新が完了しました: {} → {}。バックアップ: {}",
                        s.release, release.id, s.id
                    )
                } else {
                    format!("バックアップが完了しました: {}", s.id)
                },
            );
            Ok(s)
        }
        Err(error) => {
            let failure = if tx.phase == "applying" {
                if let Some(ref rejected) = candidate {
                    let _ = util::json(&c.state().join("rejected-release.json"), rejected, 0o644);
                }
                let _ = fs::remove_file(c.state().join("pending-release.json"));
                rollback(c, &tx)
            } else {
                tx.phase = "copied".into();
                tx_save(c, &tx)?;
                let r = if was_active { start(c) } else { Ok(()) };
                if r.is_ok() {
                    tx_clear(c)?;
                }
                r
            };
            let _ = util::webhook(
                &c.paths.webhook_file,
                if failure.is_ok() {
                    "バックアップまたは更新に失敗しました。元の構成は復帰しました。"
                } else {
                    "バックアップまたは更新に失敗し、サーバーの復帰も確認できません。診断記録を確認してください。"
                },
            );
            Err(error)
        }
    }
}
pub fn restart(c: &Config) -> Result<()> {
    let _lock = util::Lock::acquire(&c.paths.store.join("maintenance.lock"), 0)?;
    crate::admin::ensure_available(c)?;
    no_pending_reboot(c)?;
    ensure_placement_complete(c)?;
    ensure!(
        !transaction_path(c).exists(),
        "中断記録を先に復旧してください"
    );
    if active(c)? {
        notice(c, "再起動")?;
        stop(c)?;
    } else {
        ensure_stopped(c)?;
    }
    start(c)
}
pub fn reboot(c: &Config) -> Result<()> {
    let _lock = util::Lock::acquire(&c.paths.store.join("maintenance.lock"), 0)?;
    crate::admin::ensure_available(c)?;
    no_pending_reboot(c)?;
    ensure_placement_complete(c)?;
    ensure!(
        !transaction_path(c).exists(),
        "中断記録を先に復旧してください"
    );
    let was_active = active(c)?;
    if was_active {
        notice(c, "OS再起動")?;
        stop(c)?;
    } else {
        ensure_stopped(c)?;
    }
    let marker = c.state().join("reboot-pending.json");
    util::json(&marker, &util::boot(), 0o600)?;
    if let Err(e) = util::systemctl(&["--no-block", "reboot"]) {
        fs::remove_file(marker)?;
        if was_active {
            let _ = start(c);
        }
        return Err(e);
    }
    Ok(())
}
pub fn restore(c: &Config, id: &str) -> Result<()> {
    let _lock = util::Lock::acquire(&c.paths.store.join("maintenance.lock"), 0)?;
    no_pending_reboot(c)?;
    ensure_placement_complete(c)?;
    ensure!(!transaction_path(c).exists(), "中断記録があります");
    let target = snapshots::load(c, id)?;
    ensure!(
        c.paths
            .releases
            .join(&target.release)
            .join("release.json")
            .is_file(),
        "対応するBDS本体がありません"
    );
    let target_bytes = target.files.values().try_fold(0u64, |sum, f| {
        sum.checked_add(f.bytes).context("サイズが不正です")
    })?;
    let current_bytes = util::estimate_tree_bytes(&c.data())?;
    util::reserve(
        c,
        target_bytes
            .checked_add(
                current_bytes
                    .checked_mul(2)
                    .context("必要容量が大きすぎます")?,
            )
            .context("必要容量が大きすぎます")?,
    )?;
    let prepared = snapshots::prepare_restore(c, id)?;
    let _cleanup = util::CleanupDir(prepared.clone());
    let was_active = active(c)?;
    notice(c, "復元")?;
    let mut tx = new_transaction(c, "restore", was_active)?;
    let result = (|| {
        if was_active {
            stop(c)?;
        } else {
            ensure_stopped(c)?;
        }
        util::reserve(
            c,
            util::tree_bytes(&c.data())?
                .checked_mul(2)
                .context("必要容量が大きすぎます")?,
        )?;
        let safety = snapshots::capture_reason(c, &tx.old_release, "safety")?;
        snapshots::protect(c, &safety.id, true)?;
        tx.snapshot = Some(safety.id);
        tx.phase = "restoring".into();
        tx_save(c, &tx)?;
        let quarantine = replace_data(c, &prepared, &format!("{}-before-restore", tx.id))?;
        releases::prepare(c, &target.release)?;
        releases::activate(c, &target.release)?;
        if was_active {
            start(c)?;
        }
        tx_clear(c)?;
        if quarantine.exists()
            && let Err(e) = fs::remove_dir_all(quarantine)
        {
            eprintln!("復元後の退避データは残しました: {e}");
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = rollback(c, &tx);
        return Err(e);
    }
    Ok(())
}
pub fn init(c: &Config) -> Result<()> {
    init_with(c, releases::latest)
}
/// Use the same initialization workflow with a cached release in acceptance tests.
pub fn init_with(c: &Config, latest: impl FnOnce() -> Result<(String, String)>) -> Result<()> {
    let _lock = util::Lock::acquire(&c.paths.store.join("maintenance.lock"), 0)?;
    no_pending_reboot(c)?;
    ensure_stopped(c)?;
    ensure!(
        !transaction_path(c).exists(),
        "中断した操作を先に復旧してください"
    );
    if placement_path(c).exists() {
        return finish_placement(c);
    }
    ensure_empty_destination(c)?;
    let (id, url) = latest()?;
    releases::download(c, &id, &url)?;
    let stage = c.paths.store.join(format!(".import-{}", util::nonce()?));
    fs::create_dir(&stage)?;
    publish_initial_data(c, id, util::CleanupDir(stage))
}

pub(crate) fn ensure_empty_destination(c: &Config) -> Result<()> {
    for path in [c.current(), c.data()] {
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
            Ok(_) => bail!(
                "既存データがあります。配置先を上書きしません: {}",
                path.display()
            ),
        }
    }
    Ok(())
}

pub fn import_legacy(c: &Config, source: &Path) -> Result<()> {
    let _lock = util::Lock::acquire(&c.paths.store.join("maintenance.lock"), 0)?;
    no_pending_reboot(c)?;
    ensure_stopped(c)?;
    ensure!(
        !transaction_path(c).exists(),
        "中断した操作を先に復旧してください"
    );
    if placement_path(c).exists() {
        return finish_placement(c);
    }
    ensure_empty_destination(c)?;
    ensure!(
        !source.join(".server.stdin").exists(),
        "移行元が稼働中、またはFIFOが残っています"
    );
    let temporary = c.paths.store.join(format!(".legacy-{}", util::nonce()?));
    let _cleanup = util::CleanupDir(temporary.clone());
    let directory = if source.is_dir() {
        source.to_path_buf()
    } else {
        fs::create_dir(&temporary)?;
        let file = fs::File::open(source)?;
        let reader: Box<dyn std::io::Read> = if source.extension().is_some_and(|x| x == "gz") {
            Box::new(flate2::read::GzDecoder::new(file))
        } else {
            bail!("旧バックアップはtar.gz形式にしてください")
        };
        let mut archive = tar::Archive::new(reader);
        let mut total = 0u64;
        for entry in archive.entries()? {
            let mut entry = entry?;
            let p = entry.path()?.to_path_buf();
            crate::snapshots::safe_relative(&p)?;
            ensure!(
                p.starts_with("server") || p == Path::new("metadata.json"),
                "旧バックアップの構造が不正です"
            );
            let kind = entry.header().entry_type();
            ensure!(
                kind.is_file() || kind.is_dir(),
                "リンク・特殊ファイルは移行しません"
            );
            total = total
                .checked_add(entry.size())
                .context("サイズが不正です")?;
            util::reserve(c, total)?;
            entry.unpack_in(&temporary)?;
        }
        temporary.join("server")
    };
    let result = (|| {
        let version = fs::read_to_string(directory.join(".installed-version"))?
            .trim()
            .to_string();
        let url = format!(
            "https://www.minecraft.net/bedrockdedicatedserver/bin-linux/bedrock-server-{version}.zip"
        );
        releases::download(c, &version, &url)?;
        let stage = c.paths.store.join(format!(".import-{}", util::nonce()?));
        fs::create_dir(&stage)?;
        let cleanup = util::CleanupDir(stage.clone());
        for key in releases::MUTABLE {
            let from = directory.join(key);
            if from.is_dir() {
                util::copy_tree(&from, &stage.join(key))?;
            } else if from.is_file() {
                ensure!(
                    !fs::symlink_metadata(&from)?.file_type().is_symlink(),
                    "リンクは移行できません"
                );
                fs::copy(&from, stage.join(key))?;
            }
        }
        for key in releases::PACKS {
            fs::create_dir_all(stage.join(key))?;
            let from = directory.join(key);
            let vendor_root = c.paths.releases.join(&version).join(key);
            if vendor_root.is_dir() {
                // Check every vendor pack, including packs removed in the legacy tree.
                for vendor in fs::read_dir(&vendor_root)? {
                    let vendor = vendor?;
                    releases::verify_vendor_pack(&from.join(vendor.file_name()), &vendor.path())?;
                }
            }
            if !from.exists() {
                continue;
            }
            for entry in fs::read_dir(&from)? {
                let entry = entry?;
                let vendor = c
                    .paths
                    .releases
                    .join(&version)
                    .join(key)
                    .join(entry.file_name());
                if !vendor.exists() {
                    util::copy_tree(&entry.path(), &stage.join(key).join(entry.file_name()))?;
                }
            }
        }
        let props = properties(&stage.join("server.properties"))?;
        ensure!(
            props
                .get("transport")
                .map(String::as_str)
                .unwrap_or("nethernet")
                == "nethernet",
            "旧RakNet設定を先に整理してください"
        );
        publish_initial_data(c, version, cleanup)
    })();
    if temporary.exists() {
        let _ = fs::remove_dir_all(temporary);
    }
    result
}

/// Metadata-only estimate while live; the offline copy checks capacity again.
pub fn capacity_preflight(c: &Config, candidate: Option<&releases::Release>) -> Result<()> {
    if let Some(r) = candidate {
        identifier(&r.id)?;
        let saved: releases::Release =
            util::read_json(&c.paths.releases.join(&r.id).join("release.json"))?;
        ensure!(
            saved.id == r.id
                && c.paths
                    .releases
                    .join(&r.id)
                    .join("bedrock_server")
                    .is_file(),
            "更新候補が不完全です"
        );
    }
    let bytes = util::estimate_tree_bytes(&c.data())?;
    util::reserve(
        c,
        bytes
            .checked_mul(if candidate.is_some() { 2 } else { 1 })
            .context("必要容量が大きすぎます")?,
    )
}
/// Publish initialization/import data through one durable checkpoint. The guard
/// stays responsible until the checkpoint might be visible, even on fsync failure.
pub(crate) fn publish_initial_data(
    c: &Config,
    release: String,
    stage: util::CleanupDir,
) -> Result<()> {
    util::sync_tree(&stage.0)?;
    let tx = InitialPlacement {
        release,
        stage: stage
            .0
            .file_name()
            .context("取り込み先が不正です")?
            .to_str()
            .context("パスが不正です")?
            .into(),
    };
    fs::File::open(&c.paths.store)?.sync_all()?;
    stage.persist();
    util::json(&placement_path(c), &tx, 0o600)?;
    finish_placement(c)
}

pub(crate) fn prepare_after_maintenance(c: &Config) -> Result<()> {
    maintenance_preflight_records(c)?;
    prepare_current(c)?;
    if c.data().exists() {
        chown_data(c)?;
    }
    Ok(())
}
fn maintenance_preflight_records(c: &Config) -> Result<()> {
    no_pending_reboot(c)?;
    ensure_placement_complete(c)?;
    ensure!(
        !transaction_path(c).exists(),
        "中断記録があります。先にrecoverしてください"
    );
    Ok(())
}

pub fn daily(c: &Config) -> Result<Option<snapshots::Snapshot>> {
    let _lock = util::Lock::acquire(
        &c.paths.store.join("maintenance.lock"),
        c.maintenance.lock_wait_seconds,
    )?;
    if crate::admin::in_maintenance(c) {
        println!("保守中のため日次バックアップをスキップします");
        return Ok(None);
    }
    maintenance_preflight(c)?;
    maintenance_locked(c, None, "daily").map(Some)
}
