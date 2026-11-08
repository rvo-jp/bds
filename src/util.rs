use anyhow::{Context, Result, bail, ensure};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn nonce() -> Result<String> {
    let mut b = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(hex::encode(b))
}
pub fn id() -> Result<String> {
    let time = chrono::Utc::now();
    Ok(format!(
        "{}{:09}Z-{}",
        time.format("%Y%m%dT%H%M%S"),
        time.timestamp_subsec_nanos(),
        &nonce()?[..8]
    ))
}

pub fn root() -> Result<()> {
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "この操作にはroot権限が必要です"
    );
    Ok(())
}
pub fn read_json<T: DeserializeOwned>(p: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
pub fn atomic(p: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = p.parent().context("保存先が不正です")?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".write-{}", nonce()?));
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&tmp)?;
    f.set_permissions(fs::Permissions::from_mode(mode))?;
    let result = (|| {
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, p)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}
pub fn json<T: Serialize>(p: &Path, value: &T, mode: u32) -> Result<()> {
    atomic(p, &serde_json::to_vec_pretty(value)?, mode)
}
pub fn hash(p: &Path) -> Result<String> {
    let mut f = File::open(p)?;
    let mut h = Sha256::new();
    let mut b = [0u8; 65536];
    loop {
        let n = f.read(&mut b)?;
        if n == 0 {
            break;
        }
        h.update(&b[..n]);
    }
    Ok(hex::encode(h.finalize()))
}
pub struct Lock(File);
impl Lock {
    pub fn acquire(path: &Path, wait: u64) -> Result<Self> {
        fs::create_dir_all(path.parent().unwrap())?;
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        // An orphaned rsync must retain the lock if its parent is killed.
        ensure!(
            unsafe { libc::fcntl(f.as_raw_fd(), libc::F_SETFD, 0) } == 0,
            "ロックの継承設定に失敗しました"
        );
        let until = Instant::now() + Duration::from_secs(wait);
        loop {
            if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Self(f));
            }
            let e = std::io::Error::last_os_error();
            if !matches!(e.raw_os_error(), Some(libc::EWOULDBLOCK)) {
                return Err(e.into());
            }
            if Instant::now() >= until {
                bail!("別の操作を実行中です")
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}
pub fn run(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("{program}を実行できません"))?;
    ensure!(status.success(), "{program}が失敗しました");
    Ok(())
}
pub fn systemctl(args: &[&str]) -> Result<()> {
    run("systemctl", args)
}
pub fn prop(unit: &str, key: &str) -> Result<String> {
    let o = Command::new("systemctl")
        .env("TZ", "Asia/Tokyo")
        .args(["show", unit, &format!("--property={key}"), "--value"])
        .output()?;
    ensure!(o.status.success(), "サービス情報を取得できません");
    Ok(String::from_utf8(o.stdout)?.trim().into())
}
pub fn boot() -> String {
    fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .unwrap_or_default()
        .trim()
        .into()
}
pub fn free(path: &Path) -> Result<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes())?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    ensure!(
        unsafe { libc::statvfs(c.as_ptr(), st.as_mut_ptr()) } == 0,
        "空き容量を確認できません"
    );
    let st = unsafe { st.assume_init() };
    #[allow(clippy::unnecessary_cast)]
    let bytes = st.f_bavail as u64 * st.f_frsize as u64;
    Ok(bytes)
}
pub fn files(root: &Path) -> Result<Vec<PathBuf>> {
    fn walk(base: &Path, p: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        for entry in fs::read_dir(p)? {
            let e = entry?;
            let m = fs::symlink_metadata(e.path())?;
            ensure!(
                !m.file_type().is_symlink(),
                "データ内のシンボリックリンクは扱えません: {}",
                e.path().display()
            );
            if m.is_dir() {
                walk(base, &e.path(), out)?
            } else {
                ensure!(m.is_file(), "特殊ファイルは扱えません");
                out.push(e.path().strip_prefix(base)?.to_path_buf());
            }
        }
        Ok(())
    }
    ensure!(
        fs::symlink_metadata(root)?.is_dir(),
        "対象のルートは通常のディレクトリにしてください"
    );
    let mut out = vec![];
    walk(root, root, &mut out)?;
    out.sort();
    Ok(out)
}
pub fn copy_tree(source: &Path, dest: &Path) -> Result<()> {
    ensure!(!dest.exists(), "コピー先が既に存在します");
    files(source)?;
    fn copy(source: &Path, dest: &Path) -> Result<()> {
        fs::create_dir(dest)?;
        for e in fs::read_dir(source)? {
            let e = e?;
            let m = fs::symlink_metadata(e.path())?;
            ensure!(!m.file_type().is_symlink(), "リンクはコピーしません");
            let to = dest.join(e.file_name());
            if m.is_dir() {
                copy(&e.path(), &to)?;
            } else {
                ensure!(m.is_file(), "特殊ファイルはコピーしません");
                fs::copy(e.path(), &to)?;
                File::open(&to)?.sync_all()?;
                fs::set_permissions(&to, m.permissions())?;
            }
        }
        File::open(dest)?.sync_all()?;
        Ok(())
    }
    copy(source, dest)
}

