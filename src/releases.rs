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
    path::Path,
};
pub const MUTABLE: &[&str] = &[
    "worlds",
    "server.properties",
    "permissions.json",
    "allowlist.json",
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
pub fn version_url(url: &str) -> Result<String> {
    ensure!(
        url.starts_with(
            "https://www.minecraft.net/bedrockdedicatedserver/bin-linux/bedrock-server-"
        ),
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
pub fn prepare(c: &Config, id: &str) -> Result<()> {
    identifier(id)?;
    util::files(&c.data())?; // Refuse user-created links before any privileged writes.
    let release = c.paths.releases.join(id);
    ensure!(release.join("release.json").is_file(), "版が存在しません");
    for name in MUTABLE {
        let source = c.data().join(name);
        if !source.exists() {
            if source.extension().is_some() {
                fs::create_dir_all(source.parent().unwrap())?;
                fs::write(
                    &source,
                    if *name == "server.properties" {
                        "server-name=RvoSMP\nlevel-name=RvoSMP\ntransport=nethernet\nserver-port=19132\nserver-udp-ports=19132\n"
                    } else {
                        "[]\n"
                    },
                )?;
            } else {
                fs::create_dir_all(&source)?;
            }
        }
        let target = release.join(name);
        if let Ok(m) = fs::symlink_metadata(&target) {
            if m.is_dir() && !m.file_type().is_symlink() {
                fs::remove_dir_all(&target)?;
            } else {
                fs::remove_file(&target)?;
            }
        }
        symlink(source, target)?;
    }
    for name in PACKS {
        let root = release.join(name);
        fs::create_dir_all(&root)?;
        let custom = c.data().join(name);
        fs::create_dir_all(&custom)?;
        for e in fs::read_dir(&root)? {
            let e = e?;
            if e.file_type()?.is_symlink() {
                ensure!(
                    fs::read_link(e.path())?.starts_with(&custom),
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
                !dest.exists(),
                "公式パックと独自パックが衝突しています: {}",
                e.file_name().to_string_lossy()
            );
            symlink(e.path(), dest)?;
        }
    }
    util::sync_dirs(&release)
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
            fs::remove_dir_all(c.paths.releases.join(id))?;
        }
    }
    Ok(())
}
