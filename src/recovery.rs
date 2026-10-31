//! Portable disaster recovery: world, exact BDS binary, custom packs and public config.
use crate::{config::Config, operations, releases, snapshots, util};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
#[derive(Serialize, Deserialize)]
struct Manifest {
    schema: u32,
    files: BTreeMap<String, String>,
}
/// Prepared releases contain links into live data. Export only immutable vendor files.
fn vendor_files(root: &Path) -> Result<Vec<PathBuf>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        for e in fs::read_dir(dir)? {
            let e = e?;
            let path = e.path();
            let rel = path.strip_prefix(root)?;
            let first = rel.components().next().unwrap().as_os_str();
            if releases::MUTABLE.iter().any(|s| first == *s) {
                continue;
            }
            let m = fs::symlink_metadata(&path)?;
            if m.file_type().is_symlink() {
                ensure!(
                    releases::PACKS.iter().any(|s| rel.starts_with(s))
                        && rel.components().count() == 2,
                    "本体に不明なリンクがあります"
                );
                continue;
            }
            if m.is_dir() {
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
pub fn export(c: &Config, id: &str, to: &Path) -> Result<()> {
    let s = snapshots::verify(c, id)?;
    ensure!(!to.exists(), "出力ファイルが存在します");
    let release = c.paths.releases.join(&s.release);
    let r: releases::Release = util::read_json(&release.join("release.json"))?;
    ensure!(
        r.id == s.release && release.join("bedrock_server").is_file(),
        "対応するBDS本体がありません"
    );
    let partial = to.with_extension(format!("partial-{}", util::nonce()?));
    // Backup contents are private even under a permissive administrator umask.
    let output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&partial)?;
    let mut zstd = Command::new("zstd")
        .args(["-T1", "-1", "-q", "-c"])
        .stdin(Stdio::piped())
        .stdout(output)
        .spawn()?;
    let result = (|| {
        let mut tar = tar::Builder::new(zstd.stdin.take().context("圧縮入力がありません")?);
        tar.follow_symlinks(false);
        let mut files = BTreeMap::new();
        for (prefix, root, paths) in [
            (
                "data",
                c.snapshots().join(id).join("data"),
                util::files(&c.snapshots().join(id).join("data"))?,
            ),
            ("release", release.clone(), vendor_files(&release)?),
        ] {
            // Preserve empty data directories too; release directories are rebuilt by prepare.
            if prefix == "data" {
                tar.append_dir_all("data", &root)?;
            }
            for rel in paths {
                let name = format!("{prefix}/{}", rel.to_str().context("ファイル名が不正です")?);
                files.insert(name.clone(), util::hash(&root.join(&rel))?);
                if prefix != "data" {
                    tar.append_path_with_name(root.join(&rel), name)?;
                }
            }
        }
        let config = toml::to_string_pretty(c)?;
        let snapshot = serde_json::to_vec_pretty(&s)?;
        for (name, bytes) in [
            ("config.toml", config.as_bytes()),
            ("snapshot.json", snapshot.as_slice()),
        ] {
            use sha2::{Digest, Sha256};
            files.insert(name.into(), hex::encode(Sha256::digest(bytes)));
            append_bytes(&mut tar, name, bytes)?;
        }
        append_bytes(
            &mut tar,
            "recovery.json",
            &serde_json::to_vec_pretty(&Manifest { schema: 1, files })?,
        )?;
        tar.finish()?;
        drop(tar);
        ensure!(zstd.wait()?.success(), "圧縮に失敗しました");
        fs::File::open(&partial)?.sync_all()?;
        // Never replace an existing export, including one created during compression.
        fs::hard_link(&partial, to)?;
        fs::remove_file(&partial)?;
        fs::File::open(
            to.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?
        .sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = zstd.kill();
        let _ = zstd.wait();
        let _ = fs::remove_file(&partial);
    }
    result
}
fn append_bytes<W: std::io::Write>(
    tar: &mut tar::Builder<W>,
    name: &str,
    bytes: &[u8],
) -> Result<()> {
    let mut h = tar::Header::new_gnu();
    h.set_size(bytes.len() as u64);
    h.set_mode(0o600);
    h.set_cksum();
    tar.append_data(&mut h, name, bytes)?;
    Ok(())
}
/// Extract only regular files/directories; do not trust permissions, owners or links.
fn extract(c: &Config, source: &Path, dest: &Path) -> Result<()> {
    let mut decoder = Command::new("zstd")
        .args(["-d", "-q", "-c"])
        .arg("--")
        .arg(source)
        .stdout(Stdio::piped())
        .spawn()?;
    let result = (|| {
        let mut tar = tar::Archive::new(decoder.stdout.take().context("展開入力がありません")?);
        let mut seen = BTreeSet::new();
        let mut total = 0u64;
        for entry in tar.entries()? {
            let mut e = entry?;
            let p = e.path()?.to_path_buf();
            snapshots::safe_relative(&p)?;
            ensure!(
                seen.len() < 1_000_000 && seen.insert(p.clone()),
                "アーカイブに重複または過剰な項目があります"
            );
            let allowed = p.starts_with("data")
                || p.starts_with("release")
                || ["snapshot.json", "config.toml", "recovery.json"]
                    .iter()
                    .any(|n| p == Path::new(n));
            ensure!(allowed, "アーカイブに不明な項目があります");
            let kind = e.header().entry_type();
            ensure!(
                kind.is_dir() || kind.is_file(),
                "リンク・特殊ファイルは取り込めません"
            );
            let target = dest.join(&p);
            if kind.is_dir() {
                fs::create_dir_all(&target)?;
            } else {
                total = total
                    .checked_add(e.size())
                    .context("展開サイズが不正です")?;
                // Previously written bytes are already reflected in available space.
                util::reserve(c, e.size())?;
                fs::create_dir_all(target.parent().unwrap())?;
                let mut f = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&target)?;
                std::io::copy(&mut e, &mut f)?;
                f.sync_all()?;
            }
        }
        // Consume the decoder through EOF so truncation/trailing frame errors cannot be hidden.
        std::io::copy(&mut tar.into_inner(), &mut std::io::sink())?;
        ensure!(
            decoder.wait()?.success(),
            "アーカイブの圧縮データが壊れています"
        );
        Ok(())
    })();
    if result.is_err() {
        let _ = decoder.kill();
        let _ = decoder.wait();
    }
    result
}
pub fn import(c: &Config, source: &Path) -> Result<()> {
    let _lock = crate::state::lock(c, 0)?;
    operations::ensure_stopped(c)?;
    operations::ensure_empty_destination(c)?;
    ensure!(
        !operations::TRANSACTION.exists(c) && !operations::PLACEMENT.exists(c),
        "中断記録を先にrecoverしてください"
    );
    let work = c.paths.store.join(format!(".recovery-{}", util::nonce()?));
    fs::create_dir(&work)?;
    util::chmod(&work, 0o700)?;
    let _cleanup = util::CleanupDir(work.clone());
    extract(c, source, &work)?;
    let manifest: Manifest = util::read_json(&work.join("recovery.json"))?;
    ensure!(manifest.schema == 1, "復元形式が不正です");
    let actual = util::files(&work)?;
    ensure!(
        actual.len() == manifest.files.len() + 1,
        "復元ファイル一覧が一致しません"
    );
    for (name, hash) in &manifest.files {
        snapshots::safe_relative(Path::new(name))?;
        ensure!(
            util::hash(&work.join(name))? == *hash,
            "復元データが壊れています: {name}"
        );
    }
    let s: snapshots::Snapshot = util::read_json(&work.join("snapshot.json"))?;
    ensure!(s.schema == 1, "スナップショット形式が不正です");
    crate::config::identifier(&s.release)?;
    ensure!(
        util::files(&work.join("data"))?.len() == s.files.len(),
        "ワールドのファイル数が一致しません"
    );
    for (name, f) in &s.files {
        snapshots::safe_relative(Path::new(name))?;
        ensure!(
            fs::metadata(work.join("data").join(name))?.len() == f.bytes
                && util::hash(&work.join("data").join(name))? == f.sha256,
            "ワールドの検証に失敗しました"
        );
    }
    let r: releases::Release = util::read_json(&work.join("release/release.json"))?;
    ensure!(
        r.id == s.release
            && work.join("release/bedrock_server").is_file()
            && work.join("data/server.properties").is_file(),
        "復元に必要な本体・設定がありません"
    );
    // Read/validate the old public configuration, but retain destination-specific paths/IDs.
    Config::load(&work.join("config.toml"))?;
    util::reserve(c, util::tree_bytes(&work.join("data"))?)?;
    let final_release = c.paths.releases.join(&s.release);
    if final_release.exists() {
        let paths = vendor_files(&final_release)?;
        ensure!(
            paths.len() == util::files(&work.join("release"))?.len(),
            "既存の本体と復元用本体が異なります"
        );
        for p in paths {
            ensure!(
                util::hash(&final_release.join(&p))? == util::hash(&work.join("release").join(&p))?,
                "既存の本体が異なります"
            );
        }
    } else {
        ensure!(
            util::free(&c.paths.releases)? > util::tree_bytes(&work.join("release"))? + 1048576,
            "本体配置先の容量が不足しています"
        );
        let stage = c
            .paths
            .releases
            .join(format!(".recovery-{}", util::nonce()?));
        let _clean = util::CleanupDir(stage.clone());
        util::copy_tree(&work.join("release"), &stage)?;
        // Archive permissions are not trusted. These are immutable, public vendor files.
        for p in util::files(&stage)? {
            util::chmod(
                &stage.join(&p),
                if p == Path::new("bedrock_server") {
                    0o755
                } else {
                    0o644
                },
            )?;
        }
        public_dirs(&stage)?;
        util::sync_tree(&stage)?;
        fs::rename(&stage, &final_release)?;
        fs::File::open(&c.paths.releases)?.sync_all()?;
    }
    let data_stage = c.paths.store.join(format!(".import-{}", util::nonce()?));
    fs::rename(work.join("data"), &data_stage)?;
    // Ownership transfers to the durable import checkpoint; never delete it on a failed publish.
    operations::publish_initial_data(c, s.release, util::CleanupDir(data_stage))?;
    println!(
        "ワールドと同じ版のMinecraft本体を復元しました。起動・タイマー有効化は行っていません。接続先のconfig.tomlを維持しています。Webhook・Caddy・通信許可は別途設定してください。"
    );
    Ok(())
}
fn public_dirs(root: &Path) -> Result<()> {
    for e in fs::read_dir(root)? {
        let e = e?;
        if e.file_type()?.is_dir() {
            public_dirs(&e.path())?;
        }
    }
    util::chmod(root, 0o755)
}
