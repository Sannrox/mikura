use super::*;
use std::collections::HashMap;

fn rec(kind: &str, key: &str, hidden: bool, props: &[(&str, &str)]) -> ObjectRecord {
    ObjectRecord {
        gen: 1,
        kind: kind.into(),
        key: key.into(),
        hidden,
        action_id: None,
        props: props
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect(),
    }
}

fn fixture_request() -> EvaluateRequest {
    EvaluateRequest {
        root_kind: "Customer".into(),
        hops: vec![
            Hop {
                far_kind: "Order".into(),
                join_property: "customer_id".into(),
                incoming: false,
            },
            Hop {
                far_kind: "Shipment".into(),
                join_property: "order_id".into(),
                incoming: false,
            },
        ],
        sum_kind: "Shipment".into(),
        sum_property: "amount".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        object_bound: 0,
    }
}

fn fixture() -> Vec<ObjectRecord> {
    vec![
        rec("Customer", "c0", true, &[("region", "eu")]),
        rec("Customer", "c1", false, &[("region", "us")]),
        rec("Order", "o1", false, &[("customer_id", "c1")]),
        rec("Order", "o0", true, &[("customer_id", "c0")]),
        rec(
            "Shipment",
            "s1",
            false,
            &[("order_id", "o1"), ("amount", "10")],
        ),
        rec(
            "Shipment",
            "s0",
            true,
            &[("order_id", "o0"), ("amount", "99")],
        ),
    ]
}

fn generic_records() -> Vec<ObjectRecord> {
    let mut records = fixture();
    records.push(rec(
        "Asset",
        "a1",
        false,
        &[("owner_id", "c1"), ("mass", "4")],
    ));
    records.push(rec(
        "Asset",
        "a0",
        true,
        &[("owner_id", "c0"), ("mass", "99")],
    ));
    records
}

fn asset_request() -> EvaluateRequest {
    EvaluateRequest {
        root_kind: "Customer".into(),
        hops: vec![Hop {
            far_kind: "Asset".into(),
            join_property: "owner_id".into(),
            incoming: false,
        }],
        sum_kind: "Asset".into(),
        sum_property: "mass".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        object_bound: 0,
    }
}

