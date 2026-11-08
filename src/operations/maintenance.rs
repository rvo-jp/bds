//! Offline backups and updates share one stop/copy/start workflow.
use super::{
    lifecycle::{active, ensure_clean_backup_source, notice, start, stop},
    placement::ensure_placement_complete,
    restore::rollback,
    state::{TRANSACTION, TransactionKind, TransactionPhase, new_transaction},
};
use crate::state::Record;
use crate::{
    config::{Config, identifier},
    releases, snapshots, util,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

pub(crate) const PENDING_RELEASE: Record<releases::Release> =
    Record::new("pending-release.json", 0o644);
pub(crate) const REJECTED_RELEASE: Record<releases::Release> =
    Record::new("rejected-release.json", 0o644);
pub(crate) const SUCCESS: Record<MaintenanceSuccess> =
    Record::new("maintenance-success.json", 0o644);
#[derive(Serialize, Deserialize)]
pub(crate) struct MaintenanceSuccess {
    at: String,
    snapshot: String,
    reason: snapshots::Reason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaintenanceRequest {
    Backup,
    Update,
}

pub fn check(c: &Config) -> Result<Option<releases::Release>> {
    ensure_placement_complete(c)?;
    let (id, url) = releases::latest()?;
    check_release(c, &id, &url)
}

fn check_release(c: &Config, id: &str, url: &str) -> Result<Option<releases::Release>> {
    if REJECTED_RELEASE.load(c).is_ok_and(|r| r.id == id) {
        return Ok(None);
    }
    if !releases::newer(id, &releases::current(c)?)? {
        return Ok(None);
    }
    let r = releases::download(c, id, url)?;
    PENDING_RELEASE.save(c, &r)?;
    Ok(Some(r))
}

pub fn auto_update(c: &Config) -> Result<Option<snapshots::Snapshot>> {
    auto_update_with(c, releases::latest)
}

pub fn auto_update_with(
    c: &Config,
    latest: impl FnOnce() -> Result<(String, String)>,
) -> Result<Option<snapshots::Snapshot>> {
    let _lock = crate::state::lock(c, c.maintenance.lock_wait_seconds)?;
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
    maintenance_locked(c, Some(candidate), snapshots::Reason::Update).map(Some)
}

fn maintenance_preflight(c: &Config) -> Result<()> {
    crate::admin::ensure_available(c)?;
    super::state::ensure_ready(c)
}

pub fn maintenance(c: &Config, request: MaintenanceRequest) -> Result<snapshots::Snapshot> {
    let _lock = crate::state::lock(c, 0)?;
    maintenance_preflight(c)?;
    let candidate = if request == MaintenanceRequest::Update {
        REJECTED_RELEASE.remove(c)?;
        check(c)?
    } else {
        None
    };
    let reason = match request {
        MaintenanceRequest::Backup => snapshots::Reason::Manual,
        MaintenanceRequest::Update => snapshots::Reason::Update,
    };
    maintenance_locked(c, candidate, reason)
}

fn maintenance_locked(
    c: &Config,
    candidate: Option<releases::Release>,
    reason: snapshots::Reason,
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
    let mut tx = new_transaction(c, TransactionKind::Maintenance, was_active)?;
    let mut candidate_failed_startup = false;
    let result = (|| {
        if was_active {
            stop(c)?;
        }
        reserve_maintenance(c, candidate.as_ref(), util::tree_bytes(&c.data())?)?;
        let snapshot = snapshots::capture_reason(c, &tx.old_release, reason)?;
        tx.snapshot = Some(snapshot.id.clone());
        tx.phase = TransactionPhase::Copied;
        TRANSACTION.save(c, &tx)?;
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
                releases::prepare(c, &release.id)?;
                tx.phase = TransactionPhase::Applying;
                TRANSACTION.save(c, &tx)?;
                releases::activate(c, &release.id)?;
            }
            if let Err(error) = start(c) {
                candidate_failed_startup =
                    candidate.is_some() && !error.is::<crate::supervisor::PreparationError>();
                return Err(error);
            }
            if candidate.is_some() {
                PENDING_RELEASE.remove(c)?;
            }
        }
        TRANSACTION.remove(c)?;
        Ok(snapshot)
    })();
    match result {
        Ok(s) => {
            SUCCESS.save(
                c,
                &MaintenanceSuccess {
                    at: crate::admin::jst(util::now()),
                    snapshot: s.id.clone(),
                    reason: s.reason,
                },
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
            let failure = if tx.phase == TransactionPhase::Applying {
                if let Some(ref rejected) = candidate.filter(|_| candidate_failed_startup) {
                    let _ = REJECTED_RELEASE.save(c, rejected);
                }
                if candidate_failed_startup {
                    let _ = PENDING_RELEASE.remove(c);
                }
                rollback(c, &tx)
            } else {
                tx.phase = TransactionPhase::Copied;
                TRANSACTION.save(c, &tx)?;
                let r = if was_active { start(c) } else { Ok(()) };
                if r.is_ok() {
                    TRANSACTION.remove(c)?;
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
    reserve_maintenance(c, candidate, util::estimate_tree_bytes(&c.data())?)
}

fn reserve_maintenance(
    c: &Config,
    candidate: Option<&releases::Release>,
    bytes: u64,
) -> Result<()> {
    let current = releases::current(c)?;
    let mut versions = vec![current.as_str()];
    if let Some(release) = candidate {
        versions.push(&release.id);
    }
    let copies = releases::execution_copies_bytes(c, &versions)?;
    util::reserve(
        c,
        bytes
            .checked_mul(if candidate.is_some() { 2 } else { 1 })
            .and_then(|n| n.checked_add(copies))
            .context("必要容量が大きすぎます")?,
    )
}

pub fn daily(c: &Config) -> Result<Option<snapshots::Snapshot>> {
    let _lock = crate::state::lock(c, c.maintenance.lock_wait_seconds)?;
    if crate::admin::in_maintenance(c) {
        println!("保守中のため日次バックアップをスキップします");
        return Ok(None);
    }
    maintenance_preflight(c)?;
    maintenance_locked(c, None, snapshots::Reason::Daily).map(Some)
}
