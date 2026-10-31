use bdsctl::{config::Config, discord, releases, setup, snapshots, util};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::{MetadataExt, symlink},
    path::PathBuf,
};
struct Fixture {
    root: PathBuf,
    c: Config,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("bds-test-{}", util::nonce().unwrap()));
        fs::create_dir(&root).unwrap();
        let mut c = Config::default();
        c.paths.store = root.join("store");
        c.paths.releases = root.join("releases");
        c.paths.runtime = root.join("runtime");
        c.paths.diagnostics = root.join("diagnostics");
        c.paths.discord_state = root.join("discord");
        c.paths.webhook_file = root.join("webhook");
        c.maintenance.minimum_free_mib = 0;
        for p in [
            &c.paths.store,
            &c.paths.releases,
            &c.paths.runtime,
            &c.paths.discord_state,
        ] {
            fs::create_dir(p).unwrap();
        }
        Self { root, c }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[test]
fn reject_overlapping_paths() {
    let mut f = Fixture::new();
    f.c.paths.releases = f.c.paths.store.join("releases");
    assert!(f.c.validate().is_err());
}
#[test]
fn reject_live_data_links() {
    let f = Fixture::new();
    fs::create_dir(f.c.data()).unwrap();
    symlink("/etc/passwd", f.c.data().join("linked")).unwrap();
    assert!(util::files(&f.c.data()).is_err());
    assert!(snapshots::capture(&f.c, "1.2.3.4").is_err());
}
#[test]
fn empty_directories_survive_independent_copy() {
    let f = Fixture::new();
    let from = f.root.join("from");
    fs::create_dir_all(from.join("empty")).unwrap();
    fs::write(from.join("data"), "original").unwrap();
    let dest = f.root.join("to");
    util::copy_tree(&from, &dest).unwrap();
    assert!(dest.join("empty").is_dir());
    assert_ne!(
        fs::metadata(from.join("data")).unwrap().ino(),
        fs::metadata(dest.join("data")).unwrap().ino()
    );
    fs::write(dest.join("data"), "changed").unwrap();
    assert_eq!(fs::read_to_string(from.join("data")).unwrap(), "original");
}
#[test]
fn backup_links_only_other_snapshots_and_restore_is_independent() {
    let f = Fixture::new();
    fs::create_dir_all(f.c.data().join("worlds/empty")).unwrap();
    fs::write(f.c.data().join("worlds/data"), "original").unwrap();
    let a = snapshots::capture(&f.c, "1.2.3.4").unwrap();
    let b = snapshots::capture(&f.c, "1.2.3.4").unwrap();
    let a_file = f.c.snapshots().join(&a.id).join("data/worlds/data");
    let b_file = f.c.snapshots().join(&b.id).join("data/worlds/data");
    assert_eq!(
        fs::metadata(&a_file).unwrap().ino(),
        fs::metadata(&b_file).unwrap().ino()
    );
    assert_ne!(
        fs::metadata(&a_file).unwrap().ino(),
        fs::metadata(f.c.data().join("worlds/data")).unwrap().ino()
    );
    let restored = snapshots::prepare_restore(&f.c, &a.id).unwrap();
    fs::write(restored.join("worlds/data"), "mutated").unwrap();
    assert_eq!(fs::read_to_string(a_file).unwrap(), "original");
    assert!(restored.join("worlds/empty").is_dir());
    snapshots::verify(&f.c, &a.id).unwrap();
    fs::write(&b_file, "corrupt").unwrap();
    assert!(snapshots::verify(&f.c, &a.id).is_err());
}
#[test]
fn protected_snapshot_retention() {
    let mut f = Fixture::new();
    f.c.maintenance.retention = 1;
    fs::create_dir_all(f.c.data()).unwrap();
    fs::write(f.c.data().join("file"), "data").unwrap();
    let protected = snapshots::capture(&f.c, "1.2.3.4").unwrap();
    snapshots::protect(&f.c, &protected.id, true).unwrap();
    let older = snapshots::capture(&f.c, "1.2.3.4").unwrap();
    let newer = snapshots::capture(&f.c, "1.2.3.4").unwrap();
    snapshots::prune(&f.c).unwrap();
    assert!(snapshots::load(&f.c, &older.id).is_err());
    assert!(snapshots::load(&f.c, &newer.id).is_ok());
    assert_eq!(snapshots::list(&f.c).unwrap().len(), 2);
    assert!(snapshots::load(&f.c, &protected.id).unwrap().protected);
}
#[test]
fn rejects_archive_traversal() {
    for p in ["../world", "/etc/passwd", "x/../../secret", ""] {
        assert!(snapshots::safe_relative(std::path::Path::new(p)).is_err());
    }
    assert!(releases::version_url("https://evil.test/bedrock-server-1.2.3.4.zip").is_err());
    assert!(
        releases::version_url(
            "https://www.minecraft.net/bedrockdedicatedserver/bin-linux/bedrock-server-../x.zip"
        )
        .is_err()
    );
}
#[test]
fn confirmation_owner_expiry_cancel_and_replay() {
    let mut f = Fixture::new();
    f.c.discord.application_id = "1".into();
    f.c.discord.guild_id = "2".into();
    f.c.discord.allowed_roles = vec!["3".into()];
    let base = json!({"application_id":"1","guild_id":"2","member":{"user":{"id":"4"},"roles":["3"]},"type":2,"data":{"name":"restart"}});
    let (prompt, _) = discord::handle(&f.c, &base).unwrap();
    let custom = prompt["data"]["components"][0]["components"][0]["custom_id"]
        .as_str()
        .unwrap();
    let mut confirm = base.clone();
    confirm["type"] = json!(3);
    confirm["data"] = json!({"custom_id":custom});
    confirm["member"]["user"]["id"] = json!("5");
    assert!(discord::handle(&f.c, &confirm).unwrap().1.is_none());
    confirm["member"]["user"]["id"] = json!("4");
    let (_, launch) = discord::handle(&f.c, &confirm).unwrap();
    assert_eq!(launch.unwrap().kind, "restart");
    assert!(discord::handle(&f.c, &confirm).unwrap().1.is_none());
    assert!(discord::handle(&f.c, &base).unwrap().0["data"]["components"].is_null());
}
#[test]
fn unknown_dispatched_job_survives_restart_without_redispatch() {
    let mut f = Fixture::new();
    f.c.discord.application_id = "1".into();
    f.c.discord.guild_id = "2".into();
    f.c.discord.allowed_users = vec!["4".into()];
    let j = discord::Job {
        id: "nonce".into(),
        user: "4".into(),
        kind: "restart".into(),
        phase: "dispatched".into(),
        created: 0,
        boot: util::boot(),
    };
    util::json(&f.c.paths.discord_state.join("job.json"), &j, 0o600).unwrap();
    let v = json!({"application_id":"1","guild_id":"2","member":{"user":{"id":"4"}},"type":2,"data":{"name":"reboot"}});
    let (response, launch) = discord::handle(&f.c, &v).unwrap();
    assert!(launch.is_none());
    assert!(response["data"]["components"].is_null());
}
#[test]
fn units_do_not_force_kill_or_grant_arbitrary_commands() {
    let f = Fixture::new();
    let units = setup::units(
        &f.c,
        std::path::Path::new("/usr/local/bin/bdsctl"),
        std::path::Path::new("/etc/bds/config.toml"),
    )
    .unwrap();
    let bds = &units["bds.service"];
    assert!(bds.contains("Type=notify"));
    assert!(bds.contains("SendSIGKILL=no"));
    assert!(bds.contains("Restart=on-failure"));
    let policy = &units["50-bds-discord.rules"];
    assert!(policy.contains("subject.user === 'bds-discord'"));
    assert!(!policy.contains("reboot.target"));
}
#[test]
fn lock_rejects_concurrent_operation() {
    let f = Fixture::new();
    let p = f.root.join("lock");
    let a = util::Lock::acquire(&p, 0).unwrap();
    assert!(util::Lock::acquire(&p, 0).is_err());
    drop(a);
    assert!(util::Lock::acquire(&p, 0).is_ok());
}
#[test]
fn malformed_snapshot_is_not_silently_retained() {
    let f = Fixture::new();
    fs::create_dir_all(f.c.snapshots().join("damaged")).unwrap();
    fs::write(f.c.snapshots().join("damaged/snapshot.json"), "{}").unwrap();
    assert!(snapshots::list(&f.c).is_err());
}

