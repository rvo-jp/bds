use crate::{
    config::{Config, identifier},
    util,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Read,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};
pub const MUTABLE: &[&str] = &[
    "worlds",
    "server.properties",
    "permissions.json",
    "allowlist.json",
    "keys",
    "config",
    "development_behavior_packs",
    "development_resource_packs",
    "development_skin_packs",
];
pub const PACKS: &[&str] = &["behavior_packs", "resource_packs", "world_templates"];
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Release {
    pub id: String,
    pub url: String,
    pub sha256: String,
}
pub fn current(c: &Config) -> Result<String> {
    let p = fs::read_link(c.current()).context("現在のBDSが未設定です")?;
    let name = p
        .file_name()
        .and_then(|s| s.to_str())
        .context("現在の版が不正です")?;
    identifier(name)?;
    ensure!(
        p == Path::new(name),
        "現在の版は管理領域直下である必要があります"
    );
    Ok(name.into())
}
pub fn activate(c: &Config, id: &str) -> Result<()> {
    identifier(id)?;
    ensure!(
        c.paths.releases.join(id).join("release.json").is_file(),
        "版が存在しません"
    );
    let tmp = c
        .paths
        .releases
        .join(format!(".current-{}", util::nonce()?));
    symlink(id, &tmp)?;
    fs::rename(tmp, c.current())?;
    fs::File::open(&c.paths.releases)?.sync_all()?;
    Ok(())
}
pub fn latest() -> Result<(String, String)> {
    for api in [
        "https://net-secondary.web.minecraft-services.net/api/v1.0/download/links",
        "https://net.web.minecraft-services.net/api/v1.0/download/links",
    ] {
        if let Ok(mut r) = util::https_agent(20)
            .get(api)
            .header("User-Agent", "Mozilla/5.0 bdsctl")
            .call()
            && let Ok(value) = r.body_mut().read_json::<serde_json::Value>()
            && let Some(links) = value["result"]["links"].as_array()
        {
            for link in links {
                if link["downloadType"] == "serverBedrockLinux" {
                    let u = link["downloadUrl"].as_str().context("URLがありません")?;
                    let id = version_url(u)?;
                    return Ok((id, u.into()));
                }
            }
        }
    }
    bail!("公式の更新情報を取得できません")
}
const DOWNLOAD_PREFIX: &str =
    "https://www.minecraft.net/bedrockdedicatedserver/bin-linux/bedrock-server-";