pub fn https_agent(seconds: u64) -> ureq::Agent {
    https_agent_for(Duration::from_secs(seconds))
}

fn https_agent_for(timeout: Duration) -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .https_only(true)
            .timeout_global(Some(timeout))
            .build(),
    )
}
pub fn webhook(path: &Path, text: &str) -> Result<()> {
    webhook_until(path, text, Instant::now() + Duration::from_secs(17))
}

pub(crate) fn webhook_until(path: &Path, text: &str, deadline: Instant) -> Result<()> {
    let url = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let url = url.trim();
    if url.is_empty() {
        return Ok(());
    }
    ensure!(
        url.starts_with("https://discord.com/api/webhooks/")
            || url.starts_with("https://discordapp.com/api/webhooks/"),
        "Webhook URLが不正です"
    );
    for n in 0..3 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let agent = https_agent_for(remaining.min(Duration::from_secs(5)));
        let result = agent
            .post(url)
            .send_json(serde_json::json!({"content":text,"allowed_mentions":{"parse":[]}}));
        if result.is_ok() {
            return Ok(());
        }
        if n < 2 {
            std::thread::sleep(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(1)),
            );
        }
    }
    bail!("Discord通知に失敗しました")
}
pub fn chmod(p: &Path, mode: u32) -> Result<()> {
    fs::set_permissions(p, fs::Permissions::from_mode(mode))?;
    Ok(())
}

pub fn process_start(pid: u32) -> Option<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)
        .map(str::to_owned)
}

/// Removes only a caller-generated staging directory, including on early returns.
pub struct CleanupDir(pub PathBuf);
impl CleanupDir {
    /// A durable recovery record now owns this directory's lifetime.
    pub fn persist(mut self) {
        self.0.clear();
    }
}
impl Drop for CleanupDir {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() && self.0.exists() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

/// Persist directory entries without following any links into live data.
pub fn sync_dirs(root: &Path) -> Result<()> {
    for e in fs::read_dir(root)? {
        let e = e?;
        if e.file_type()?.is_dir() {
            sync_dirs(&e.path())?;
        }
    }
    File::open(root)?.sync_all()?;
    Ok(())
}
pub fn sync_tree(root: &Path) -> Result<()> {
    for p in files(root)? {
        File::open(root.join(p))?.sync_all()?;
    }
    sync_dirs(root)
}

pub fn tree_bytes(root: &Path) -> Result<u64> {
    files(root)?.into_iter().try_fold(0u64, |sum, p| {
        sum.checked_add(fs::metadata(root.join(p))?.len())
            .context("データサイズが上限を超えます")
    })
}
/// Estimate live data using one metadata read per entry. LevelDB may remove files
/// during the scan; only NotFound is tolerated. Offline operations use tree_bytes.
pub fn estimate_tree_bytes(root: &Path) -> Result<u64> {
    fn walk(path: &Path) -> Result<u64> {
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        let mut bytes = 0u64;
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            let path = entry.path();
            let m = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            ensure!(
                !m.file_type().is_symlink(),
                "データ内のリンクは見積もりできません: {}",
                path.display()
            );
            let n = if m.is_dir() {
                walk(&path)?
            } else {
                ensure!(m.is_file(), "特殊ファイルは扱えません");
                m.len()
            };
            bytes = bytes
                .checked_add(n)
                .context("データサイズが上限を超えます")?;
        }
        Ok(bytes)
    }
    ensure!(
        fs::symlink_metadata(root)?.is_dir(),
        "対象のルートは通常のディレクトリにしてください"
    );
    walk(root)
}

pub fn reserve(c: &crate::config::Config, bytes: u64) -> Result<()> {
    let minimum = c
        .maintenance
        .minimum_free_mib
        .checked_mul(1048576)
        .context("予約容量が大きすぎます")?;
    let required = bytes
        .checked_add(minimum)
        .context("必要容量が大きすぎます")?;
    ensure!(
        free(&c.paths.store)? > required,
        "操作と失敗時の復旧に必要な空き容量が不足しています"
    );
    Ok(())
}
