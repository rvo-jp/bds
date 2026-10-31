//! Restore independent world files and roll back failed changes.
use super::{
    lifecycle::{active, ensure_stopped, notice, start, stop},
    state::{
        TRANSACTION, Transaction, TransactionKind, TransactionPhase, ensure_ready, new_transaction,
    },
};
use crate::{
    config::{Config, identifier},
    releases, snapshots, util,
};
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(super) fn chown_data(c: &Config) -> Result<()> {
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

pub(super) fn replace_data(c: &Config, prepared: &Path, label: &str) -> Result<PathBuf> {
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

pub(super) fn rollback(c: &Config, t: &Transaction) -> Result<()> {
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
    restored.phase = TransactionPhase::Copied;
    TRANSACTION.save(c, &restored)?;
    if t.was_active {
        start(c)?;
    }
    TRANSACTION.remove(c)
}

pub fn restore(c: &Config, id: &str) -> Result<()> {
    let _lock = crate::state::lock(c, 0)?;
    ensure_ready(c)?;
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
    let mut tx = new_transaction(c, TransactionKind::Restore, was_active)?;
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
        let safety = snapshots::capture_reason(c, &tx.old_release, snapshots::Reason::Safety)?;
        snapshots::protect(c, &safety.id, true)?;
        tx.snapshot = Some(safety.id);
        tx.phase = TransactionPhase::Restoring;
        TRANSACTION.save(c, &tx)?;
        let quarantine = replace_data(c, &prepared, &format!("{}-before-restore", tx.id))?;
        releases::prepare(c, &target.release)?;
        releases::activate(c, &target.release)?;
        if was_active {
            start(c)?;
        }
        TRANSACTION.remove(c)?;
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
