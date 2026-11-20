//! Manual and startup recovery have separate entry guards and one recovery body.
use super::{
    lifecycle::{ensure_starting_without_bds, ensure_stopped},
    placement::{PLACEMENT, ensure_placement_complete, finish_placement},
    restore::{chown_data, replace_data},
    state::{TRANSACTION, Transaction, ensure_ready, no_pending_reboot, valid_permit},
};
use crate::{config::Config, releases, snapshots, util};
use anyhow::{Context, Result, ensure};
use std::fs;

pub fn recover(c: &Config) -> Result<()> {
    let _lock = crate::state::lock(c, 0)?;
    ensure_stopped(c)?;
    recover_without_bds(c)
}

pub fn start_check(c: &Config) -> Result<()> {
    crate::admin::ensure_available(c)?;
    ensure_starting_without_bds(c)?;
    if valid_permit(c) {
        // The maintenance owner holds the lock and has already prepared its data.
        no_pending_reboot(c)?;
        ensure_placement_complete(c)?;
        if TRANSACTION.exists(c) {
            let t: Transaction = TRANSACTION.load(c)?;
            ensure!(
                t.process.is_alive() && t.phase.permits_start(),
                "操作中の停止期間には起動できません"
            );
        }
        prepare_current(c)
    } else {
        let _lock = crate::state::lock(c, 0)?;
        recover_without_bds(c)
    }
}

/// Caller holds the operation lock and has excluded a live BDS process.
fn recover_without_bds(c: &Config) -> Result<()> {
    no_pending_reboot(c)?;
    if PLACEMENT.exists(c) {
        ensure!(
            !TRANSACTION.exists(c),
            "取り込みと操作の記録が競合しています"
        );
        finish_placement(c)?;
    }
    if TRANSACTION.exists(c) {
        let t: Transaction = TRANSACTION
            .load(c)
            .context("中断記録を読めません。復旧を中止します")?;
        ensure!(!t.process.is_alive(), "操作の実行者がまだ動いています");
        if t.phase.needs_restore() {
            let id = t.snapshot.as_ref().context("復旧元の記録がありません")?;
            let prepared = snapshots::prepare_restore(c, id)?;
            replace_data(
                c,
                &prepared,
                &format!("{}-recovery-{}", t.id, &util::nonce()?[..8]),
            )?;
            releases::activate(c, &t.old_release)?;
        }
        TRANSACTION.remove(c)?;
    }
    cleanup_staging(c)?;
    prepare_current(c)
}

fn cleanup_staging(c: &Config) -> Result<()> {
    releases::cleanup_downloads(c)?;
    for (root, prefixes) in [
        (
            c.paths.store.clone(),
            vec![
                ".restore-",
                ".import-",
                ".legacy-",
                ".recovery-",
                ".engine-",
            ],
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
        Ok(_) => {
            let id = releases::current(c)?;
            releases::prepare(c, &id)?;
            own_working(c, &id)
        }
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

pub(crate) fn prepare_after_maintenance(c: &Config) -> Result<()> {
    ensure_ready(c)?;
    prepare_current(c)?;
    if c.data().exists() {
        chown_data(c)?;
    }
    Ok(())
}

fn own_working(c: &Config, id: &str) -> Result<()> {
    // prepare may add missing top-level data paths; do not rescan/fsync the whole world.
    let owner = format!("{}:{}", c.server.user, c.server.user);
    let mut args = vec!["-h", owner.as_str()];
    let paths: Vec<_> = std::iter::once(c.data())
        .chain(
            releases::MUTABLE
                .iter()
                .chain(releases::PACKS)
                .map(|name| c.data().join(name)),
        )
        .chain(std::iter::once(
            c.data().join("keys").join(crate::identity::KEY),
        ))
        .collect();
    for path in &paths {
        args.push(path.to_str().context("データのパスが不正です")?);
    }
    util::run("chown", &args)?;
    util::run(
        "chown",
        &[
            "-hR",
            &format!("{}:{}", c.server.user, c.server.user),
            releases::working(c, id)
                .to_str()
                .context("実行領域のパスが不正です")?,
        ],
    )?;
    util::chmod(&releases::working(c, id), 0o700)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_cleans_orphan_download_staging() {
        let root =
            std::env::temp_dir().join(format!("bds-audit-staging-{}", util::nonce().unwrap()));
        let _cleanup = util::CleanupDir(root.clone());
        let mut c = Config::default();
        c.paths.store = root.join("store");
        c.paths.releases = root.join("releases");
        fs::create_dir_all(&c.paths.store).unwrap();
        fs::create_dir_all(&c.paths.releases).unwrap();
        let zip = c
            .paths
            .releases
            .join(".download-0123456789abcdef0123456789abcdef.zip");
        let dir = c
            .paths
            .releases
            .join(".release-0123456789abcdef0123456789abcdef");
        fs::write(&zip, "partial").unwrap();
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("partial"), "unfinished extraction").unwrap();
        let published = c.paths.releases.join("1.2.3.4");
        fs::create_dir(&published).unwrap();
        fs::write(published.join("bedrock_server"), "published").unwrap();
        let unrelated = c.paths.releases.join(".download-notes.zip");
        fs::write(&unrelated, "administrator notes").unwrap();
        cleanup_staging(&c).unwrap();
        assert!(
            !zip.exists() && !dir.exists(),
            "orphan archive/extraction remains after recovery"
        );
        assert_eq!(
            fs::read_to_string(&unrelated).unwrap(),
            "administrator notes"
        );
        assert_eq!(
            fs::read_to_string(published.join("bedrock_server")).unwrap(),
            "published"
        );
        let external = root.join("external");
        fs::create_dir(&external).unwrap();
        fs::write(external.join("keep"), "safe").unwrap();
        std::os::unix::fs::symlink(&external, &dir).unwrap();
        assert!(cleanup_staging(&c).is_err());
        assert_eq!(fs::read_to_string(external.join("keep")).unwrap(), "safe");
    }
}
