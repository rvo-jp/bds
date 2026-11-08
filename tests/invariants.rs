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
    f.c.maintenance.extra_retention = 1;
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
fn update_versions_are_compared_numerically_without_downgrading() {
    assert!(releases::newer("1.26.10.1", "1.26.9.99").unwrap());
    assert!(!releases::newer("1.26.9.99", "1.26.10.1").unwrap());
    assert!(!releases::newer("1.26.10.1", "1.26.10.1").unwrap());
    assert!(releases::newer("invalid", "1.26.10.1").is_err());
    assert!(releases::newer("1.26.10.1", "invalid").is_err());
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
    assert_eq!(launch.unwrap().kind, discord::Action::Restart);
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
        kind: discord::Action::Restart,
        phase: discord::JobPhase::Dispatched,
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

#[test]
fn daily_retention_is_not_consumed_by_update_or_manual_backups() {
    let mut f = Fixture::new();
    f.c.maintenance.retention = 2;
    f.c.maintenance.extra_retention = 1;
    fs::create_dir_all(f.c.data()).unwrap();
    fs::write(f.c.data().join("world"), "data").unwrap();
    let oldest = snapshots::capture_reason(&f.c, "1.2.3.4", snapshots::Reason::Daily).unwrap();
    let daily = snapshots::capture_reason(&f.c, "1.2.3.4", snapshots::Reason::Daily).unwrap();
    let newest = snapshots::capture_reason(&f.c, "1.2.3.4", snapshots::Reason::Daily).unwrap();
    let old_extra = snapshots::capture_reason(&f.c, "1.2.3.4", snapshots::Reason::Update).unwrap();
    let extra = snapshots::capture_reason(&f.c, "1.2.3.4", snapshots::Reason::Manual).unwrap();
    snapshots::protect(&f.c, &old_extra.id, true).unwrap();
    snapshots::prune(&f.c).unwrap();
    assert!(snapshots::load(&f.c, &oldest.id).is_err());
    for id in [daily.id, newest.id, old_extra.id, extra.id] {
        snapshots::verify(&f.c, &id).unwrap();
    }
}
#[test]
fn maintenance_guard_survives_reload_and_rejects_corrupt_record() {
    let f = Fixture::new();
    let p = bdsctl::admin::mode_path(&f.c);
    util::atomic(&p, b"broken record", 0o644).unwrap();
    assert!(bdsctl::admin::ensure_available(&f.c).is_err());
    assert!(bdsctl::admin::in_maintenance(&f.c.clone()));
    fs::remove_file(p).unwrap();
    bdsctl::admin::ensure_available(&f.c).unwrap();
}
#[test]
fn preflight_rejects_missing_candidate_and_insufficient_space_without_changes() {
    let mut f = Fixture::new();
    fs::create_dir_all(f.c.data()).unwrap();
    fs::write(f.c.data().join("world"), "data").unwrap();
    let r = releases::Release {
        id: "1.2.3.5".into(),
        url: "fixture".into(),
        sha256: "fixture".into(),
    };
    assert!(bdsctl::operations::capacity_preflight(&f.c, Some(&r)).is_err());
    f.c.maintenance.minimum_free_mib = u64::MAX;
    assert!(bdsctl::operations::capacity_preflight(&f.c, None).is_err());
    assert_eq!(
        fs::read_to_string(f.c.data().join("world")).unwrap(),
        "data"
    );
    assert!(!f.c.state().join("transaction.json").exists());
}
#[test]
fn old_snapshot_metadata_remains_readable_as_manual() {
    let f = Fixture::new();
    fs::create_dir_all(f.c.data()).unwrap();
    fs::write(f.c.data().join("world"), "data").unwrap();
    let s = snapshots::capture(&f.c, "1.2.3.4").unwrap();
    let p = f.c.snapshots().join(&s.id).join("snapshot.json");
    let mut value: serde_json::Value = util::read_json(&p).unwrap();
    value.as_object_mut().unwrap().remove("reason");
    util::json(&p, &value, 0o644).unwrap();
    assert_eq!(
        snapshots::load(&f.c, &s.id).unwrap().reason,
        snapshots::Reason::Manual
    );
    assert!(bdsctl::admin::snapshot_line(&s).contains("JST"));
}

#[test]
fn discord_explains_maintenance_without_creating_a_job() {
    let mut f = Fixture::new();
    f.c.discord.application_id = "1".into();
    f.c.discord.guild_id = "2".into();
    f.c.discord.allowed_users = vec!["4".into()];
    util::atomic(&bdsctl::admin::mode_path(&f.c), b"maintenance", 0o644).unwrap();
    let request = json!({"application_id":"1","guild_id":"2","member":{"user":{"id":"4"}},"type":2,"data":{"name":"restart"}});
    let (reply, launch) = discord::handle(&f.c, &request).unwrap();
    assert!(launch.is_none());
    assert!(reply["data"]["content"].as_str().unwrap().contains("保守"));
    assert!(!f.c.paths.discord_state.join("job.json").exists());
}

#[test]
fn live_capacity_estimate_tolerates_leveldb_file_replacement() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let f = Fixture::new();
    let db = f.c.data().join("worlds/db");
    fs::create_dir_all(&db).unwrap();
    fs::write(db.join("stable.ldb"), b"stable-data").unwrap();
    let running = Arc::new(AtomicBool::new(true));
    let flag = running.clone();
    let transient = db.clone();
    let worker = std::thread::spawn(move || {
        while flag.load(Ordering::Relaxed) {
            for i in 0..30 {
                let p = transient.join(format!("compacting-{i}.ldb"));
                fs::write(&p, b"replacement").unwrap();
                fs::remove_file(p).unwrap();
            }
        }
    });
    let result = (|| -> anyhow::Result<()> {
        for _ in 0..300 {
            assert!(util::estimate_tree_bytes(&f.c.data())? >= 11);
            bdsctl::operations::capacity_preflight(&f.c, None)?;
        }
        Ok(())
    })();
    running.store(false, Ordering::Relaxed);
    worker.join().unwrap();
    result.unwrap();
    assert_eq!(util::tree_bytes(&f.c.data()).unwrap(), 11);
    symlink("/etc/passwd", db.join("invalid-link")).unwrap();
    assert!(util::estimate_tree_bytes(&f.c.data()).is_err());
    assert!(util::files(&f.c.data()).is_err());
}

