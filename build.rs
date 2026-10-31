use std::process::Command;
fn main() {
    println!("cargo:rerun-if-env-changed=BDS_COMMIT");
    if let Ok(commit) = std::env::var("BDS_COMMIT") {
        println!("cargo:rustc-env=BDS_COMMIT={commit}");
        return;
    }
    let output = Command::new("git").args(["rev-parse", "HEAD"]).output();
    let commit = output
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_else(|| "unversioned".into());
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(true);
    println!(
        "cargo:rustc-env=BDS_COMMIT={}{}",
        commit.trim(),
        if dirty { "-dirty" } else { "" }
    );
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=Cargo.lock");
}
