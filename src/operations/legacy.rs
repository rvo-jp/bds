//! Preflight legacy overlays while BDS is still running; revalidate after stop.
use super::{
    placement::ensure_empty_destination,
    state::{TRANSACTION, no_pending_reboot},
};
use crate::{
    config::{Config, identifier},
    releases::{self, Release},
    snapshots,
    state::Record,
    util,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
pub(super) struct LegacyPlan {
    schema: u32,
    source: PathBuf,
    current: Release,
    reference: Release,
    extras: BTreeMap<PathBuf, String>,
}
pub(super) const PLAN: Record<LegacyPlan> = Record::new("legacy-plan.json", 0o600);

pub fn prepare_legacy(c: &Config, source: &Path, reference: &str) -> Result<()> {
    let _lock = crate::state::lock(c, 0)?;
    no_pending_reboot(c)?;
    ensure_empty_destination(c)?;
    ensure!(
        !TRANSACTION.exists(c) && !super::placement::PLACEMENT.exists(c),
        "中断記録を先に復旧してください"
    );
    ensure!(
        source.is_dir(),
        "prepare-legacyには旧サーバーのディレクトリを指定してください"
    );
    let source = fs::canonicalize(source)?;
    let version = installed_version(&source)?;
    ensure!(
        releases::newer(&version, reference)?,
        "参照版には移行元より古い公式版を指定してください"
    );
    PLAN.remove(c)?;
    let current = releases::download(c, &version, &releases::download_url(&version))?;
    let reference = releases::download(c, reference, &releases::download_url(reference))?;
    let mut extras = BTreeMap::new();
    for (prefix, vendor) in vendor_packs(c, &version)? {
        let expected: BTreeSet<_> = util::files(&vendor)?.into_iter().collect();
        for rel in util::files(&source.join(&prefix))? {
            if expected.contains(&rel) {
                continue;
            }
            let name = prefix.join(&rel);
            let previous = c.paths.releases.join(&reference.id).join(&name);
            let digest = util::hash(&source.join(&name))?;
            ensure!(
                previous.is_file() && util::hash(&previous)? == digest,
                "過去の公式版と一致しない追加ファイルがあります。独自変更を保全して確認してください: {}",
                name.display()
            );
            extras.insert(name, digest);
        }
    }
    verify_packs(c, &source, &version, &extras)?;
    ensure!(
        installed_version(&source)? == version,
        "検証中に移行元の版が変わりました。再実行してください"
    );
    let count = extras.len();
    PLAN.save(
        c,
        &LegacyPlan {
            schema: 1,
            source,
            current,
            reference,
            extras,
        },
    )?;
    println!(
        "移行準備完了: BDS {version}。過去の公式版と一致する残存ファイル{count}件を確認しました。ワールドのコピー・変更・停止は行っていません。"
    );
    Ok(())
}

pub(super) fn installed_version(source: &Path) -> Result<String> {
    let version = fs::read_to_string(source.join(".installed-version"))?
        .trim()
        .to_string();
    releases::version_url(&releases::download_url(&version))?;
    Ok(version)
}

/// A plan proves exact bytes at exact paths, not permission to ignore arbitrary additions.
pub(super) fn verify_source(c: &Config, source: &Path, version: &str) -> Result<()> {
    let extras = if PLAN.exists(c) {
        let plan = PLAN.load(c)?;
        ensure!(
            plan.schema == 1
                && plan.source == fs::canonicalize(source)?
                && plan.current.id == version,
            "移行準備の記録と移行元が異なります。prepare-legacyを再実行してください"
        );
        let cached: Release = util::read_json(&c.paths.releases.join(version).join("release.json"))
            .context("停止前に取得した本体がありません。prepare-legacyを再実行してください")?;
        ensure!(
            cached.id == plan.current.id && cached.sha256 == plan.current.sha256,
            "準備時と本体が異なります。prepare-legacyを再実行してください"
        );
        ensure!(
            releases::newer(version, &plan.reference.id)?,
            "参照版の記録が不正です"
        );
        plan.extras
    } else {
        releases::download(c, version, &releases::download_url(version))?;
        BTreeMap::new()
    };
    verify_packs(c, source, version, &extras)
}

fn vendor_packs(c: &Config, version: &str) -> Result<Vec<(PathBuf, PathBuf)>> {
    identifier(version)?;
    let mut packs = Vec::new();
    for key in releases::PACKS {
        let root = c.paths.releases.join(version).join(key);
        if !root.exists() {
            continue;
        }
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            ensure!(
                entry.file_type()?.is_dir(),
                "公式パックの配置が不正です: {}",
                entry.path().display()
            );
            packs.push((Path::new(key).join(entry.file_name()), entry.path()));
        }
    }
    packs.sort();
    Ok(packs)
}

fn verify_packs(
    c: &Config,
    source: &Path,
    version: &str,
    extras: &BTreeMap<PathBuf, String>,
) -> Result<()> {
    let mut used = BTreeSet::new();
    for (prefix, vendor) in vendor_packs(c, version)? {
        let mut permitted = BTreeMap::new();
        for (path, digest) in extras {
            snapshots::safe_relative(path)?;
            if let Ok(rel) = path.strip_prefix(&prefix) {
                permitted.insert(rel.to_path_buf(), digest.clone());
                used.insert(path);
            }
        }
        releases::verify_vendor_pack_with_extras(&source.join(prefix), &vendor, &permitted)?;
    }
    ensure!(
        used.len() == extras.len(),
        "移行準備の記録に不明な除外対象があります"
    );
    Ok(())
}
