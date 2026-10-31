//! First placement of a new world or imported data, through one durable checkpoint.
use super::{
    lifecycle::ensure_stopped,
    restore::chown_data,
    state::{TRANSACTION, no_pending_reboot},
};
use crate::state::Record;
use crate::{
    config::{Config, identifier, properties},
    releases, util,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

#[derive(Serialize, Deserialize)]
pub(crate) struct InitialPlacement {
    release: String,
    stage: String,
}

pub(crate) const PLACEMENT: Record<InitialPlacement> = Record::new("import.json", 0o600);

pub(super) fn ensure_placement_complete(c: &Config) -> Result<()> {
    ensure!(
        !PLACEMENT.exists(c),
        "初期化またはデータの取り込みが中断しています。停止状態でrecoverを実行してください"
    );
    Ok(())
}

pub(crate) fn finish_placement(c: &Config) -> Result<()> {
    let tx: InitialPlacement = PLACEMENT.load(c)?;
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
    PLACEMENT.remove(c)?;
    Ok(())
}

pub fn init(c: &Config) -> Result<()> {
    init_with(c, releases::latest)
}

/// Acceptance tests supply cached versions without changing the production workflow.
pub fn init_with(c: &Config, latest: impl FnOnce() -> Result<(String, String)>) -> Result<()> {
    let _lock = crate::state::lock(c, 0)?;
    no_pending_reboot(c)?;
    ensure_stopped(c)?;
    ensure!(!TRANSACTION.exists(c), "中断した操作を先に復旧してください");
    if PLACEMENT.exists(c) {
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
    let _lock = crate::state::lock(c, 0)?;
    no_pending_reboot(c)?;
    ensure_stopped(c)?;
    ensure!(!TRANSACTION.exists(c), "中断した操作を先に復旧してください");
    if PLACEMENT.exists(c) {
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
        ensure!(
            source.extension().is_some_and(|x| x == "gz"),
            "旧バックアップはtar.gz形式にしてください"
        );
        let reader = flate2::read::GzDecoder::new(file);
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
    let version = fs::read_to_string(directory.join(".installed-version"))?
        .trim()
        .to_string();
    let url = releases::download_url(&version);
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
}

/// Sync data and its parent before publishing the resumable checkpoint.
/// Preserve staging before the checkpoint write: a failed fsync may still expose it.
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
    PLACEMENT.save(c, &tx)?;
    finish_placement(c)
}