#[test]
fn legacy_vendor_comparison_rejects_deleted_added_and_changed_files() {
    let f = Fixture::new();
    let original = f.root.join("official-pack");
    fs::create_dir_all(original.join("nested")).unwrap();
    fs::write(original.join("nested/kept.json"), "original").unwrap();
    fs::write(original.join("removed.json"), "must remain").unwrap();
    let legacy = f.root.join("legacy-pack");
    util::copy_tree(&original, &legacy).unwrap();
    releases::verify_vendor_pack(&legacy, &original).unwrap();
    fs::remove_file(legacy.join("removed.json")).unwrap();
    assert!(releases::verify_vendor_pack(&legacy, &original).is_err());
    fs::copy(original.join("removed.json"), legacy.join("removed.json")).unwrap();
    fs::write(legacy.join("added.json"), "extra").unwrap();
    assert!(releases::verify_vendor_pack(&legacy, &original).is_err());
    fs::remove_file(legacy.join("added.json")).unwrap();
    fs::write(legacy.join("nested/kept.json"), "changed").unwrap();
    assert!(releases::verify_vendor_pack(&legacy, &original).is_err());
    fs::remove_dir_all(legacy).unwrap();
    assert!(releases::verify_vendor_pack(&f.root.join("legacy-pack"), &original).is_err());
}

