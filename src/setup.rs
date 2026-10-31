//! Installation is explicit: generating files never enables or starts a service.
use crate::{config::Config, util};
use anyhow::{Result, ensure};
use std::{collections::BTreeMap, fs, path::Path};
fn quote(p: &Path) -> String {
    format!("\"{}\"", p.display())
}
pub fn units(c: &Config, binary: &Path, config: &Path) -> Result<BTreeMap<String, String>> {
    crate::config::valid_absolute(binary)?;
    crate::config::valid_absolute(config)?;
    let exec = format!("{} --config {}", quote(binary), quote(config));
    let mut out = BTreeMap::new();
    let startup = c.server.startup_seconds + c.server.stable_seconds + 15;
    let interval = 600.max((startup + c.server.restart_seconds) * c.server.start_limit as u64 + 60);
    out.insert(c.server.unit.clone(),format!("[Unit]\nDescription=Minecraft Bedrock server\nWants=network-online.target\nAfter=network-online.target\nStartLimitIntervalSec={interval}\nStartLimitBurst={}\n\n[Service]\nType=notify\nNotifyAccess=main\nUser=bds\nGroup=bds\nExecStartPre=+{exec} recover\nExecStart={exec} supervise\nRestart=on-failure\nRestartSec={}\nTimeoutStartSec={startup}\nTimeoutStopSec=infinity\nKillMode=mixed\nSendSIGKILL=no\nRuntimeDirectory=bds\nRuntimeDirectoryMode=0750\nRuntimeDirectoryPreserve=yes\nStateDirectory=bds-supervisor\nStateDirectoryMode=0700\nUMask=0027\nNoNewPrivileges=yes\nProtectSystem=strict\nProtectHome=yes\nPrivateTmp=yes\nReadWritePaths={} {} {} {}\n\n[Install]\nWantedBy=multi-user.target\n",c.server.start_limit,c.server.restart_seconds,quote(&c.paths.store),quote(&c.paths.releases),quote(&c.paths.runtime),quote(&c.paths.diagnostics)));
    for (name, command) in [
        ("bds-daily", "daily"),
        ("bds-update", "auto-update"),
        ("bds-discord-restart", "discord-job restart"),
        ("bds-discord-reboot", "discord-job reboot"),
    ] {
        out.insert(format!("{name}.service"),format!("[Unit]\nDescription=BDS {command}\nWants=network-online.target\nAfter=network-online.target\n\n[Service]\nType=oneshot\nExecStart={exec} {command}\nTimeoutStartSec=infinity\nTimeoutStopSec=infinity\nKillMode=control-group\nSendSIGKILL=no\nNice=10\nCPUWeight=20\nIOWeight=20\nUMask=0077\n"));
    }
    out.insert("bds-daily.timer".into(),"[Unit]\nDescription=Daily offline BDS backup\n[Timer]\nOnCalendar=*-*-* 04:30:00 Asia/Tokyo\nAccuracySec=1s\nPersistent=false\n[Install]\nWantedBy=timers.target\n".into());
    out.insert("bds-update.timer".into(),"[Unit]\nDescription=Check and install BDS updates hourly\n[Timer]\nOnCalendar=*-*-* *:00:00 Asia/Tokyo\nRandomizedDelaySec=5m\nAccuracySec=1s\nPersistent=true\n[Install]\nWantedBy=timers.target\n".into());
    out.insert("bds-discord.service".into(),format!("[Unit]\nDescription=BDS Discord HTTP interactions\nWants=network-online.target\nAfter=network-online.target\nStartLimitIntervalSec=600\nStartLimitBurst=3\n[Service]\nType=notify\nUser=bds-discord\nGroup=bds-discord\nExecStart={exec} discord\nRestart=on-failure\nRestartSec=10\nTimeoutStartSec=30\nStateDirectory=bds-discord\nStateDirectoryMode=0700\nUMask=0077\nNoNewPrivileges=yes\nProtectSystem=strict\nProtectHome=yes\nPrivateTmp=yes\nPrivateDevices=yes\nRestrictSUIDSGID=yes\nReadWritePaths={}\n[Install]\nWantedBy=multi-user.target\n",quote(&c.paths.discord_state)));
    out.insert("50-bds-discord.rules".into(),"// Only the two fixed root jobs may be started by the Discord receiver.\npolkit.addRule(function(action, subject) {\n  if (subject.user === 'bds-discord' && action.id === 'org.freedesktop.systemd1.manage-units' && action.lookup('verb') === 'start' && (action.lookup('unit') === 'bds-discord-restart.service' || action.lookup('unit') === 'bds-discord-reboot.service')) return polkit.Result.YES;\n});\n".into());
    out.insert("bds-alert@.service".into(),format!("[Unit]\nDescription=BDS failure notification\n[Service]\nType=oneshot\nExecStart={exec} notify-failure %i\nTimeoutStartSec=25\nUMask=0077\n"));
    for (name, text) in out.iter_mut() {
        if name.ends_with(".service") && name != "bds-alert@.service" {
            *text = text.replacen("[Unit]\n", "[Unit]\nOnFailure=bds-alert@%n.service\n", 1);
        }
    }
    for text in out.values_mut() {
        *text = text.replace(
            "User=bds\nGroup=bds\n",
            &format!("User={}\nGroup={}\n", c.server.user, c.server.user),
        );
    }
    Ok(out)
}
pub fn generate(c: &Config, binary: &Path, config: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;
    for (name, contents) in units(c, binary, config)? {
        util::atomic(&dest.join(name), contents.as_bytes(), 0o644)?;
    }
    Ok(())
}
pub fn directories(c: &Config) -> Result<()> {
    util::root()?;
    for user in [c.server.user.as_str(), "bds-discord"] {
        let exists = std::process::Command::new("id")
            .args(["-u", user])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?
            .success();
        if !exists {
            util::run(
                "useradd",
                &[
                    "--system",
                    "--user-group",
                    "--no-create-home",
                    "--shell",
                    "/usr/sbin/nologin",
                    user,
                ],
            )?;
        }
    }
    if !std::process::Command::new("getent")
        .args(["group", "bds-notify"])
        .stdout(std::process::Stdio::null())
        .status()?
        .success()
    {
        util::run("groupadd", &["--system", "bds-notify"])?;
    }
    for user in [c.server.user.as_str(), "bds-discord"] {
        util::run("usermod", &["-a", "-G", "bds-notify", user])?;
    }
    for p in [
        &c.paths.store,
        &c.paths.releases,
        &c.state(),
        &c.snapshots(),
        &c.paths.runtime,
        &c.paths.discord_state,
        &c.paths.diagnostics,
    ] {
        fs::create_dir_all(p)?;
    }
    util::chmod(&c.paths.store, 0o755)?;
    util::chmod(&c.state(), 0o755)?;
    util::chmod(&c.snapshots(), 0o700)?;
    let bds_owner = format!("{}:{}", c.server.user, c.server.user);
    for (p, owner) in [
        (&c.paths.runtime, bds_owner.as_str()),
        (&c.paths.diagnostics, bds_owner.as_str()),
        (&c.paths.discord_state, "bds-discord:bds-discord"),
    ] {
        util::run("chown", &[owner, p.to_str().unwrap()])?;
        util::chmod(p, 0o700)?;
    }
    ensure!(
        std::process::Command::new("rsync")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .status()?
            .success(),
        "rsyncが必要です"
    );
    Ok(())
}