pub fn download_url(version: &str) -> String {
    format!("{DOWNLOAD_PREFIX}{version}.zip")
}
pub fn version_url(url: &str) -> Result<String> {
    ensure!(
        url.starts_with(DOWNLOAD_PREFIX),
        "公式のLinux配布URL以外は使用しません"
    );
    let name = url.rsplit('/').next().unwrap();
    let version = name
        .strip_prefix("bedrock-server-")
        .and_then(|s| s.strip_suffix(".zip"))
        .context("配布名が不正です")?;
    ensure!(
        !version.is_empty()
            && version.split('.').count() == 4
            && version
                .split('.')
                .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())),
        "バージョンが不正です"
    );
    Ok(version.into())
}
pub fn newer(candidate: &str, current: &str) -> Result<bool> {
    fn parts(version: &str) -> Result<Vec<u64>> {
        let parts = version
            .split('.')
            .map(str::parse::<u64>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ensure!(parts.len() == 4, "BDSバージョンは4つの数値が必要です");
        Ok(parts)
    }
    Ok(parts(candidate)? > parts(current)?)
}
fn public_directories(root: &Path) -> Result<()> {
    for e in fs::read_dir(root)? {
        let e = e?;
        if e.file_type()?.is_dir() {
            public_directories(&e.path())?;
        }
    }
    util::chmod(root, 0o755)
}
pub fn extract_zip(zip_path: &Path, to: &Path) -> Result<()> {
    ensure!(!to.exists(), "展開先が存在します");
    fs::create_dir(to)?;
    let result = (|| {
        let mut zip = zip::ZipArchive::new(fs::File::open(zip_path)?)?;
        ensure!(zip.len() < 100000, "ZIPにファイルが多すぎます");
        let mut total = 0u64;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            let rel = entry.enclosed_name().context("ZIP内のパスが不正です")?;
            crate::snapshots::safe_relative(&rel)?;
            let mode = entry.unix_mode().unwrap_or(0o100644);
            ensure!(
                !matches!(mode & 0o170000, 0o120000 | 0o020000 | 0o010000 | 0o060000),
                "リンクや特殊ファイルは展開しません"
            );
            total = total
                .checked_add(entry.size())
                .context("サイズが不正です")?;
            ensure!(total < 4 * 1024 * 1024 * 1024, "展開サイズが上限を超えます");
            let target = to.join(&rel);
            if entry.is_dir() {
                fs::create_dir_all(target)?;
            } else {
                fs::create_dir_all(target.parent().unwrap())?;
                let mut f = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)?;
                std::io::copy(&mut entry, &mut f)?;
                fs::set_permissions(
                    &target,
                    fs::Permissions::from_mode(if rel == Path::new("bedrock_server") {
                        0o755
                    } else {
                        0o644
                    }),
                )?;
                f.sync_all()?;
            }
        }
        public_directories(to)?;
        ensure!(
            to.join("bedrock_server").is_file() && to.join("server.properties").is_file(),
            "BDSの必須ファイルがありません"
        );
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(to);
    }
    result
}
pub fn download(c: &Config, id: &str, url: &str) -> Result<Release> {
    ensure!(version_url(url)? == id, "版とURLが一致しません");
    fs::create_dir_all(&c.paths.releases)?;
    util::chmod(&c.paths.releases, 0o755)?;
    let target = c.paths.releases.join(id);
    if target.is_dir() {
        let r: Release = util::read_json(&target.join("release.json"))?;
        ensure!(r.id == id, "保存済み版が不正です");
        return Ok(r);
    }
    ensure!(
        util::free(&c.paths.releases)? > 3 * 1024 * 1024 * 1024,
        "本体展開用の空き容量が不足しています"
    );
    let zip_path = c
        .paths
        .releases
        .join(format!(".download-{}.zip", util::nonce()?));
    let stage = c
        .paths
        .releases
        .join(format!(".release-{}", util::nonce()?));
    let result = (|| {
        let mut r = util::https_agent(180)
            .get(url)
            .header("User-Agent", "Mozilla/5.0 bdsctl")
            .call()
            .map_err(|_| anyhow::anyhow!("BDSの取得に失敗しました"))?;
        let mut f = fs::File::create(&zip_path)?;
        let n = std::io::copy(
            &mut r.body_mut().as_reader().take(1024 * 1024 * 1024 + 1),
            &mut f,
        )?;
        ensure!(n <= 1024 * 1024 * 1024, "配布ファイルが大きすぎます");
        f.sync_all()?;
        let digest = util::hash(&zip_path)?;
        extract_zip(&zip_path, &stage)?;
        let release = Release {
            id: id.into(),
            url: url.into(),
            sha256: digest,
        };
        util::json(&stage.join("release.json"), &release, 0o644)?;
        util::sync_dirs(&stage)?;
        fs::rename(&stage, &target)?;
        fs::File::open(&c.paths.releases)?.sync_all()?;
        Ok(release)
    })();
    let _ = fs::remove_file(zip_path);
    if result.is_err() {
        let _ = fs::remove_dir_all(stage);
    }
    result
}
/// A writable execution copy is disposable; data() remains the sole backup source.
pub fn working(c: &Config, id: &str) -> PathBuf {
    c.paths.store.join("engine").join(id)
}

/// Both execution copies and exports exclude links left by older installations.
pub(crate) fn vendor_files(root: &Path) -> Result<Vec<PathBuf>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        for e in fs::read_dir(dir)? {
            let e = e?;
            let path = e.path();
            let rel = path.strip_prefix(root)?;
            let first = rel.components().next().unwrap().as_os_str();
            if MUTABLE.iter().any(|s| first == *s) {
                continue;
            }
            let m = fs::symlink_metadata(&path)?;
            if m.file_type().is_symlink() {
                ensure!(
                    PACKS.iter().any(|s| rel.starts_with(s)) && rel.components().count() == 2,
                    "本体に不明なリンクがあります"
                );
            } else if m.is_dir() {
                walk(root, &path, out)?;
            } else {
                ensure!(m.is_file(), "本体に特殊ファイルがあります");
                out.push(rel.to_path_buf());
            }
        }
        Ok(())
    }
    let mut out = vec![];
    walk(root, root, &mut out)?;
    out.sort();
    Ok(out)
}

fn directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) => ensure!(
            m.is_dir() && !m.file_type().is_symlink(),
            "通常のディレクトリが必要です: {}",
            path.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path)?;
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