#[test]
fn operation_records_keep_the_existing_wire_format_and_reject_unknown_states() {
    use bdsctl::operations::{Transaction, TransactionKind, TransactionPhase};
    let record = json!({
        "id":"20261031-000000", "kind":"maintenance", "phase":"applying",
        "old_release":"1.2.3.4", "snapshot":"20261030-000000", "was_active":true,
        "owner":123, "owner_start":"456", "boot":"previous-boot"
    });
    let transaction: Transaction = serde_json::from_value(record.clone()).unwrap();
    assert_eq!(transaction.kind, TransactionKind::Maintenance);
    assert_eq!(transaction.phase, TransactionPhase::Applying);
    assert_eq!(serde_json::to_value(transaction).unwrap(), record);
    for (field, unknown) in [("phase", "aplying"), ("kind", "remove")] {
        let mut invalid = record.clone();
        invalid[field] = json!(unknown);
        assert!(serde_json::from_value::<Transaction>(invalid).is_err());
    }
}

#[test]
fn corrupt_discord_job_is_not_overwritten_by_a_new_request() {
    let mut f = Fixture::new();
    f.c.discord.application_id = "1".into();
    f.c.discord.guild_id = "2".into();
    f.c.discord.allowed_users = vec!["4".into()];
    let request = json!({"application_id":"1","guild_id":"2","member":{"user":{"id":"4"}},"type":2,"data":{"name":"restart"}});
    let path = f.c.paths.discord_state.join("job.json");
    for bytes in [
        "broken JSON".to_string(),
        json!({"id":"old-request","user":"4","kind":"restart","phase":"unknown","created":0,"boot":util::boot()}).to_string(),
    ] {
        fs::write(&path, &bytes).unwrap();
        assert!(discord::handle(&f.c, &request).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), bytes);
    }
}

#[test]
fn config_defaults_are_generated_without_a_config_file_and_examples_only_override_discord() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_bdsctl"))
        .arg("--default-config")
        .current_dir(std::env::temp_dir())
        .output()
        .unwrap();
    assert!(output.status.success());
    let defaults: Config = toml::from_str(std::str::from_utf8(&output.stdout).unwrap()).unwrap();
    defaults.validate().unwrap();
    let expected = serde_json::to_value(Config::default()).unwrap();
    assert_eq!(serde_json::to_value(defaults).unwrap(), expected);
    let example: Config = toml::from_str(include_str!("../bds.toml.example")).unwrap();
    example.validate().unwrap();
    let example = serde_json::to_value(example).unwrap();
    for section in ["paths", "server", "maintenance"] {
        assert_eq!(example[section], expected[section]);
    }
    assert_eq!(example["discord"]["enabled"], true);
}

#[test]
fn legacy_preflight_accepts_only_exact_historical_files_without_touching_source() {
    use bdsctl::operations;
    let f = Fixture::new();
    let legacy = f.root.join("legacy");
    let current = f.c.paths.releases.join("1.2.3.4");
    let previous = f.c.paths.releases.join("1.2.3.3");
    for (root, id) in [(&current, "1.2.3.4"), (&previous, "1.2.3.3")] {
        fs::create_dir_all(root.join("behavior_packs/vanilla")).unwrap();
        util::json(
            &root.join("release.json"),
            &json!({"id":id,"url":"fixture","sha256":id}),
            0o644,
        )
        .unwrap();
        fs::write(root.join("bedrock_server"), "fixture").unwrap();
    }
    fs::write(
        current.join("behavior_packs/vanilla/kept.json"),
        "current bytes",
    )
    .unwrap();
    fs::write(
        previous.join("behavior_packs/vanilla/old.json"),
        "historical bytes",
    )
    .unwrap();
    fs::create_dir(&legacy).unwrap();
    util::copy_tree(
        &current.join("behavior_packs"),
        &legacy.join("behavior_packs"),
    )
    .unwrap();
    fs::write(legacy.join(".installed-version"), "1.2.3.4").unwrap();
    let old = legacy.join("behavior_packs/vanilla/old.json");
    fs::write(&old, "historical bytes").unwrap();
    operations::prepare_legacy(&f.c, &legacy, "1.2.3.3").unwrap();
    let plan = f.c.state().join("legacy-plan.json");
    let record: serde_json::Value = util::read_json(&plan).unwrap();
    assert_eq!(record["extras"].as_object().unwrap().len(), 1);
    assert_eq!(fs::read(&old).unwrap(), b"historical bytes");
    assert!(!f.c.data().exists() && !f.c.current().exists());
    // Neither unknown additions nor changes to current official files can be approved.
    fs::write(&old, "custom bytes").unwrap();
    assert!(operations::prepare_legacy(&f.c, &legacy, "1.2.3.3").is_err());
    assert!(!plan.exists());
    fs::write(&old, "historical bytes").unwrap();
    fs::write(
        legacy.join("behavior_packs/vanilla/kept.json"),
        "custom current",
    )
    .unwrap();
    assert!(operations::prepare_legacy(&f.c, &legacy, "1.2.3.3").is_err());
    fs::remove_file(legacy.join("behavior_packs/vanilla/kept.json")).unwrap();
    assert!(operations::prepare_legacy(&f.c, &legacy, "1.2.3.3").is_err());
    assert!(!plan.exists() && !f.c.data().exists());
}

