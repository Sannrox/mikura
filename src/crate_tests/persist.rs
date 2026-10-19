use super::super::*;
use super::helpers::*;

#[test]
fn ingest_evaluate_rebuild_acl_action_and_spark_fail_closed() {
    let (dir, log) = temp_log("v1-lib");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());

    let oss = ObjectSet::new(LocalCompute);
    let visible = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(visible.two_hop_count, 1);
    assert_eq!(visible.sum_amount, 10);

    store
        .apply_action(
            Action {
                namespace: NS.into(),
                id: "act-s2".into(),
                kind: "Shipment".into(),
                key: "s2".into(),
                props: HashMap::from([
                    ("order_id".into(), "o1".into()),
                    ("amount".into(), "5".into()),
                ]),
            },
            None,
        )
        .unwrap();
    let after = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(after.sum_amount, 15);

    let mut denied_req = fixture_request();
    denied_req.acl = PropertyAcl::deny_property("Shipment", "amount");
    let denied = oss.evaluate(&store, &denied_req);
    assert!(matches!(
        denied,
        Err(ComputeError::Acl(AclError::Denied { .. }))
    ));

    let rebuilt = Store::open(&log).unwrap();
    let from_log = oss.evaluate(&rebuilt, &fixture_request()).unwrap();
    assert_eq!(from_log.two_hop_count, after.two_hop_count);
    assert_eq!(from_log.sum_amount, after.sum_amount);

    let spark = ObjectSet::new(SparkCompute);
    let err = spark.evaluate(&store, &fixture_request()).unwrap_err();
    assert!(matches!(err, ComputeError::UnsupportedBackend { .. }));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn persist_reopen_hop_count_and_sum_match_live_evaluate() {
    let (dir, log) = temp_log("join-reopen");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, generic_records());
    let oss = ObjectSet::new(LocalCompute);
    let live = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(live.two_hop_count, 1);
    assert_eq!(live.sum_amount, 10);
    let live_asset = oss.evaluate(&store, &asset_request()).unwrap();
    assert_eq!(live_asset.two_hop_count, 1);
    assert_eq!(live_asset.sum_amount, 4);
    assert!(Store::join_map_path(&log).is_file());
    drop(store);

    let reopened = Store::open(&log).unwrap();
    let from_maps = oss.evaluate(&reopened, &fixture_request()).unwrap();
    assert_eq!(from_maps.two_hop_count, live.two_hop_count);
    assert_eq!(from_maps.sum_amount, live.sum_amount);
    let from_maps_asset = oss.evaluate(&reopened, &asset_request()).unwrap();
    assert_eq!(from_maps_asset.two_hop_count, live_asset.two_hop_count);
    assert_eq!(from_maps_asset.sum_amount, live_asset.sum_amount);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sidecar_stamp_pages_tracks_checkpoint_and_delta() {
    let (dir, log) = temp_log("sidecar-stamp");
    let mut store = Store::create(&log).unwrap();
    for record in generic_records() {
        store.append_uncommitted(record).unwrap();
    }
    store.commit().unwrap();
    let checkpoint_pages = store.committed_pages();
    assert!(!Store::join_delta_path(&log).exists());
    assert_eq!(store.sidecar_stamp_pages(), checkpoint_pages);
    store
        .append(rec(
            "Shipment",
            "s2",
            false,
            &[("order_id", "o1"), ("amount", "5")],
        ))
        .unwrap();
    let with_delta = store.committed_pages();
    assert!(with_delta > checkpoint_pages);
    assert!(Store::join_delta_path(&log).is_file());
    assert_eq!(store.sidecar_stamp_pages(), with_delta);
    std::fs::remove_file(Store::join_delta_path(&log)).unwrap();
    assert_eq!(store.sidecar_stamp_pages(), checkpoint_pages);
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    assert_eq!(store.sidecar_stamp_pages(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dual_read_projection_equals_log_replay() {
    let (dir, log) = temp_log("join-dual-read");
    let sidecar = Store::join_map_path(&log);
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, generic_records());
    let oss = ObjectSet::new(LocalCompute);
    let from_projection = oss.evaluate(&store, &fixture_request()).unwrap();
    let from_projection_asset = oss.evaluate(&store, &asset_request()).unwrap();
    drop(store);

    std::fs::remove_file(&sidecar).unwrap();
    let replayed = Store::open(&log).unwrap();
    let from_log = oss.evaluate(&replayed, &fixture_request()).unwrap();
    let from_log_asset = oss.evaluate(&replayed, &asset_request()).unwrap();
    assert_eq!(from_log.two_hop_count, from_projection.two_hop_count);
    assert_eq!(from_log.sum_amount, from_projection.sum_amount);
    assert_eq!(
        from_log_asset.two_hop_count,
        from_projection_asset.two_hop_count
    );
    assert_eq!(from_log_asset.sum_amount, from_projection_asset.sum_amount);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn generic_kind_join_property_survives_reopen() {
    let (dir, log) = temp_log("join-generic");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, generic_records());
    assert!(store.joins(NS).unwrap().is_visible("Asset", "a1"));
    assert_eq!(
        store.joins(NS).unwrap().prop("Asset", "a1", "owner_id"),
        Some("c1")
    );
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert!(reopened.joins(NS).unwrap().is_visible("Asset", "a1"));
    assert_eq!(
        reopened.joins(NS).unwrap().prop("Asset", "a1", "owner_id"),
        Some("c1")
    );
    let oss = ObjectSet::new(LocalCompute);
    let response = oss.evaluate(&reopened, &asset_request()).unwrap();
    assert_eq!(response.two_hop_count, 1);
    assert_eq!(response.sum_amount, 4);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hidden_keys_absent_from_join_maps_after_reopen() {
    let (dir, log) = temp_log("join-hidden");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, generic_records());
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert!(!reopened.joins(NS).unwrap().is_visible("Customer", "c0"));
    assert!(!reopened.joins(NS).unwrap().is_visible("Order", "o0"));
    assert!(!reopened.joins(NS).unwrap().is_visible("Shipment", "s0"));
    assert!(!reopened.joins(NS).unwrap().is_visible("Asset", "a0"));
    assert!(reopened.joins(NS).unwrap().is_visible("Customer", "c1"));
    assert!(reopened.joins(NS).unwrap().is_visible("Asset", "a1"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn join_sidecar_checksum_mismatch_and_truncate_fail_closed() {
    let (dir, log) = temp_log("join-checksum");
    let sidecar = Store::join_map_path(&log);
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, generic_records());
    let oss = ObjectSet::new(LocalCompute);
    let live = oss.evaluate(&store, &fixture_request()).unwrap();
    drop(store);

    let good = std::fs::read(&sidecar).unwrap();
    let mut flipped = good.clone();
    let last = flipped.len() - 5;
    flipped[last] ^= 0x01;
    std::fs::write(&sidecar, &flipped).unwrap();
    let flipped_err = match Store::open(&log) {
        Err(err) => err,
        Ok(_) => panic!("bit-flip should fail closed"),
    };
    assert!(
        flipped_err.contains("checksum"),
        "bit-flip should fail closed: {flipped_err}"
    );

    std::fs::write(&sidecar, &good[..3]).unwrap();
    let truncated_err = match Store::open(&log) {
        Err(err) => err,
        Ok(_) => panic!("truncate should fail closed"),
    };
    assert!(
        truncated_err.contains("short") || truncated_err.contains("checksum"),
        "truncate should fail closed: {truncated_err}"
    );

    std::fs::remove_file(&sidecar).unwrap();
    let recovered = Store::open(&log).unwrap();
    let from_log = oss.evaluate(&recovered, &fixture_request()).unwrap();
    assert_eq!(from_log.two_hop_count, live.two_hop_count);
    assert_eq!(from_log.sum_amount, live.sum_amount);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stale_join_sidecar_rebuilds_from_log() {
    let (dir, log) = temp_log("join-stale");
    let sidecar = Store::join_map_path(&log);
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    drop(store);
    let stale = std::fs::read(&sidecar).unwrap();

    let mut store = Store::open(&log).unwrap();
    store
        .append(rec(
            "Shipment",
            "s2",
            false,
            &[("order_id", "o1"), ("amount", "5")],
        ))
        .unwrap();
    drop(store);
    std::fs::write(&sidecar, stale).unwrap();

    let reopened = Store::open(&log).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let response = oss.evaluate(&reopened, &fixture_request()).unwrap();
    assert_eq!(response.two_hop_count, 1);
    assert_eq!(response.sum_amount, 15);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn single_record_append_is_durable_across_reopen() {
    let (dir, log) = temp_log("group-commit-single");
    let mut store = Store::create(&log).unwrap();
    store
        .append(rec("Customer", "c1", false, &[("region", "us")]))
        .unwrap();
    drop(store);
    let reopened = Store::open(&log).unwrap();
    assert!(reopened.joins(NS).unwrap().is_visible("Customer", "c1"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mkjoin05_sidecar_still_opens() {
    let (dir, log) = temp_log("join-mkjoin05");
    let sidecar = Store::join_map_path(&log);
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    drop(store);
    let mut bytes = std::fs::read(&sidecar).unwrap();
    let crc_at = bytes.len() - 4;
    bytes[..8].copy_from_slice(JOIN_FORMAT_V5);
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&bytes[..crc_at]);
    bytes[crc_at..].copy_from_slice(&hasher.finalize().to_le_bytes());
    std::fs::write(&sidecar, &bytes).unwrap();
    let reopened = Store::open(&log).unwrap();
    assert_eq!(
        ObjectSet::new(LocalCompute)
            .evaluate(&reopened, &fixture_request())
            .unwrap()
            .sum_amount,
        10
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn old_join_sidecar_magic_fails_closed() {
    let (dir, log) = temp_log("join-old-magic");
    let sidecar = Store::join_map_path(&log);
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    drop(store);
    let mut bytes = std::fs::read(&sidecar).unwrap();
    let crc_at = bytes.len() - 4;
    bytes[..8].copy_from_slice(b"MKJOIN01");
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&bytes[..crc_at]);
    bytes[crc_at..].copy_from_slice(&hasher.finalize().to_le_bytes());
    std::fs::write(&sidecar, &bytes).unwrap();
    let err = match Store::open(&log) {
        Err(err) => err,
        Ok(_) => panic!("old magic should fail closed"),
    };
    assert!(err.contains("magic") || err.contains("checksum"), "{err}");
    bytes[..8].copy_from_slice(b"MKJOIN02");
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&bytes[..crc_at]);
    bytes[crc_at..].copy_from_slice(&hasher.finalize().to_le_bytes());
    std::fs::write(&sidecar, &bytes).unwrap();
    let err02 = match Store::open(&log) {
        Err(err) => err,
        Ok(_) => panic!("MKJOIN02 should fail closed"),
    };
    assert!(
        err02.contains("magic") || err02.contains("checksum"),
        "{err02}"
    );
    bytes[..8].copy_from_slice(b"MKJOIN03");
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&bytes[..crc_at]);
    bytes[crc_at..].copy_from_slice(&hasher.finalize().to_le_bytes());
    std::fs::write(&sidecar, &bytes).unwrap();
    let err03 = match Store::open(&log) {
        Err(err) => err,
        Ok(_) => panic!("MKJOIN03 should fail closed"),
    };
    assert!(
        err03.contains("magic") || err03.contains("checksum"),
        "{err03}"
    );
    bytes[..8].copy_from_slice(b"MKJOIN04");
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&bytes[..crc_at]);
    bytes[crc_at..].copy_from_slice(&hasher.finalize().to_le_bytes());
    std::fs::write(&sidecar, &bytes).unwrap();
    let err04 = match Store::open(&log) {
        Err(err) => err,
        Ok(_) => panic!("MKJOIN04 should fail closed"),
    };
    assert!(
        err04.contains("magic") || err04.contains("checksum"),
        "{err04}"
    );
    std::fs::remove_file(&sidecar).unwrap();
    let recovered = Store::open(&log).unwrap();
    assert!(recovered.joins(NS).unwrap().is_visible("Customer", "c1"));
    let _ = std::fs::remove_dir_all(&dir);
}

fn isolated_log(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "mikura-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("objects.mikura");
    (dir, log)
}

fn seed_checkpoint(store: &mut Store) -> u32 {
    for record in generic_records() {
        store.append_uncommitted(record).unwrap();
    }
    store.commit().unwrap();
    let pages = store.committed_pages();
    assert_eq!(store.sidecar_stamp_pages(), pages);
    store.set_sidecar_persist(u32::MAX, std::time::Duration::from_secs(3600));
    pages
}

fn append_second_shipment(store: &mut Store) {
    store
        .append(rec(
            "Shipment",
            "s2",
            false,
            &[("order_id", "o1"), ("amount", "5")],
        ))
        .unwrap();
}

/// Log commit is durable even when sidecar persist is skipped. Forget is the
/// crash: Drop would persist.
#[test]
fn kill_between_log_commit_and_sidecar_persist_reopens_from_log() {
    let (dir, log) = isolated_log("sidecar-crash");
    let mut store = Store::create(&log).unwrap();
    let checkpoint_pages = seed_checkpoint(&mut store);
    append_second_shipment(&mut store);
    let committed = store.committed_pages();
    assert!(committed > checkpoint_pages);
    assert_eq!(store.sidecar_stamp_pages(), checkpoint_pages);
    assert!(
        !Store::join_delta_path(&log).exists(),
        "sidecar must lag the log until persist"
    );
    std::mem::forget(store);

    let reopened = Store::open(&log).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let response = oss.evaluate(&reopened, &fixture_request()).unwrap();
    assert_eq!(response.two_hop_count, 1);
    assert_eq!(response.sum_amount, 15);
    drop(reopened);

    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let _ = std::fs::remove_file(Store::join_delta_path(&log));
    let replayed = Store::open(&log).unwrap();
    let from_log = oss.evaluate(&replayed, &fixture_request()).unwrap();
    assert_eq!(from_log.two_hop_count, 1);
    assert_eq!(from_log.sum_amount, 15);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deferred_sidecar_persist_runs_on_close() {
    let (dir, log) = isolated_log("sidecar-close");
    let mut store = Store::create(&log).unwrap();
    let checkpoint_pages = seed_checkpoint(&mut store);
    append_second_shipment(&mut store);
    assert_eq!(store.sidecar_stamp_pages(), checkpoint_pages);
    assert!(!Store::join_delta_path(&log).exists());
    drop(store);
    assert!(
        Store::join_delta_path(&log).is_file(),
        "close must persist the lagging dirty set"
    );

    let reopened = Store::open(&log).unwrap();
    assert_eq!(reopened.sidecar_stamp_pages(), reopened.committed_pages());
    let oss = ObjectSet::new(LocalCompute);
    let response = oss.evaluate(&reopened, &fixture_request()).unwrap();
    assert_eq!(response.two_hop_count, 1);
    assert_eq!(response.sum_amount, 15);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sidecar_persist_after_n_commits() {
    let (dir, log) = isolated_log("sidecar-n-commits");
    let mut store = Store::create(&log).unwrap();
    let checkpoint_pages = seed_checkpoint(&mut store);
    store.set_sidecar_persist(2, std::time::Duration::from_secs(3600));
    append_second_shipment(&mut store);
    assert_eq!(store.sidecar_stamp_pages(), checkpoint_pages);
    assert!(!Store::join_delta_path(&log).exists());
    store
        .append(rec("Customer", "c2", false, &[("region", "eu")]))
        .unwrap();
    assert!(Store::join_delta_path(&log).is_file());
    assert_eq!(store.sidecar_stamp_pages(), store.committed_pages());
    let _ = std::fs::remove_dir_all(&dir);
}
