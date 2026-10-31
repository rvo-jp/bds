//! Isolated acceptance-test version source. Never included in production bundles.
use anyhow::{Context, Result};
use bdsctl::{config::Config, operations, releases, util};
use std::path::Path;
fn main() -> Result<()> {
    util::root()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(args.len() == 2, "CONFIG VERSION");
    let c = Config::load(Path::new(&args[0]))?;
    operations::auto_update_with(&c, || {
        anyhow::ensure!(args[1] != "offline", "fixture: version source unavailable");
        let id = args.get(1).context("version")?.clone();
        let url = releases::download_url(&id);
        Ok((id, url))
    })?;
    Ok(())
}