fn temp_log(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("mikura-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("objects.mikura");
    (dir, log)
}

fn append_all(store: &mut Store, records: Vec<ObjectRecord>) {
    for record in records {
        store.append(record).unwrap();
    }
}

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

#[test]
fn batch_commit_writes_delta_not_full_sidecar() {
    let (dir, log) = temp_log("join-delta");
    let sidecar = Store::join_map_path(&log);
    let delta = Store::join_delta_path(&log);
    let mut store = Store::create(&log).unwrap();
    for record in generic_records() {
        store.append_uncommitted(record).unwrap();
    }
    store.commit().unwrap();
    let checkpoint = std::fs::metadata(&sidecar).unwrap().len();
    assert!(!delta.exists());
    store
        .append(rec(
            "Shipment",
            "s2",
            false,
            &[("order_id", "o1"), ("amount", "5")],
        ))
        .unwrap();
    let after = std::fs::metadata(&sidecar).unwrap().len();
    assert_eq!(after, checkpoint, "checkpoint should not be rewritten");
    assert!(delta.is_file(), "dirty commit should write a delta");
    assert!(
        std::fs::metadata(&delta).unwrap().len() < checkpoint,
        "delta should be smaller than the checkpoint"
    );
    drop(store);
    let reopened = Store::open(&log).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let response = oss.evaluate(&reopened, &fixture_request()).unwrap();
    assert_eq!(response.two_hop_count, 1);
    assert_eq!(response.sum_amount, 15);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn load_current_object_after_reopen() {
    let (dir, log) = temp_log("load-reopen");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    let live_visible = store
        .load("Customer", "c1", &PropertyAcl::allow_all())
        .unwrap();
    let live_hidden = store
        .load("Customer", "c0", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(live_visible.gen, 1);
    assert!(!live_visible.hidden);
    assert_eq!(
        live_visible.props.get("region").map(String::as_str),
        Some("us")
    );
    assert_eq!(live_hidden.gen, 1);
    assert!(live_hidden.hidden);
    assert_eq!(
        live_hidden.props.get("region").map(String::as_str),
        Some("eu")
    );
    assert!(!store.joins().is_visible("Customer", "c0"));
    let missing = store
        .load("Customer", "missing", &PropertyAcl::allow_all())
        .unwrap_err();
    assert!(missing.contains("unknown identity"), "{missing}");
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert_eq!(
        reopened
            .load("Customer", "c1", &PropertyAcl::allow_all())
            .unwrap(),
        live_visible
    );
    assert_eq!(
        reopened
            .load("Customer", "c0", &PropertyAcl::allow_all())
            .unwrap(),
        live_hidden
    );
    assert!(!reopened.joins().is_visible("Customer", "c0"));
    drop(reopened);

    let mut store = Store::open(&log).unwrap();
    store
        .append(rec("Customer", "c1", false, &[("region", "ap")]))
        .unwrap();
    drop(store);
    let updated = Store::open(&log).unwrap();
    let latest = updated
        .load("Customer", "c1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(latest.gen, 2);
    assert_eq!(latest.props.get("region").map(String::as_str), Some("ap"));

    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    assert_eq!(
        replayed
            .load("Customer", "c1", &PropertyAcl::allow_all())
            .unwrap(),
        latest
    );
    assert_eq!(
        replayed
            .load("Customer", "c0", &PropertyAcl::allow_all())
            .unwrap(),
        live_hidden
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn load_omits_denied_properties() {
    let (dir, log) = temp_log("load-acl");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    let allow = PropertyAcl::allow_all();
    let live = store.load("Shipment", "s1", &allow).unwrap();
    assert_eq!(live.props.get("amount").map(String::as_str), Some("10"));
    assert_eq!(live.props.get("order_id").map(String::as_str), Some("o1"));

    let deny_unknown = PropertyAcl::deny_property("Shipment", "not_a_column");
    assert_eq!(
        store.load("Shipment", "s1", &deny_unknown).unwrap().props,
        live.props
    );

    let deny_amount = PropertyAcl::deny_property("Shipment", "amount");
    let redacted = store.load("Shipment", "s1", &deny_amount).unwrap();
    assert!(!redacted.props.contains_key("amount"));
    assert_eq!(
        redacted.props.get("order_id").map(String::as_str),
        Some("o1")
    );
    assert!(!redacted.props.contains_key("fabricated"));

    let hidden = store
        .load(
            "Customer",
            "c0",
            &PropertyAcl::deny_property("Customer", "region"),
        )
        .unwrap();
    assert!(hidden.hidden);
    assert!(!hidden.props.contains_key("region"));
    assert!(!store.joins().is_visible("Customer", "c0"));

    let oss = ObjectSet::new(LocalCompute);
    let mut denied_req = fixture_request();
    denied_req.acl = PropertyAcl::deny_property("Shipment", "amount");
    assert!(matches!(
        oss.evaluate(&store, &denied_req),
        Err(ComputeError::Acl(AclError::Denied { .. }))
    ));
    drop(store);

    let reopened = Store::open(&log).unwrap();
    let after_open = reopened.load("Shipment", "s1", &deny_amount).unwrap();
    assert!(!after_open.props.contains_key("amount"));
    assert_eq!(
        after_open.props.get("order_id").map(String::as_str),
        Some("o1")
    );
    assert_eq!(
        reopened.load("Shipment", "s1", &allow).unwrap().props,
        live.props
    );
    drop(reopened);

    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    let after_replay = replayed.load("Shipment", "s1", &deny_amount).unwrap();
    assert!(!after_replay.props.contains_key("amount"));
    assert_eq!(
        after_replay.props.get("order_id").map(String::as_str),
        Some("o1")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn incoming_hop_follows_join_property() {
    let (dir, log) = temp_log("incoming-hop");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    let oss = ObjectSet::new(LocalCompute);
    let outgoing = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(outgoing.two_hop_count, 1);
    assert_eq!(outgoing.sum_amount, 10);
    let from_customer = EvaluateRequest {
        root_kind: "Customer".into(),
        hops: vec![Hop {
            far_kind: "Order".into(),
            join_property: "customer_id".into(),
            incoming: false,
        }],
        sum_kind: "Order".into(),
        sum_property: "customer_id".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        object_bound: 0,
    };
    assert_eq!(
        oss.evaluate(&store, &from_customer).unwrap().two_hop_count,
        1
    );
    let incoming = EvaluateRequest {
        root_kind: "Order".into(),
        hops: vec![Hop {
            far_kind: "Customer".into(),
            join_property: "customer_id".into(),
            incoming: true,
        }],
        sum_kind: "Customer".into(),
        sum_property: "region".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        object_bound: 0,
    };
    let response = oss.evaluate(&store, &incoming).unwrap();
    assert_eq!(response.two_hop_count, 1);
    assert!(!store.joins().is_visible("Order", "o0"));
    assert!(!store.joins().is_visible("Customer", "c0"));
    let denied = EvaluateRequest {
        root_kind: "Order".into(),
        hops: vec![Hop {
            far_kind: "Customer".into(),
            join_property: "customer_id".into(),
            incoming: true,
        }],
        sum_kind: "Customer".into(),
        sum_property: "region".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::deny_property("Customer", "region"),
        filter: None,
        object_bound: 0,
    };
    assert!(matches!(
        oss.evaluate(&store, &denied),
        Err(ComputeError::Acl(AclError::Denied { .. }))
    ));
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert_eq!(oss.evaluate(&reopened, &incoming).unwrap().two_hop_count, 1);
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    assert_eq!(oss.evaluate(&replayed, &incoming).unwrap().two_hop_count, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn exact_match_filter_restricts_roots() {
    let (dir, log) = temp_log("filter-roots");
    let mut store = Store::create(&log).unwrap();
    append_all(
        &mut store,
        vec![
            rec("Customer", "c0", true, &[("region", "eu")]),
            rec("Customer", "c1", false, &[("region", "us")]),
            rec("Customer", "c2", false, &[("region", "eu")]),
            rec("Order", "o1", false, &[("customer_id", "c1")]),
            rec("Order", "o2", false, &[("customer_id", "c2")]),
            rec(
                "Shipment",
                "s1",
                false,
                &[("order_id", "o1"), ("amount", "10")],
            ),
            rec(
                "Shipment",
                "s2",
                false,
                &[("order_id", "o2"), ("amount", "7")],
            ),
        ],
    );
    let oss = ObjectSet::new(LocalCompute);
    let unfiltered = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(unfiltered.two_hop_count, 2);
    assert_eq!(unfiltered.sum_amount, 17);

    let mut us = fixture_request();
    us.filter = Some(ExactMatch {
        property: "region".into(),
        value: "us".into(),
    });
    let us_resp = oss.evaluate(&store, &us).unwrap();
    assert_eq!(us_resp.two_hop_count, 1);
    assert_eq!(us_resp.sum_amount, 10);

    let mut eu = fixture_request();
    eu.filter = Some(ExactMatch {
        property: "region".into(),
        value: "eu".into(),
    });
    let eu_resp = oss.evaluate(&store, &eu).unwrap();
    assert_eq!(eu_resp.two_hop_count, 1);
    assert_eq!(eu_resp.sum_amount, 7);
    assert!(!store.joins().is_visible("Customer", "c0"));

    let mut miss = fixture_request();
    miss.filter = Some(ExactMatch {
        property: "region".into(),
        value: "ap".into(),
    });
    let empty = oss.evaluate(&store, &miss).unwrap();
    assert_eq!(empty.two_hop_count, 0);
    assert_eq!(empty.sum_amount, 0);

    let mut denied = us.clone();
    denied.acl = PropertyAcl::deny_property("Customer", "region");
    assert!(matches!(
        oss.evaluate(&store, &denied),
        Err(ComputeError::Acl(AclError::Denied { .. }))
    ));

    drop(store);
    let reopened = Store::open(&log).unwrap();
    assert_eq!(oss.evaluate(&reopened, &us).unwrap(), us_resp);
    assert!(!reopened.joins().is_visible("Customer", "c0"));
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    assert_eq!(oss.evaluate(&replayed, &us).unwrap(), us_resp);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn exact_match_filter_uses_by_prop_among_many_roots() {
    let (dir, log) = temp_log("filter-by-prop");
    let mut store = Store::create(&log).unwrap();
    let mut records = vec![
        rec("Customer", "keep", false, &[("region", "us")]),
        rec("Order", "o1", false, &[("customer_id", "keep")]),
        rec(
            "Shipment",
            "s1",
            false,
            &[("order_id", "o1"), ("amount", "4")],
        ),
    ];
    for i in 0..64 {
        records.push(rec(
            "Customer",
            &format!("other-{i}"),
            false,
            &[("region", "eu")],
        ));
    }
    append_all(&mut store, records);
    let mut request = fixture_request();
    request.filter = Some(ExactMatch {
        property: "region".into(),
        value: "us".into(),
    });
    let response = ObjectSet::new(LocalCompute)
        .evaluate(&store, &request)
        .unwrap();
    assert_eq!(response.two_hop_count, 1);
    assert_eq!(response.sum_amount, 4);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hop_fan_out_sums_every_leaf_path() {
    let (dir, log) = temp_log("hop-fan-out");
    let mut store = Store::create(&log).unwrap();
    append_all(
        &mut store,
        vec![
            rec("Customer", "c1", false, &[("region", "us")]),
            rec("Order", "o1", false, &[("customer_id", "c1")]),
            rec(
                "Shipment",
                "s1",
                false,
                &[("order_id", "o1"), ("amount", "10")],
            ),
            rec(
                "Shipment",
                "s2",
                false,
                &[("order_id", "o1"), ("amount", "7")],
            ),
        ],
    );
    let oss = ObjectSet::new(LocalCompute);
    let response = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(response.two_hop_count, 1);
    assert_eq!(response.sum_amount, 17);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hop_diamond_counts_distinct_roots_once() {
    let (dir, log) = temp_log("hop-diamond");
    let mut store = Store::create(&log).unwrap();
    append_all(
        &mut store,
        vec![
            rec("Customer", "c1", false, &[("region", "us")]),
            rec("Order", "o1", false, &[("customer_id", "c1")]),
            rec("Order", "o2", false, &[("customer_id", "c1")]),
            rec(
                "Shipment",
                "s1",
                false,
                &[("order_id", "o1"), ("amount", "10")],
            ),
            rec(
                "Shipment",
                "s2",
                false,
                &[("order_id", "o2"), ("amount", "3")],
            ),
        ],
    );
    let oss = ObjectSet::new(LocalCompute);
    let response = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(response.two_hop_count, 1);
    assert_eq!(response.sum_amount, 13);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn action_id_round_trips_and_empty_id_fails_closed() {
    let (dir, log) = temp_log("action-id");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
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
    let live = store
        .load("Shipment", "s2", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(live.action_id.as_deref(), Some("act-s2"));
    for i in 0..8 {
        store
            .apply_action(Action {
                id: format!("act-s2-{i}"),
                kind: "Shipment".into(),
                key: "s2".into(),
                props: HashMap::from([
                    ("order_id".into(), "o1".into()),
                    ("amount".into(), "5".into()),
                ]),
            })
            .unwrap();
    }
    assert_eq!(
        store
            .load("Shipment", "s2", &PropertyAcl::allow_all())
            .unwrap()
            .action_id
            .as_deref(),
        Some("act-s2-7")
    );
    assert_eq!(
        store
            .load("Customer", "c1", &PropertyAcl::allow_all())
            .unwrap()
            .action_id,
        None
    );
    let missing = store
        .apply_action(Action {
            id: String::new(),
            kind: "Shipment".into(),
            key: "s3".into(),
            props: HashMap::from([("order_id".into(), "o1".into())]),
        })
        .unwrap_err();
    assert!(missing.contains("action id"), "{missing}");
    let mut empty_field = rec("Shipment", "s3", false, &[("order_id", "o1")]);
    empty_field.action_id = Some(String::new());
    let empty = store.append(empty_field).unwrap_err();
    assert!(empty.contains("empty action id"), "{empty}");

    let hidden_action = rec("Customer", "c0", true, &[("region", "eu")]);
    let mut hidden_action = hidden_action;
    hidden_action.action_id = Some("act-hide".into());
    store.append(hidden_action).unwrap();
    assert_eq!(
        store
            .load("Customer", "c0", &PropertyAcl::allow_all())
            .unwrap()
            .action_id
            .as_deref(),
        Some("act-hide")
    );
    assert!(!store.joins().is_visible("Customer", "c0"));
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert_eq!(
        reopened
            .load("Shipment", "s2", &PropertyAcl::allow_all())
            .unwrap()
            .action_id
            .as_deref(),
        Some("act-s2-7")
    );
    assert_eq!(
        reopened
            .load("Customer", "c0", &PropertyAcl::allow_all())
            .unwrap()
            .action_id
            .as_deref(),
        Some("act-hide")
    );
    let oss = ObjectSet::new(LocalCompute);
    let hops = oss.evaluate(&reopened, &fixture_request()).unwrap();
    assert_eq!(hops.two_hop_count, 1);
    assert_eq!(hops.sum_amount, 15);
    drop(reopened);

    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    assert_eq!(
        replayed
            .load("Shipment", "s2", &PropertyAcl::allow_all())
            .unwrap()
            .action_id
            .as_deref(),
        Some("act-s2-7")
    );
    assert_eq!(
        replayed
            .load("Customer", "c1", &PropertyAcl::allow_all())
            .unwrap()
            .action_id,
        None
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn product_loop_schemas() -> (SchemaDescriptor, SchemaDescriptor) {
    (
        SchemaDescriptor {
            kind: "component".into(),
            properties: vec!["name".into(), "tier".into()],
            required: vec!["name".into(), "tier".into()],
            links: vec![SchemaLink {
                name: "affects".into(),
                far_kind: "incident".into(),
                outgoing: false,
            }],
        },
        SchemaDescriptor {
            kind: "incident".into(),
            properties: vec!["affects".into(), "name".into()],
            required: vec!["name".into()],
            links: vec![SchemaLink {
                name: "affects".into(),
                far_kind: "component".into(),
                outgoing: true,
            }],
        },
    )
}

#[test]
fn committed_schema_validates_writes_and_rebuilds() {
    let (dir, log) = temp_log("schema-validate");
    let mut store = Store::create(&log).unwrap();
    store
        .append(rec(
            "component",
            "legacy",
            false,
            &[("alias", "pre-schema")],
        ))
        .unwrap();
    let (component_schema, incident_schema) = product_loop_schemas();
    store.append(component_schema.to_record().unwrap()).unwrap();
    store.append(incident_schema.to_record().unwrap()).unwrap();
    store
        .append(rec(
            "component",
            "svc-api",
            false,
            &[("name", "billing-api"), ("tier", "prod")],
        ))
        .unwrap();
    store
        .append(rec(
            "incident",
            "inc-1",
            false,
            &[("name", "elevated latency"), ("affects", "svc-api")],
        ))
        .unwrap();

    let missing = store
        .append(rec("component", "svc-web", false, &[("name", "web")]))
        .unwrap_err();
    assert!(missing.contains("missing required"), "{missing}");
    let extra = store
        .append(rec(
            "incident",
            "inc-2",
            false,
            &[("name", "n"), ("affects", "svc-api"), ("note", "acked")],
        ))
        .unwrap_err();
    assert!(extra.contains("unknown property"), "{extra}");
    assert!(store
        .load("component", "svc-web", &PropertyAcl::allow_all())
        .is_err());

    let historical = store
        .load("component", "legacy", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        historical.props.get("alias").map(String::as_str),
        Some("pre-schema")
    );
    let checked = store
        .load_with_schema(
            "component",
            "svc-api",
            &PropertyAcl::allow_all(),
            &component_schema,
        )
        .unwrap();
    assert_eq!(checked.props.get("tier").map(String::as_str), Some("prod"));
    let historical_check = store
        .load_with_schema(
            "component",
            "legacy",
            &PropertyAcl::allow_all(),
            &component_schema,
        )
        .unwrap_err();
    assert!(
        historical_check.contains("unknown property"),
        "{historical_check}"
    );

    store.append(rec("incident", "inc-1", true, &[])).unwrap();
    let hidden = store
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert!(hidden.hidden);
    assert!(!store.joins().is_visible("incident", "inc-1"));

    store
        .append(rec(
            "incident",
            "inc-3",
            false,
            &[("name", "disk full"), ("affects", "svc-api")],
        ))
        .unwrap();
    let incoming = EvaluateRequest {
        root_kind: "incident".into(),
        hops: vec![Hop {
            far_kind: "component".into(),
            join_property: "affects".into(),
            incoming: true,
        }],
        sum_kind: "component".into(),
        sum_property: "tier".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        object_bound: 0,
    };
    let hopped = ObjectSet::new(LocalCompute)
        .evaluate(&store, &incoming)
        .unwrap();
    assert_eq!(hopped.two_hop_count, 1);
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert_eq!(
        reopened.schema("component").unwrap().unwrap().kind,
        "component"
    );
    assert_eq!(
        reopened
            .load("component", "svc-api", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("name")
            .map(String::as_str),
        Some("billing-api")
    );
    assert!(
        reopened
            .load("incident", "inc-1", &PropertyAcl::allow_all())
            .unwrap()
            .hidden
    );
    drop(reopened);

    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    assert_eq!(
        replayed.schema("incident").unwrap().unwrap(),
        incident_schema
    );
    let rebuilt = replayed
        .load("component", "legacy", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        rebuilt.props.get("alias").map(String::as_str),
        Some("pre-schema")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn product_loop_seed() -> Vec<ObjectRecord> {
    vec![
        rec(
            "component",
            "svc-api",
            false,
            &[("name", "billing-api"), ("tier", "prod")],
        ),
        rec(
            "incident",
            "inc-1",
            false,
            &[("name", "elevated latency"), ("affects", "svc-api")],
        ),
    ]
}

fn list_prod_components(bound: usize) -> EvaluateRequest {
    EvaluateRequest {
        root_kind: "component".into(),
        hops: vec![],
        sum_kind: "component".into(),
        sum_property: "tier".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: Some(ExactMatch {
            property: "tier".into(),
            value: "prod".into(),
        }),
        object_bound: bound,
    }
}

fn hop_incident_to_component(bound: usize) -> EvaluateRequest {
    EvaluateRequest {
        root_kind: "incident".into(),
        hops: vec![Hop {
            far_kind: "component".into(),
            join_property: "affects".into(),
            incoming: true,
        }],
        sum_kind: "component".into(),
        sum_property: "tier".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        object_bound: bound,
    }
}

#[test]
fn evaluate_returns_bounded_product_loop_objects() {
    let (dir, log) = temp_log("evaluate-objects");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, product_loop_seed());
    let oss = ObjectSet::new(LocalCompute);

    let listed = oss.evaluate(&store, &list_prod_components(8)).unwrap();
    assert_eq!(listed.two_hop_count, 1);
    assert_eq!(listed.objects.len(), 1);
    assert_eq!(listed.objects[0].kind, "component");
    assert_eq!(listed.objects[0].key, "svc-api");
    assert_eq!(
        listed.objects[0].props.get("tier").map(String::as_str),
        Some("prod")
    );

    let hopped = oss.evaluate(&store, &hop_incident_to_component(8)).unwrap();
    assert_eq!(hopped.objects.len(), 1);
    assert_eq!(hopped.objects[0].key, "svc-api");

    let count_only = oss.evaluate(&store, &list_prod_components(0)).unwrap();
    assert!(count_only.objects.is_empty());

    store
        .append(rec(
            "component",
            "svc-web",
            false,
            &[("name", "web"), ("tier", "prod")],
        ))
        .unwrap();
    let overflow = oss.evaluate(&store, &list_prod_components(1)).unwrap_err();
    assert!(
        matches!(overflow, ComputeError::ObjectBound { bound: 1, count: 2 }),
        "{overflow:?}"
    );

    let denied = EvaluateRequest {
        root_kind: "component".into(),
        hops: vec![],
        sum_kind: "component".into(),
        sum_property: "tier".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::deny_property("component", "name"),
        filter: Some(ExactMatch {
            property: "tier".into(),
            value: "prod".into(),
        }),
        object_bound: 8,
    };
    let redacted = oss.evaluate(&store, &denied).unwrap();
    assert_eq!(redacted.objects.len(), 2);
    assert!(!redacted.objects[0].props.contains_key("name"));
    assert_eq!(
        redacted.objects[0].props.get("tier").map(String::as_str),
        Some("prod")
    );
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert_eq!(
        oss.evaluate(&reopened, &hop_incident_to_component(8))
            .unwrap()
            .objects[0]
            .key,
        "svc-api"
    );
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    assert_eq!(
        oss.evaluate(&replayed, &list_prod_components(8))
            .unwrap()
            .objects
            .len(),
        2
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn overlay_survives_refresh_and_stale_gen_fails() {
    let (dir, log) = temp_log("overlay-refresh");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, product_loop_seed());
    store
        .apply_overlay(
            OverlayPatch {
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([("note".into(), "acked".into())]),
                cleared: Vec::new(),
                action_id: None,
            },
            "act-inc-1-note".into(),
            Some(1),
        )
        .unwrap();
    let edited = store
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(edited.props.get("note").map(String::as_str), Some("acked"));
    assert_eq!(edited.action_id.as_deref(), Some("act-inc-1-note"));
    let stale = store
        .apply_overlay(
            OverlayPatch {
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([("note".into(), "again".into())]),
                cleared: Vec::new(),
                action_id: None,
            },
            "act-inc-1-note".into(),
            Some(1),
        )
        .unwrap_err();
    assert!(stale.contains("stale generation"), "{stale}");
    store
        .append(rec(
            "incident",
            "inc-1",
            false,
            &[("name", "elevated latency"), ("affects", "svc-api")],
        ))
        .unwrap();
    let refreshed = store
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        refreshed.props.get("note").map(String::as_str),
        Some("acked")
    );
    assert_eq!(
        refreshed.props.get("name").map(String::as_str),
        Some("elevated latency")
    );
    assert_eq!(refreshed.action_id.as_deref(), Some("act-inc-1-note"));
    store.append(rec("incident", "inc-1", true, &[])).unwrap();
    store
        .append(rec(
            "incident",
            "inc-1",
            false,
            &[("name", "elevated latency"), ("affects", "svc-api")],
        ))
        .unwrap();
    let recreated = store
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert!(!recreated.hidden);
    assert!(!recreated.props.contains_key("note"));
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert!(reopened.overlay("incident", "inc-1").unwrap().is_none());
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    assert!(!replayed
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap()
        .props
        .contains_key("note"));
    let _ = std::fs::remove_dir_all(&dir);
}