#[test]
fn execution_copy_is_writable_disposable_and_preserves_canonical_data() {
    let f = Fixture::new();
    fs::create_dir_all(f.c.data().join("worlds")).unwrap();
    fs::write(f.c.data().join("worlds/data"), "canonical-world").unwrap();
    let vendor = f.c.paths.releases.join("1.2.3.4");
    fs::create_dir_all(vendor.join("resource_packs/vanilla")).unwrap();
    fs::write(
        vendor.join("resource_packs/vanilla/manifest.json"),
        "official-pack",
    )
    .unwrap();
    fs::write(vendor.join("bedrock_server"), "official-executable").unwrap();
    fs::write(
        vendor.join("release.json"),
        r#"{"id":"1.2.3.4","url":"fixture","sha256":"fixture"}"#,
    )
    .unwrap();
    releases::prepare(&f.c, "1.2.3.4").unwrap();
    let work = releases::working(&f.c, "1.2.3.4");
    assert_eq!(
        fs::read_link(work.join("worlds")).unwrap(),
        f.c.data().join("worlds")
    );
    assert!(!vendor.join("worlds").exists());
    fs::create_dir(work.join("new-engine-cache")).unwrap();
    fs::write(work.join("new-engine-cache/value"), "keep-across-restarts").unwrap();
    fs::write(
        work.join("resource_packs/vanilla/manifest.json"),
        "engine-write",
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(vendor.join("resource_packs/vanilla/manifest.json")).unwrap(),
        "official-pack"
    );
    releases::prepare(&f.c, "1.2.3.4").unwrap();
    assert_eq!(
        fs::read_to_string(work.join("new-engine-cache/value")).unwrap(),
        "keep-across-restarts"
    );
    let snapshot = snapshots::capture(&f.c, "1.2.3.4").unwrap();
    assert!(snapshot.files.keys().all(|p| !p.starts_with("engine")));
    fs::remove_dir_all(&work).unwrap();
    releases::prepare(&f.c, "1.2.3.4").unwrap();
    assert_eq!(
        fs::read_to_string(work.join("worlds/data")).unwrap(),
        "canonical-world"
    );
    assert_eq!(
        fs::read_to_string(work.join("resource_packs/vanilla/manifest.json")).unwrap(),
        "official-pack"
    );
}

#[test]
fn failed_execution_copy_is_not_published_and_can_be_rebuilt() {
    let f = Fixture::new();
    fs::create_dir_all(f.c.data()).unwrap();
    let vendor = f.c.paths.releases.join("1.2.3.4");
    fs::create_dir_all(&vendor).unwrap();
    fs::write(vendor.join("bedrock_server"), "fixture").unwrap();
    fs::write(vendor.join("release.json"), "{}").unwrap();
    fs::write(vendor.join("world_templates"), "invalid directory").unwrap();
    assert!(releases::prepare(&f.c, "1.2.3.4").is_err());
    assert!(!releases::working(&f.c, "1.2.3.4").exists());
    assert!(fs::read_dir(&f.c.paths.store).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".engine-")
    }));
    fs::remove_file(vendor.join("world_templates")).unwrap();
    releases::prepare(&f.c, "1.2.3.4").unwrap();
    assert!(
        releases::working(&f.c, "1.2.3.4")
            .join("world_templates")
            .is_dir()
    );
}