#[test]
fn unsafe_or_incomplete_zip_is_removed() {
    use std::io::Write;
    let f = Fixture::new();
    let archive = f.root.join("input.zip");
    let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
    zip.start_file("../escape", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"bad").unwrap();
    zip.finish().unwrap();
    let dest = f.root.join("extract");
    assert!(releases::extract_zip(&archive, &dest).is_err());
    assert!(!dest.exists());
    assert!(!f.root.join("escape").exists());
    let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
    zip.start_file(
        "server.properties",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(b"name=x").unwrap();
    zip.finish().unwrap();
    assert!(releases::extract_zip(&archive, &dest).is_err());
    assert!(!dest.exists());
}
#[test]
fn metadata_preserving_changes_are_still_copied() {
    let f = Fixture::new();
    fs::create_dir_all(f.c.data()).unwrap();
    let live = f.c.data().join("world");
    fs::write(&live, b"old!").unwrap();
    let first = snapshots::capture(&f.c, "1.2.3.4").unwrap();
    let metadata = fs::metadata(&live).unwrap();
    let time = metadata.modified().unwrap();
    fs::write(&live, b"new!").unwrap();
    fs::File::options()
        .write(true)
        .open(&live)
        .unwrap()
        .set_modified(time)
        .unwrap();
    let second = snapshots::capture(&f.c, "1.2.3.4").unwrap();
    assert_eq!(
        fs::read(f.c.snapshots().join(first.id).join("data/world")).unwrap(),
        b"old!"
    );
    assert_eq!(
        fs::read(f.c.snapshots().join(second.id).join("data/world")).unwrap(),
        b"new!"
    );
}

#[test]
fn extraction_under_private_umask() {
    if std::env::var_os("BDS_TEST_PRIVATE_MASK").is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "extraction_under_private_umask"])
            .env("BDS_TEST_PRIVATE_MASK", "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stdout)
        );
        return;
    }
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    unsafe {
        libc::umask(0o077);
    }
    let f = Fixture::new();
    let public = f.root.join("public.json");
    util::atomic(&public, b"public", 0o644).unwrap();
    assert_eq!(
        fs::metadata(public).unwrap().permissions().mode() & 0o777,
        0o644
    );
    let archive = f.root.join("input.zip");
    let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
    for path in ["bedrock_server", "server.properties", "packs/nested/file"] {
        zip.start_file(path, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"fixture").unwrap();
    }
    zip.finish().unwrap();
    let dest = f.root.join("extract");
    releases::extract_zip(&archive, &dest).unwrap();
    for p in [&dest, &dest.join("packs"), &dest.join("packs/nested")] {
        assert_eq!(fs::metadata(p).unwrap().permissions().mode() & 0o777, 0o755);
    }
    assert_eq!(
        fs::metadata(dest.join("bedrock_server"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}

#[test]
fn restore_capacity_is_checked_before_copying() {
    let mut f = Fixture::new();
    fs::create_dir_all(f.c.data()).unwrap();
    fs::write(f.c.data().join("world"), b"world").unwrap();
    let s = snapshots::capture(&f.c, "1.2.3.4").unwrap();
    f.c.maintenance.minimum_free_mib = u64::MAX;
    assert!(snapshots::prepare_restore(&f.c, &s.id).is_err());
    assert!(!fs::read_dir(&f.c.paths.store).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".restore-")
    }));
    assert_eq!(fs::read(f.c.data().join("world")).unwrap(), b"world");
}
