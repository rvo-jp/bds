use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
#[derive(Default)]
pub struct Config {
    pub paths: Paths,
    pub server: Server,
    pub maintenance: Maintenance,
    pub discord: Discord,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Paths {
    pub store: PathBuf,
    pub releases: PathBuf,
    pub runtime: PathBuf,
    pub diagnostics: PathBuf,
    pub discord_state: PathBuf,
    pub webhook_file: PathBuf,
}
impl Default for Paths {
    fn default() -> Self {
        Self {
            store: "/var/lib/bds".into(),
            releases: "/opt/bedrock/releases".into(),
            runtime: "/run/bds".into(),
            diagnostics: "/var/lib/bds-supervisor".into(),
            discord_state: "/var/lib/bds-discord".into(),
            webhook_file: "/etc/bds/webhook".into(),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Server {
    pub user: String,
    pub unit: String,
    pub startup_seconds: u64,
    pub stable_seconds: u64,
    pub response_seconds: u64,
    pub restart_seconds: u64,
    pub start_limit: u32,
}
impl Default for Server {
    fn default() -> Self {
        Self {
            user: "bds".into(),
            unit: "bds.service".into(),
            startup_seconds: 120,
            stable_seconds: 15,
            response_seconds: 10,
            restart_seconds: 30,
            start_limit: 3,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Maintenance {
    pub notice_seconds: u64,
    pub retention: usize,
    pub minimum_free_mib: u64,
    pub lock_wait_seconds: u64,
}
impl Default for Maintenance {
    fn default() -> Self {
        Self {
            notice_seconds: 60,
            retention: 14,
            minimum_free_mib: 1024,
            lock_wait_seconds: 300,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Discord {
    pub enabled: bool,
    pub listen: String,
    pub application_id: String,
    pub public_key: String,
    pub guild_id: String,
    pub allowed_users: Vec<String>,
    pub allowed_roles: Vec<String>,
    pub cooldown_seconds: u64,
}
impl Default for Discord {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: "127.0.0.1:8765".into(),
            application_id: String::new(),
            public_key: String::new(),
            guild_id: String::new(),
            allowed_users: vec![],
            allowed_roles: vec![],
            cooldown_seconds: 300,
        }
    }
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let c: Self = toml::from_str(&fs::read_to_string(path).context("設定を読み込めません")?)?;
        c.validate()?;
        Ok(c)
    }
    pub fn data(&self) -> PathBuf {
        self.paths.store.join("data")
    }
    pub fn state(&self) -> PathBuf {
        self.paths.store.join("state")
    }
    pub fn snapshots(&self) -> PathBuf {
        self.paths.store.join("backups")
    }
    pub fn current(&self) -> PathBuf {
        self.paths.releases.join("current")
    }
    pub fn socket(&self) -> PathBuf {
        self.paths.runtime.join("control.sock")
    }
    pub fn validate(&self) -> Result<()> {
        identifier(&self.server.unit)?;
        ensure!(
            self.server.unit.starts_with("bds") && self.server.unit.ends_with(".service"),
            "サービス名はbdsで始まる.serviceにしてください"
        );
        let paths = [
            &self.paths.store,
            &self.paths.releases,
            &self.paths.runtime,
            &self.paths.diagnostics,
            &self.paths.discord_state,
        ];
        for (i, p) in paths.iter().enumerate() {
            valid_absolute(p)?;
            for q in paths.iter().skip(i + 1) {
                ensure!(
                    !p.starts_with(q) && !q.starts_with(p),
                    "保存先・実行先は互いに独立したディレクトリにしてください"
                );
            }
        }
        valid_absolute(&self.paths.webhook_file)?;
        ensure!(
            self.server.user.len() <= 32
                && self.server.user.starts_with("bds")
                && self
                    .server
                    .user
                    .bytes()
                    .all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || x == b'-'),
            "実行ユーザーはbdsで始まる専用ユーザーにしてください"
        );
        ensure!(
            (1..=3).contains(&self.server.start_limit),
            "起動上限は1〜3です"
        );
        ensure!(
            (1..=300).contains(&self.server.startup_seconds) && self.server.stable_seconds <= 60,
            "起動確認の秒数が範囲外です"
        );
        ensure!(
            (1..=60).contains(&self.server.response_seconds),
            "起動時の応答期限は1〜60秒にしてください"
        );
        ensure!(self.server.restart_seconds > 0, "復旧設定が不正です");
        ensure!(
            self.maintenance.retention > 0
                && self.maintenance.notice_seconds <= 3600
                && self.maintenance.lock_wait_seconds <= 300,
            "メンテナンス設定が不正です"
        );
        let addr: std::net::SocketAddr = self.discord.listen.parse()?;
        ensure!(addr.ip().is_loopback(), "Discord受付はループバックのみです");
        if self.discord.enabled {
            ensure!(
                snowflake(&self.discord.application_id) && snowflake(&self.discord.guild_id),
                "Discord IDが不正です"
            );
            let key: [u8; 32] = hex::decode(&self.discord.public_key)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("Public Keyは32バイトです"))?;
            ed25519_dalek::VerifyingKey::from_bytes(&key)?;
            ensure!(
                !self.discord.allowed_users.is_empty() || !self.discord.allowed_roles.is_empty(),
                "許可対象が空です"
            );
            ensure!(
                self.discord
                    .allowed_users
                    .iter()
                    .chain(self.discord.allowed_roles.iter())
                    .all(|x| snowflake(x)),
                "許可IDが不正です"
            );
        }
        Ok(())
    }
}
pub fn snowflake(s: &str) -> bool {
    !s.is_empty() && s.len() <= 20 && s.bytes().all(|c| c.is_ascii_digit())
}
pub fn identifier(s: &str) -> Result<()> {
    ensure!(
        !s.is_empty()
            && s.len() <= 100
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "識別子が不正です"
    );
    Ok(())
}
pub fn valid_absolute(p: &Path) -> Result<()> {
    ensure!(
        p.is_absolute() && p.parent().is_some(),
        "絶対パスが必要です"
    );
    ensure!(
        p.components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_))),
        "正規化されたパスが必要です"
    );
    ensure!(
        !p.as_os_str()
            .to_string_lossy()
            .contains(['\n', '\r', '\0', '%', '"', '\\']),
        "パスに使用できない文字があります"
    );
    #[cfg(target_os = "linux")]
    for ancestor in p.ancestors() {
        if let Ok(m) = fs::symlink_metadata(ancestor)
            && m.file_type().is_symlink()
        {
            anyhow::bail!("保存先にシンボリックリンクがあります");
        }
    }
    Ok(())
}
pub fn properties(path: &Path) -> Result<std::collections::BTreeMap<String, String>> {
    let mut m = std::collections::BTreeMap::new();
    for line in fs::read_to_string(path)?.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            m.insert(k.trim().into(), v.trim().into());
        }
    }
    Ok(m)
}
pub fn port(c: &Config) -> Result<u16> {
    let p = properties(&c.data().join("server.properties"))?;
    ensure!(
        p.get("transport")
            .map(String::as_str)
            .unwrap_or("nethernet")
            == "nethernet",
        "NetherNetのみ対応します"
    );
    let port = p
        .get("server-port")
        .map(String::as_str)
        .unwrap_or("19132")
        .parse::<u16>()?;
    ensure!(port > 0, "ポートが不正です");
    Ok(port)
}
