//! Durable operation records and shared preconditions.
use super::placement::ensure_placement_complete;
use crate::state::Record;
use crate::{config::Config, releases, util};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transaction {
    pub id: String,
    pub kind: TransactionKind,
    pub phase: TransactionPhase,
    pub old_release: String,
    pub snapshot: Option<String>,
    pub was_active: bool,
    #[serde(flatten)]
    pub process: ProcessOwner,
}

crate::model::text_enum! {
    pub enum TransactionKind {
        Maintenance => "maintenance",
        Restore => "restore",
    }
}
crate::model::text_enum! {
    pub enum TransactionPhase {
        Stopping => "stopping",
        Copied => "copied",
        Applying => "applying",
        Restoring => "restoring",
    }
}
impl TransactionPhase {
    pub(super) fn permits_start(self) -> bool {
        matches!(self, Self::Copied | Self::Applying | Self::Restoring)
    }
    pub(super) fn needs_restore(self) -> bool {
        matches!(self, Self::Applying | Self::Restoring)
    }
}
pub(crate) const TRANSACTION: Record<Transaction> = Record::new("transaction.json", 0o600);
pub(super) const START_PERMIT: Record<ProcessOwner> = Record::new("start-permit.json", 0o600);
pub(crate) const REBOOT_PENDING: Record<String> = Record::new("reboot-pending.json", 0o600);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessOwner {
    owner: u32,
    owner_start: String,
    boot: String,
}
impl ProcessOwner {
    pub(super) fn current() -> Self {
        Self {
            owner: std::process::id(),
            owner_start: util::process_start(std::process::id()).unwrap_or_default(),
            boot: util::boot(),
        }
    }
    pub(super) fn is_alive(&self) -> bool {
        self.boot == util::boot()
            && !self.owner_start.is_empty()
            && util::process_start(self.owner).as_deref() == Some(&self.owner_start)
    }
}
pub(super) fn valid_permit(c: &Config) -> bool {
    START_PERMIT.load(c).is_ok_and(|p| p.is_alive())
}

pub(super) fn no_pending_reboot(c: &Config) -> Result<()> {
    if REBOOT_PENDING.exists(c) {
        let boot = REBOOT_PENDING.load(c)?;
        ensure!(
            boot != util::boot(),
            "OS再起動要求済みです。再起動を待つか、管理者が状態を確認してください"
        );
        REBOOT_PENDING.remove(c)?;
    }
    Ok(())
}

pub(super) fn new_transaction(
    c: &Config,
    kind: TransactionKind,
    was_active: bool,
) -> Result<Transaction> {
    ensure!(
        !TRANSACTION.exists(c),
        "中断された操作を先にrecoverしてください"
    );
    let t = Transaction {
        id: util::id()?,
        kind,
        phase: TransactionPhase::Stopping,
        old_release: releases::current(c)?,
        snapshot: None,
        was_active,
        process: ProcessOwner::current(),
    };
    TRANSACTION.save(c, &t)?;
    Ok(t)
}

pub(super) fn ensure_ready(c: &Config) -> Result<()> {
    no_pending_reboot(c)?;
    ensure_placement_complete(c)?;
    ensure!(
        !TRANSACTION.exists(c),
        "中断記録があります。先にrecoverしてください"
    );
    Ok(())
}

/// Deployment and maintenance entry must not bypass unresolved durable records.
pub(crate) fn ensure_no_interruption(c: &Config) -> Result<()> {
    ensure!(
        !TRANSACTION.exists(c)
            && !super::placement::PLACEMENT.exists(c)
            && !REBOOT_PENDING.exists(c),
        "中断記録・再起動要求を先に解決してください"
    );
    Ok(())
}