pub fn prepare(c: &Config, id: &str) -> Result<()> {
    identifier(id)?;
    util::files(&c.data())?;
    let release = c.paths.releases.join(id);
    ensure!(
        fs::symlink_metadata(&release)?.is_dir(),
        "版の保存先が不正です"
    );
    ensure!(release.join("release.json").is_file(), "版が存在しません");
    // A failed copy is never published. Existing execution copies retain BDS caches.
    directory(&c.paths.store.join("engine"))?;
    util::chmod(&c.paths.store.join("engine"), 0o755)?;
    let final_work = working(c, id);
    let mut unpublished = None;
    let work = if fs::symlink_metadata(&final_work)
        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
    {
        let files = vendor_files(&release)?;
        let bytes = files.iter().try_fold(0u64, |n, p| -> Result<u64> {
            n.checked_add(fs::metadata(release.join(p))?.len())
                .context("実行領域のサイズが不正です")
        })?;
        util::reserve(c, bytes)?;
        let stage = c.paths.store.join(format!(".engine-{}", util::nonce()?));
        fs::create_dir(&stage)?;
        let cleanup = util::CleanupDir(stage.clone());
        for rel in files {
            let dest = stage.join(&rel);
            fs::create_dir_all(dest.parent().unwrap())?;
            fs::copy(release.join(&rel), &dest)?;
        }
        util::sync_tree(&stage)?;
        unpublished = Some(cleanup);
        stage
    } else {
        final_work.clone()
    };
    directory(&work)?;
    crate::identity::adopt(&c.data(), &work)?;
    for name in MUTABLE {
        let source = c.data().join(name);
        if !source.exists() {
            if source.extension().is_some() {
                fs::write(
                    &source,
                    if *name == "server.properties" {
                        "server-name=Bedrock Server\nlevel-name=Bedrock level\ntransport=nethernet\nserver-port=19132\nserver-udp-ports=19132\n"
                    } else {
                        "[]\n"
                    },
                )?;
            } else {
                fs::create_dir_all(&source)?;
            }
        }
        let target = work.join(name);
        match fs::symlink_metadata(&target) {
            Ok(m) => {
                ensure!(
                    m.file_type().is_symlink() && fs::read_link(&target)? == source,
                    "実行領域に不明なデータがあります: {}",
                    target.display()
                );
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => symlink(&source, &target)?,
            Err(e) => return Err(e.into()),
        }
    }
    for name in PACKS {
        let root = work.join(name);
        directory(&root)?;
        let custom = c.data().join(name);
        fs::create_dir_all(&custom)?;
        for e in fs::read_dir(&root)? {
            let e = e?;
            if e.file_type()?.is_symlink() {
                ensure!(
                    fs::read_link(e.path())? == custom.join(e.file_name()),
                    "不明なパックのリンクです"
                );
                fs::remove_file(e.path())?;
            }
        }
        for e in fs::read_dir(&custom)? {
            let e = e?;
            ensure!(
                e.file_type()?.is_dir(),
                "独自パックは通常のディレクトリにしてください"
            );
            let dest = root.join(e.file_name());
            ensure!(
                !dest.try_exists()?,
                "公式パックと独自パックが衝突しています: {}",
                e.file_name().to_string_lossy()
            );
            symlink(e.path(), dest)?;
        }
    }
    crate::identity::prepare(&c.data())?;
    util::sync_dirs(&work)?;
    if let Some(cleanup) = unpublished {
        fs::rename(&work, &final_work)?;
        fs::File::open(final_work.parent().unwrap())?.sync_all()?;
        cleanup.persist();
    }
    Ok(())
}

pub fn prune(c: &Config, protected: &[String]) -> Result<()> {
    let mut entries = vec![];
    for e in fs::read_dir(&c.paths.releases)? {
        let e = e?;
        if !e.file_type()?.is_dir() {
            continue;
        }
        if let Ok(r) = util::read_json::<Release>(&e.path().join("release.json")) {
            entries.push(r.id)
        }
    }
    entries.sort_by_key(|id| {
        id.split('.')
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    });
    let current = current(c)?;
    let mut keep = protected.to_vec();
    keep.push(current);
    keep.extend(entries.iter().rev().take(2).cloned());
    for s in crate::snapshots::list(c)? {
        keep.push(s.release);
    }
    for id in entries {
        if !keep.contains(&id) {
            identifier(&id)?;
            let work = working(c, &id);
            if work.try_exists()? {
                ensure!(fs::symlink_metadata(&work)?.is_dir(), "実行領域が不正です");
                fs::remove_dir_all(work)?;
            }
            fs::remove_dir_all(c.paths.releases.join(id))?;
        }
    }
    Ok(())
}

/// Strict comparison is the default; only a separately verified legacy plan may allow extras.
pub fn verify_vendor_pack(legacy: &Path, vendor: &Path) -> Result<()> {
    verify_vendor_pack_with_extras(legacy, vendor, &std::collections::BTreeMap::new())
}

pub(crate) fn verify_vendor_pack_with_extras(
    legacy: &Path,
    vendor: &Path,
    extras: &std::collections::BTreeMap<PathBuf, String>,
) -> Result<()> {
    let message = "公式パックの差分を確認できません。停止前にprepare-legacyで検証し、独自変更は別パックへ分離してください";
    let actual = util::files(legacy).context(message)?;
    let expected = util::files(vendor)?;
    let mut permitted = expected.clone();
    for rel in extras.keys() {
        crate::snapshots::safe_relative(rel)?;
        ensure!(
            !expected.contains(rel),
            "現行の公式ファイルは除外できません"
        );
        permitted.push(rel.clone());
    }
    permitted.sort();
    ensure!(actual == permitted, "{message}: {}", legacy.display());
    for rel in actual {
        let digest = match extras.get(&rel) {
            Some(digest) => digest.clone(),
            None => util::hash(&vendor.join(&rel))?,
        };
        ensure!(
            util::hash(&legacy.join(&rel))? == digest,
            "{message}: {}",
            legacy.join(&rel).display()
        );
    }
    Ok(())
}
