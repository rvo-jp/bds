//! Start and stop one systemd invocation; never supervise by polling in the background.
use super::{
    placement::ensure_placement_complete,
    state::{ProcessOwner, REBOOT_PENDING, START_PERMIT, ensure_ready},
};
use crate::{config::Config, supervisor, util};
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};

pub fn active(c: &Config) -> Result<bool> {
    let state = util::prop(&c.server.unit, "ActiveState")?;
    ensure!(
        matches!(state.as_str(), "active" | "inactive" | "failed"),
        "起動・停止の途中です: {state}"
    );
    Ok(state == "active")
}

pub fn ensure_stopped(c: &Config) -> Result<()> {
    let state = util::prop(&c.server.unit, "ActiveState")?;
    ensure!(
        matches!(state.as_str(), "inactive" | "failed"),
        "BDSの完全停止を確認できません: {state}"
    );
    ensure!(
        util::prop(&c.server.unit, "MainPID")? == "0",
        "BDSのプロセスが残っています"
    );
    let cg = util::prop(&c.server.unit, "ControlGroup")?;
    if !cg.is_empty() {
        let p = PathBuf::from(format!("/sys/fs/cgroup{cg}/cgroup.procs"));
        if p.exists() {
            ensure!(
                fs::read_to_string(p)?.trim().is_empty(),
                "BDSの子プロセスが残っています"
            );
        }
    }
    Ok(())
}

/// Inactivity alone is insufficient: require a normal save/exit from this OS boot.
pub fn ensure_clean_backup_source(c: &Config) -> Result<()> {
    ensure_stopped(c)?;
    ensure!(
        util::prop(&c.server.unit, "ActiveState")? == "inactive",
        "異常停止中のデータを通常バックアップには保存しません。診断記録を確認してください"
    );
    let s = supervisor::status(c)
        .context("正常終了記録がありません。通常バックアップには起動・正常停止の確認が必要です")?;
    ensure!(
        s.stopped
            && !s.failed
            && s.child_exit == Some(0)
            && s.boot == util::boot()
            && !s.invocation.is_empty(),
        "正常終了を確認できないデータを通常バックアップには保存しません。診断記録を確認してください"
    );
    Ok(())
}

pub fn stop(c: &Config) -> Result<()> {
    let invocation = util::prop(&c.server.unit, "InvocationID")?;
    util::systemctl(&["stop", &c.server.unit])?;
    ensure_stopped(c)?;
    let s = supervisor::status(c)?;
    ensure!(
        !invocation.is_empty()
            && s.invocation == invocation
            && s.stopped
            && !s.failed
            && s.child_exit == Some(0),
        "今回のBDSの正常終了記録がありません"
    );
    Ok(())
}

pub fn start(c: &Config) -> Result<()> {
    crate::admin::ensure_available(c)?;
    ensure_placement_complete(c)?;
    START_PERMIT.save(c, &ProcessOwner::current())?;
    let result = start_wait(c);
    let _ = START_PERMIT.remove(c);
    result
}

fn start_wait(c: &Config) -> Result<()> {
    // systemctl waits for recovery too; only BDS retries use the deadline below.
    let previous = supervisor::status(c).ok().map(|s| s.invocation);
    let _ = util::systemctl(&["start", &c.server.unit]);
    let deadline = Instant::now()
        + Duration::from_secs(
            (c.server.port_wait_seconds
                + c.server.startup_seconds
                + c.server.stable_seconds
                + 60
                + c.server.restart_seconds)
                * c.server.start_limit as u64
                + 30,
        );
    while Instant::now() < deadline {
        let state = util::prop(&c.server.unit, "ActiveState")?;
        if state == "active" {
            ensure!(supervisor::running(c), "サービスと起動記録が一致しません");
            return Ok(());
        }
        if matches!(state.as_str(), "inactive" | "failed") {
            let recorded = supervisor::status(c)
                .ok()
                .filter(|s| !s.invocation.is_empty() && previous.as_ref() != Some(&s.invocation));
            if recorded.as_ref().is_some_and(|s| s.pid > 0) {
                bail!("BDSを起動できません。診断記録を確認してください")
            }
            let reason = recorded
                .map(|s| s.reason)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    "BDSの起動準備を完了できません。サービス状態と診断記録を確認してください".into()
                });
            return Err(supervisor::PreparationError(anyhow::anyhow!(reason)).into());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    bail!("起動確認が期限を超えました")
}

pub fn maintenance_notice(c: &Config, message: &str) -> Result<()> {
    notice(c, message)
}

pub(super) fn notice(c: &Config, message: &str) -> Result<()> {
    if active(c)? {
        let n = c.maintenance.notice_seconds;
        supervisor::command(
            c,
            &format!("say {message}のため{n}秒後にサーバーを停止します。"),
        )?;
        if n > 10 {
            std::thread::sleep(Duration::from_secs(n - 10));
            supervisor::command(c, "say 10秒後にサーバーを停止します。")?;
            std::thread::sleep(Duration::from_secs(10));
        } else {
            std::thread::sleep(Duration::from_secs(n));
        }
    }
    Ok(())
}

pub(super) fn ensure_starting_without_bds(c: &Config) -> Result<()> {
    let pid = std::process::id().to_string();
    ensure!(
        util::prop(&c.server.unit, "ActiveState")? == "activating"
            && util::prop(&c.server.unit, "MainPID")? == "0"
            && util::prop(&c.server.unit, "ControlPID")? == pid,
        "start-checkはBDS起動前のサービス処理専用です。手動復旧には停止後にrecoverを使用してください"
    );
    let group = util::prop(&c.server.unit, "ControlGroup")?;
    ensure!(!group.is_empty(), "起動前のプロセス領域を確認できません");
    let processes = fs::read_to_string(format!("/sys/fs/cgroup{group}/cgroup.procs"))?;
    ensure!(
        processes.lines().any(|p| p == pid) && processes.lines().all(|p| p == pid),
        "起動前処理以外のプロセスがサービス内に残っています"
    );
    Ok(())
}

pub fn restart(c: &Config) -> Result<()> {
    let _lock = crate::state::lock(c, 0)?;
    crate::admin::ensure_available(c)?;
    ensure_ready(c)?;
    if active(c)? {
        notice(c, "再起動")?;
        stop(c)?;
    } else {
        ensure_stopped(c)?;
    }
    start(c)
}
pub fn reboot(c: &Config) -> Result<()> {
    let _lock = crate::state::lock(c, 0)?;
    crate::admin::ensure_available(c)?;
    ensure_ready(c)?;
    let was_active = active(c)?;
    if was_active {
        notice(c, "OS再起動")?;
        stop(c)?;
    } else {
        ensure_stopped(c)?;
    }
    REBOOT_PENDING.save(c, &util::boot())?;
    if let Err(e) = util::systemctl(&["--no-block", "reboot"]) {
        REBOOT_PENDING.remove(c)?;
        if was_active {
            let _ = start(c);
        }
        return Err(e);
    }
    Ok(())
}
