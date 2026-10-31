//! Isolated acceptance-test version source; excluded from production bundles.
use anyhow::Result;
use bdsctl::{config::Config, operations, util};
use std::path::Path;
fn main() -> Result<()> {
    util::root()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(args.len() == 2, "CONFIG VERSION");
    let c = Config::load(Path::new(&args[0]))?;
    operations::init_with(&c, || {
        anyhow::ensure!(args[1] != "offline", "fixture: version source unavailable");
        let id = args[1].clone();
        let url = format!(
            "https://www.minecraft.net/bedrockdedicatedserver/bin-linux/bedrock-server-{id}.zip"
        );
        Ok((id, url))
    })
}
