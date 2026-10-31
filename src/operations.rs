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
    let deadline = Instant::now()
        + Duration::from_secs(
            (c.server.startup_seconds + c.server.stable_seconds + 60 + c.server.restart_seconds)
                * c.server.start_limit as u64
                + 30,
        );
    let _ = util::systemctl(&["start", &c.server.unit]);
    while Instant::now() < deadline {
        let state = util::prop(&c.server.unit, "ActiveState")?;
        if state == "active" {
            let s = supervisor::status(c)?;
            ensure!(
                s.invocation == util::prop(&c.server.unit, "InvocationID")?
                    && supervisor::healthy(c),
                "サービスと起動記録が一致しません"
            );
            return Ok(());
        }
        if matches!(state.as_str(), "inactive" | "failed") {
            bail!("BDSを起動できません。診断記録を確認してください")
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    bail!("起動確認が期限を超えました")
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
pub fn recover(c: &Config) -> Result<()> {
    no_pending_reboot(c)?;
    let mut _guard = None;
    let path = transaction_path(c);
    if path.exists() {
        let t: Transaction =
            util::read_json(&path).context("中断記録を読めません。自動起動を中止します")?;
        let in_progress = t.boot == util::boot()
            && !t.owner_start.is_empty()
            && util::process_start(t.owner).as_deref() == Some(&t.owner_start);
        if !in_progress {
            _guard = Some(util::Lock::acquire(
                &c.paths.store.join("maintenance.lock"),
                0,
            )?);
            ensure!(
                util::prop(&c.server.unit, "MainPID")? == "0",
                "復旧前にBDSが停止している必要があります"
            );
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
        } else {
            ensure!(
                valid_permit(c) && matches!(t.phase.as_str(), "copied" | "applying" | "restoring"),
                "バックアップ用の停止中です。起動は操作の完了まで待ちます"
            );
        }
    } else if !valid_permit(c) {
        // An unrelated download/backup/restore may not race an administrator's start.
        _guard = Some(util::Lock::acquire(
            &c.paths.store.join("maintenance.lock"),
            0,
        )?);
    }
    if _guard.is_some() {
        for (root, prefixes) in [
            (
                c.paths.store.clone(),
                vec![".restore-", ".import-", ".legacy-"],
            ),
            (c.snapshots(), vec![".partial-"]),
        ] {
            if root.is_dir() {
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
        }
    }
    if let Ok(id) = releases::current(c) {
        releases::prepare(c, &id)?;
    }
    Ok(())
}
pub fn check(c: &Config) -> Result<Option<releases::Release>> {
    let (id, url) = releases::latest()?;
    if util::read_json::<releases::Release>(&c.state().join("rejected-release.json"))
        .is_ok_and(|r| r.id == id)
    {
        return Ok(None);
    }
    if releases::current(c).ok().as_deref() == Some(&id) {
        return Ok(None);
    }
    let r = releases::download(c, &id, &url)?;
    util::json(&c.state().join("pending-release.json"), &r, 0o644)?;
    Ok(Some(r))
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
    no_pending_reboot(c)?;
    ensure!(
        !transaction_path(c).exists(),
        "中断記録があります。recoverを実行してください"
    );
    let candidate = if update {
        let rejected = c.state().join("rejected-release.json");
        if rejected.exists() {
            fs::remove_file(rejected)?;
        }
        check(c)?
    } else if daily {
        util::read_json::<releases::Release>(&c.state().join("pending-release.json"))
            .ok()
            .filter(|r| releases::current(c).ok().as_deref() != Some(&r.id))
    } else {
        None
    };
    let was_active = active(c)?;
    if !was_active {
        ensure_stopped(c)?;
    }
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
        let snapshot = snapshots::capture(c, &tx.old_release)?;
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
        }
        tx_clear(c)?;
        Ok(snapshot)
    })();
    match result {
        Ok(s) => {
            if let Err(error) = snapshots::prune(c).and_then(|_| releases::prune(c, &[])) {
                eprintln!("世代整理に失敗しました: {error:#}");
                let _ = util::webhook(
                    &c.paths.webhook_file,
                    "バックアップ・起動は完了しましたが、古い世代の整理に失敗しました。",
                );
            }
            let _ = util::webhook(
                &c.paths.webhook_file,
                &format!("バックアップが完了しました: {}", s.id),
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
    no_pending_reboot(c)?;
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
    no_pending_reboot(c)?;
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
    let current_bytes = util::tree_bytes(&c.data())?;
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
        let safety = snapshots::capture(c, &tx.old_release)?;
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
    let _lock = util::Lock::acquire(&c.paths.store.join("maintenance.lock"), 0)?;
    no_pending_reboot(c)?;
    ensure!(
        !c.current().exists() && !c.data().exists(),
        "既存データがあります"
    );
    ensure_stopped(c)?;
    let (id, url) = releases::latest()?;
    releases::download(c, &id, &url)?;
    fs::create_dir_all(c.data())?;
    releases::prepare(c, &id)?;
    chown_data(c)?;
    releases::activate(c, &id)
}
pub fn import_legacy(c: &Config, source: &Path) -> Result<()> {
    let _lock = util::Lock::acquire(&c.paths.store.join("maintenance.lock"), 0)?;
    no_pending_reboot(c)?;
    ensure!(
        !c.current().exists() && !c.data().exists(),
        "移行先に既存データがあります"
    );
    ensure_stopped(c)?;
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
        let _cleanup = util::CleanupDir(stage.clone());
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
                if vendor.exists() {
                    for rel in util::files(&entry.path())? {
                        let other = vendor.join(&rel);
                        ensure!(
                            other.is_file()
                                && util::hash(&entry.path().join(&rel))? == util::hash(&other)?,
                            "公式パックの直接編集を検出しました。独自パックへ分離してから移行してください"
                        );
                    }
                } else {
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
        fs::rename(&stage, c.data())?;
        releases::prepare(c, &version)?;
        chown_data(c)?;
        releases::activate(c, &version)?;
        Ok(())
    })();
    if temporary.exists() {
        let _ = fs::remove_dir_all(temporary);
    }
    result
}
