fn main() {
    println!("cargo:rerun-if-env-changed=BDS_COMMIT");
    let commit = std::env::var("BDS_COMMIT").unwrap_or_else(|_| "development".into());
    if commit != "development" {
        assert!(
            commit.len() == 40
                && commit
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "BDS_COMMIT must be a full lowercase Git commit"
        );
    }
    println!("cargo:rustc-env=BDS_COMMIT={commit}");
}
