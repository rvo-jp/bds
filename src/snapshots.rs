use crate::{
    config::{Config, identifier},
    util,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Command,
};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileRecord {
    pub bytes: u64,
    pub sha256: String,
    pub device: u64,
    pub inode: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema: u32,
    pub id: String,
    pub release: String,
    pub created: u64,
    pub protected: bool,
    pub files: BTreeMap<String, FileRecord>,
}
pub fn list(c: &Config) -> Result<Vec<Snapshot>> {
    let mut out = vec![];
    if !c.snapshots().exists() {
        return Ok(out);
    }
    for e in fs::read_dir(c.snapshots())? {
        let e = e?;
        if e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        out.push(load(c, &e.file_name().to_string_lossy())?);
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}
pub fn load(c: &Config, id: &str) -> Result<Snapshot> {
    identifier(id)?;
    let path = c.snapshots().join(id);
    ensure!(
        !fs::symlink_metadata(&path)?.file_type().is_symlink(),
        "バックアップのリンクは扱えません"
    );
    let s: Snapshot = util::read_json(&path.join("snapshot.json"))?;
    ensure!(s.schema == 1 && s.id == id, "バックアップ形式が不正です");
    identifier(&s.release)?;
    for name in s.files.keys() {
        safe_relative(Path::new(name))?;
    }
    Ok(s)
}
pub fn safe_relative(path: &Path) -> Result<()> {
    ensure!(
        !path.as_os_str().is_empty()
            && path
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
        "アーカイブ内のパスが不正です"
    );
    Ok(())
}
pub fn capture(c: &Config, release: &str) -> Result<Snapshot> {
    identifier(release)?;
    fs::create_dir_all(c.snapshots())?;
    let paths = util::files(&c.data())?;
    ensure!(!paths.is_empty(), "バックアップ対象が空です");
    let bytes = paths
        .iter()
        .map(|p| fs::metadata(c.data().join(p)).map(|m| m.len()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .sum::<u64>();
    util::reserve(c, bytes)?;
    let previous = list(c)?.pop();
    let id = util::id()?;
    let partial = c.snapshots().join(format!(".partial-{id}"));
    fs::create_dir(&partial)?;
    let dest = partial.join("data");
    fs::create_dir(&dest)?;
    let result = (|| {
        let mut rsync = Command::new("rsync");
        #[cfg(target_os = "linux")]
        rsync.arg("--fsync");
        rsync.args([
            "-a",
            "--checksum",
            "--no-owner",
            "--no-group",
            "--no-devices",
            "--no-specials",
        ]);
        if let Some(ref s) = previous {
            rsync.arg(format!(
                "--link-dest={}",
                c.snapshots().join(&s.id).join("data").display()
            ));
        }
        let status = rsync
            .arg(format!("{}/", c.data().display()))
            .arg(format!("{}/", dest.display()))
            .status()?;
        ensure!(status.success(), "バックアップのコピーが失敗しました");
        let mut files = BTreeMap::new();
        for p in util::files(&dest)? {
            let m = fs::metadata(dest.join(&p))?;
            let name = p
                .to_str()
                .context("ファイル名はUTF-8にしてください")?
                .to_string();
            let old = previous.as_ref().and_then(|s| s.files.get(&name));
            let hash = if let Some(old) =
                old.filter(|x| x.device == m.dev() && x.inode == m.ino() && x.bytes == m.len())
            {
                old.sha256.clone()
            } else {
                util::hash(&dest.join(&p))?
            };
            files.insert(
                name,
                FileRecord {
                    bytes: m.len(),
                    sha256: hash,
                    device: m.dev(),
                    inode: m.ino(),
                },
            );
        }
        let snapshot = Snapshot {
            schema: 1,
            id: id.clone(),
            release: release.into(),
            created: util::now(),
            protected: false,
            files,
        };
        util::json(&partial.join("snapshot.json"), &snapshot, 0o644)?;
        util::sync_dirs(&dest)?;
        fs::rename(&partial, c.snapshots().join(&id))?;
        fs::File::open(c.snapshots())?.sync_all()?;
        Ok(snapshot)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&partial);
    }
    result
}
pub fn verify(c: &Config, id: &str) -> Result<Snapshot> {
    let s = load(c, id)?;
    let data = c.snapshots().join(id).join("data");
    let actual = util::files(&data)?;
    ensure!(
        actual.len() == s.files.len(),
        "バックアップのファイル数が一致しません"
    );
    for p in actual {
        let name = p.to_str().context("ファイル名が不正です")?;
        let r = s.files.get(name).context("不明なファイルがあります")?;
        ensure!(
            fs::metadata(data.join(&p))?.len() == r.bytes
                && util::hash(&data.join(&p))? == r.sha256,
            "バックアップ検証に失敗しました: {name}"
        );
    }
    Ok(s)
}
pub fn prepare_restore(c: &Config, id: &str) -> Result<PathBuf> {
    let snapshot = verify(c, id)?;
    let bytes = snapshot.files.values().try_fold(0u64, |sum, f| {
        sum.checked_add(f.bytes).context("データサイズが不正です")
    })?;
    util::reserve(c, bytes)?;
    let dest = c.paths.store.join(format!(".restore-{}", util::id()?));
    if let Err(e) = util::copy_tree(&c.snapshots().join(id).join("data"), &dest) {
        let _ = fs::remove_dir_all(&dest);
        return Err(e);
    }
    Ok(dest)
}
pub fn prune(c: &Config) -> Result<()> {
    let mut eligible: Vec<_> = list(c)?.into_iter().filter(|s| !s.protected).collect();
    let n = eligible.len().saturating_sub(c.maintenance.retention);
    for s in eligible.drain(..n) {
        fs::remove_dir_all(c.snapshots().join(s.id))?;
    }
    Ok(())
}
pub fn protect(c: &Config, id: &str, value: bool) -> Result<()> {
    let mut s = load(c, id)?;
    s.protected = value;
    util::json(&c.snapshots().join(id).join("snapshot.json"), &s, 0o644)
}
pub fn export(c: &Config, id: &str, to: &Path) -> Result<()> {
    verify(c, id)?;
    ensure!(!to.exists(), "出力ファイルが存在します");
    let partial = to.with_extension(format!("partial-{}", util::nonce()?));
    let mut zstd = Command::new("zstd")
        .args(["-T1", "-1", "-q", "-o"])
        .arg(&partial)
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    let result = (|| {
        let input = zstd.stdin.take().unwrap();
        let mut tar = tar::Builder::new(input);
        tar.append_dir_all("data", c.snapshots().join(id).join("data"))?;
        tar.append_path_with_name(
            c.snapshots().join(id).join("snapshot.json"),
            "snapshot.json",
        )?;
        tar.finish()?;
        drop(tar);
        ensure!(zstd.wait()?.success(), "圧縮に失敗しました");
        fs::rename(&partial, to)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = zstd.kill();
        let _ = zstd.wait();
        let _ = fs::remove_file(&partial);
    }
    result
}
