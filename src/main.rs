use anyhow::{Context, Result, bail};
use bdsctl::{
    admin, config::Config, discord, operations, recovery, setup, snapshots, supervisor, util,
};
use std::path::Path;
fn main() {
    if let Err(error) = run() {
        eprintln!("bdsctl: {error:#}");
        std::process::exit(1)
    }
}
fn run() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "--help" {
        println!(
            "bdsctl [--config /etc/bds/config.toml] COMMAND\n\nstatus | status-json | inventory | maintenance-begin | maintenance-end | console TEXT | backup | daily | restart | reboot | update | check-update | auto-update\nsnapshots | verify ID | protect ID | unprotect ID | restore ID | export ID FILE\ninit | import-legacy PATH | import-package FILE | recover | supervise | discord | register-commands\nprepare-directories | generate-units DIR [BINARY]\n\n破壊を伴う操作・インストール操作はroot専用です。コマンドを省略すると何も変更しません。"
        );
        return Ok(());
    }
    if args[0] == "--version" {
        println!(
            "bdsctl {} ({})",
            env!("CARGO_PKG_VERSION"),
            option_env!("BDS_COMMIT").unwrap_or("development")
        );
        return Ok(());
    }
    let config = if args[0] == "--config" {
        if args.len() < 3 {
            bail!("設定パスとコマンドが必要です")
        }
        let path = args.remove(1);
        args.remove(0);
        path
    } else {
        "/etc/bds/config.toml".into()
    };
    let c = Config::load(Path::new(&config))?;
    let command = args.remove(0);
    let arity = match command.as_str() {
        "console" => None,
        "generate-units" => None,
        "verify" | "protect" | "unprotect" | "restore" | "import-legacy" | "discord-job"
        | "notify-failure" => Some(1),
        "export" => Some(2),
        "import-package" => Some(1),
        _ => Some(0),
    };
    if let Some(n) = arity {
        anyhow::ensure!(args.len() == n, "引数の数が不正です");
    } else if command == "generate-units" {
        anyhow::ensure!(
            (1..=2).contains(&args.len()),
            "出力先と、省略可能なバイナリパスを指定してください"
        );
    } else {
        anyhow::ensure!(!args.is_empty(), "コンソールコマンドが必要です");
    }
    let value = |index: usize| -> Result<&str> {
        args.get(index)
            .map(String::as_str)
            .context("引数が不足しています")
    };
    match command.as_str() {
        "status" => {
            util::root()?;
            admin::status(&c)?;
        }
        "status-json" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&supervisor::status(&c)?)?
            );
        }
        "console" => {
            util::root()?;
            supervisor::command(&c, &args.join(" "))?
        }
        "snapshots" => {
            util::root()?;
            for s in snapshots::list(&c)? {
                println!("{}", admin::snapshot_line(&s));
            }
        }
        "verify" => {
            util::root()?;
            let s = snapshots::verify(&c, value(0)?)?;
            println!("{}: {} files verified", s.id, s.files.len())
        }
        "protect" | "unprotect" => {
            util::root()?;
            let _lock = util::Lock::acquire(&c.paths.store.join("maintenance.lock"), 0)?;
            snapshots::protect(&c, value(0)?, command == "protect")?
        }
        "export" => {
            util::root()?;
            let _lock = util::Lock::acquire(&c.paths.store.join("maintenance.lock"), 0)?;
            snapshots::export(&c, value(0)?, Path::new(value(1)?))?
        }
        "supervise" => supervisor::run(&c)?,
        "discord" => discord::serve(&c)?,
        "register-commands" => discord::register(&c)?,
        "generate-units" => setup::generate(
            &c,
            Path::new(
                args.get(1)
                    .map(String::as_str)
                    .unwrap_or("/usr/local/bin/bdsctl"),
            ),
            Path::new(&config),
            Path::new(value(0)?),
        )?,
        command => {
            util::root()?;
            match command {
                "prepare-directories" => setup::directories(&c)?,
                "notify-failure" => {
                    bdsctl::config::identifier(value(0)?)?;
                    util::webhook(
                        &c.paths.webhook_file,
                        &format!(
                            "サーバー管理処理 {} が失敗しました。サービス状態と診断記録を確認してください。",
                            value(0)?
                        ),
                    )?;
                }
                "maintenance-begin" => admin::begin(&c)?,
                "maintenance-end" => admin::end(&c)?,
                "inventory" => admin::inventory(&c)?,
                "deployment-check" => admin::deployment_check(&c)?,
                "start-check" => operations::start_check(&c)?,
                "import-package" => recovery::import(&c, Path::new(value(0)?))?,
                "recover" => operations::recover(&c)?,
                "init" => operations::init(&c)?,
                "import-legacy" => operations::import_legacy(&c, Path::new(value(0)?))?,
                "backup" => {
                    operations::maintenance(&c, false, false)?;
                }
                "daily" => {
                    operations::daily(&c)?;
                }
                "update" => {
                    operations::maintenance(&c, true, false)?;
                }
                "auto-update" => {
                    operations::auto_update(&c)?;
                }
                "check-update" => {
                    let _lock = util::Lock::acquire(&c.paths.store.join("maintenance.lock"), 0)?;
                    operations::check(&c)?;
                }
                "restart" => operations::restart(&c)?,
                "reboot" => operations::reboot(&c)?,
                "restore" => operations::restore(&c, value(0)?)?,
                "discord-job" => discord::execute(&c, value(0)?)?,
                _ => bail!("不明なコマンドです: {command}"),
            }
        }
    }
    Ok(())
}
