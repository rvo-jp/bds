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
    out.insert(
        c.server.unit.clone(),
        format!(
            r#"[Unit]
Description=Minecraft Bedrock server
ConditionPathExists={}
Wants=network-online.target
After=network-online.target
StartLimitIntervalSec={interval}
StartLimitBurst={}

[Service]
Type=notify
NotifyAccess=main
User=bds
Group=bds
ExecStartPre=+{exec} start-check
ExecStart={exec} supervise
Restart=on-failure
RestartSec={}
TimeoutStartSec=infinity
TimeoutStopSec=infinity
KillMode=mixed
SendSIGKILL=no
RuntimeDirectory=bds
RuntimeDirectoryMode=0750
RuntimeDirectoryPreserve=yes
StateDirectory=bds-supervisor
StateDirectoryMode=0700
UMask=0027
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
ReadWritePaths={} {} {} {}

[Install]
WantedBy=multi-user.target
"#,
            format_args!("!{}", crate::admin::mode_path(c).display()),
            c.server.start_limit,
            c.server.restart_seconds,
            quote(&c.paths.store),
            quote(&c.paths.releases),
            quote(&c.paths.runtime),
            quote(&c.paths.diagnostics)
        ),
    );
    for (name, command) in [
        ("bds-daily", "daily"),
        ("bds-update", "auto-update"),
        ("bds-discord-restart", "discord-job restart"),
        ("bds-discord-reboot", "discord-job reboot"),
    ] {
        out.insert(
            format!("{name}.service"),
            format!(
                r#"[Unit]
Description=BDS {command}
Wants=network-online.target
After=network-online.target

[Service]
Type=oneshot
ExecStart={exec} {command}
TimeoutStartSec=infinity
TimeoutStopSec=infinity
KillMode=control-group
SendSIGKILL=no
Nice=10
CPUWeight=20
IOWeight=20
UMask=0077
"#
            ),
        );
    }
    out.insert(
        "bds-daily.timer".into(),
        r#"[Unit]
Description=Daily offline BDS backup
[Timer]
OnCalendar=*-*-* 04:30:00 Asia/Tokyo
AccuracySec=1s
Persistent=false
[Install]
WantedBy=timers.target
"#
        .into(),
    );
    out.insert(
        "bds-update.timer".into(),
        r#"[Unit]
Description=Check and install BDS updates hourly
[Timer]
OnCalendar=*-*-* *:00:00 Asia/Tokyo
RandomizedDelaySec=5m
AccuracySec=1s
Persistent=true
[Install]
WantedBy=timers.target
"#
        .into(),
    );
    out.insert(
        "bds-discord.service".into(),
        format!(
            r#"[Unit]
Description=BDS Discord HTTP interactions
Wants=network-online.target
After=network-online.target
StartLimitIntervalSec=600
StartLimitBurst=3
[Service]
Type=notify
User=bds-discord
Group=bds-discord
ExecStart={exec} discord
Restart=on-failure
RestartSec=10
TimeoutStartSec=30
StateDirectory=bds-discord
StateDirectoryMode=0700
UMask=0077
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
PrivateDevices=yes
RestrictSUIDSGID=yes
ReadWritePaths={}
[Install]
WantedBy=multi-user.target
"#,
            quote(&c.paths.discord_state)
        ),
    );
    out.insert("50-bds-discord.rules".into(),r#"// Only the two fixed root jobs may be started by the Discord receiver.
polkit.addRule(function(action, subject) {
  if (subject.user === 'bds-discord' && action.id === 'org.freedesktop.systemd1.manage-units' && action.lookup('verb') === 'start' && (action.lookup('unit') === 'bds-discord-restart.service' || action.lookup('unit') === 'bds-discord-reboot.service')) return polkit.Result.YES;
});
"#.into());
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