#[test]
fn server_identity_survives_release_changes_engine_rebuilds_and_backup_restore() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use ring::{rand::SystemRandom, signature};
    use std::os::unix::fs::PermissionsExt;

    let f = Fixture::new();
    fs::create_dir_all(f.c.data()).unwrap();
    for id in ["1.2.3.4", "1.2.3.5"] {
        let vendor = f.c.paths.releases.join(id);
        fs::create_dir(&vendor).unwrap();
        fs::write(vendor.join("bedrock_server"), "fixture").unwrap();
        fs::write(vendor.join("release.json"), "{}").unwrap();
        releases::prepare(&f.c, id).unwrap();
    }
    let key = f.c.data().join("keys/server_identity_key.pem");
    let original = fs::read_to_string(&key).unwrap();
    let der = STANDARD
        .decode(
            original
                .lines()
                .filter(|s| !s.starts_with("-----"))
                .collect::<String>(),
        )
        .unwrap();
    signature::EcdsaKeyPair::from_pkcs8(
        &signature::ECDSA_P384_SHA384_ASN1_SIGNING,
        &der,
        &SystemRandom::new(),
    )
    .unwrap();
    assert_eq!(
        fs::metadata(&key).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(key.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    for id in ["1.2.3.4", "1.2.3.5"] {
        let work = releases::working(&f.c, id);
        assert_eq!(
            fs::read_link(work.join("keys")).unwrap(),
            f.c.data().join("keys")
        );
        assert!(fs::read_to_string(work.join("keys/server_identity_key.pem")).unwrap() == original);
        fs::remove_dir_all(work).unwrap();
        releases::prepare(&f.c, id).unwrap();
        assert!(fs::read_to_string(&key).unwrap() == original);
    }
    let snapshot = snapshots::capture(&f.c, "1.2.3.5").unwrap();
    assert!(snapshot.files.contains_key("keys/server_identity_key.pem"));
    let restored = snapshots::prepare_restore(&f.c, &snapshot.id).unwrap();
    let restored_key = restored.join("keys/server_identity_key.pem");
    assert!(fs::read_to_string(&restored_key).unwrap() == original);
    assert_ne!(
        fs::metadata(&key).unwrap().ino(),
        fs::metadata(restored_key).unwrap().ino()
    );
    // An existing engine-local identity is moved, never discarded or replaced.
    let work = releases::working(&f.c, "1.2.3.5");
    fs::remove_file(work.join("keys")).unwrap();
    fs::rename(f.c.data().join("keys"), work.join("keys")).unwrap();
    releases::prepare(&f.c, "1.2.3.5").unwrap();
    assert!(fs::read_to_string(&key).unwrap() == original);
    assert_eq!(
        fs::read_link(work.join("keys")).unwrap(),
        f.c.data().join("keys")
    );
    fs::remove_file(work.join("keys")).unwrap();
    fs::create_dir(work.join("keys")).unwrap();
    fs::write(work.join("keys/server_identity_key.pem"), "conflicting key").unwrap();
    assert!(releases::prepare(&f.c, "1.2.3.5").is_err());
    assert!(fs::read_to_string(&key).unwrap() == original);
    fs::remove_dir_all(work.join("keys")).unwrap();
    fs::create_dir(work.join("keys")).unwrap();
    releases::prepare(&f.c, "1.2.3.5").unwrap();
    assert!(fs::read_to_string(&key).unwrap() == original);
    // Invalid existing storage must not cause an automatic key replacement.
    fs::write(&key, "").unwrap();
    assert!(releases::prepare(&f.c, "1.2.3.5").is_err());
    assert_eq!(fs::read(&key).unwrap(), b"");
}
