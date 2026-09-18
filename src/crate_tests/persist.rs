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
        .apply_action(Action {
            id: "act-s2".into(),
            kind: "Shipment".into(),
            key: "s2".into(),
            props: HashMap::from([
                ("order_id".into(), "o1".into()),
                ("amount".into(), "5".into()),
            ]),
        })
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
    assert!(store.joins().is_visible("Asset", "a1"));
    assert_eq!(store.joins().prop("Asset", "a1", "owner_id"), Some("c1"));
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert!(reopened.joins().is_visible("Asset", "a1"));
    assert_eq!(reopened.joins().prop("Asset", "a1", "owner_id"), Some("c1"));
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
    assert!(!reopened.joins().is_visible("Customer", "c0"));
    assert!(!reopened.joins().is_visible("Order", "o0"));
    assert!(!reopened.joins().is_visible("Shipment", "s0"));
    assert!(!reopened.joins().is_visible("Asset", "a0"));
    assert!(reopened.joins().is_visible("Customer", "c1"));
    assert!(reopened.joins().is_visible("Asset", "a1"));
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
    assert!(reopened.joins().is_visible("Customer", "c1"));
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
    std::fs::remove_file(&sidecar).unwrap();
    let recovered = Store::open(&log).unwrap();
    assert!(recovered.joins().is_visible("Customer", "c1"));
    let _ = std::fs::remove_dir_all(&dir);
}
