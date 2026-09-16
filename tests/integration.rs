//! Public-API integration suite for `mikura` and `mikura-ingest`.
//!
//! Run with `cargo test --test integration --locked`. Also included in
//! `cargo test --workspace --locked`. These tests do not spawn `mikura-host`.

use mikura::{
    AclError, Action, Aggregate, ComputeError, EvaluateRequest, Hop, LocalCompute, ObjectRecord,
    ObjectSet, PropertyAcl, Store,
};
use mikura_ingest::{BatchIngest, StreamIngest};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

struct TempLog {
    dir: PathBuf,
    log: PathBuf,
}

impl TempLog {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "mikura-integration-{name}-{}-{}",
            std::process::id(),
            name
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp log directory");
        let log = dir.join("objects.mikura");
        Self { dir, log }
    }

    fn path(&self) -> &Path {
        &self.log
    }
}

impl Drop for TempLog {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn rec(kind: &str, key: &str, hidden: bool, props: &[(&str, &str)]) -> ObjectRecord {
    ObjectRecord {
        gen: 1,
        kind: kind.into(),
        key: key.into(),
        hidden,
        props: props
            .iter()
            .map(|(name, value)| ((*name).into(), (*value).into()))
            .collect(),
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
        rec("Asset", "a1", false, &[("owner_id", "c1"), ("mass", "4")]),
        rec("Asset", "a0", true, &[("owner_id", "c0"), ("mass", "99")]),
    ]
}

fn shipment_request() -> EvaluateRequest {
    EvaluateRequest {
        root_kind: "Customer".into(),
        hops: vec![
            Hop {
                far_kind: "Order".into(),
                join_property: "customer_id".into(),
            },
            Hop {
                far_kind: "Shipment".into(),
                join_property: "order_id".into(),
            },
        ],
        sum_kind: "Shipment".into(),
        sum_property: "amount".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
    }
}

fn asset_request() -> EvaluateRequest {
    EvaluateRequest {
        root_kind: "Customer".into(),
        hops: vec![Hop {
            far_kind: "Asset".into(),
            join_property: "owner_id".into(),
        }],
        sum_kind: "Asset".into(),
        sum_property: "mass".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
    }
}

#[test]
fn batch_ingest_evaluates_demo_and_non_demo_kinds() {
    let tmp = TempLog::new("batch-eval");
    let mut store = Store::create(tmp.path()).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();

    let oss = ObjectSet::new(LocalCompute);
    let shipments = oss.evaluate(&store, &shipment_request()).unwrap();
    assert_eq!(shipments.two_hop_count, 1);
    assert_eq!(shipments.sum_amount, 10);

    let assets = oss.evaluate(&store, &asset_request()).unwrap();
    assert_eq!(assets.two_hop_count, 1);
    assert_eq!(assets.sum_amount, 4);
}

#[test]
fn action_writeback_is_a_new_generation() {
    let tmp = TempLog::new("action-gen");
    let mut store = Store::create(tmp.path()).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let before = oss.evaluate(&store, &shipment_request()).unwrap();
    assert_eq!(before.sum_amount, 10);
    assert!(!store.joins().is_visible("Shipment", "s2"));

    store
        .apply_action(Action {
            kind: "Shipment".into(),
            key: "s2".into(),
            props: HashMap::from([
                ("order_id".into(), "o1".into()),
                ("amount".into(), "5".into()),
            ]),
        })
        .unwrap();

    assert!(store.joins().is_visible("Shipment", "s2"));
    assert_eq!(store.joins().prop("Shipment", "s2", "amount"), Some("5"));
    let after = oss.evaluate(&store, &shipment_request()).unwrap();
    assert_eq!(after.two_hop_count, 1);
    assert_eq!(after.sum_amount, 15);
}

#[test]
fn acl_deny_of_sum_property_fails_closed() {
    let tmp = TempLog::new("acl-deny");
    let mut store = Store::create(tmp.path()).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();

    let mut denied = shipment_request();
    denied.acl = PropertyAcl::deny_property("Shipment", "amount");
    let err = ObjectSet::new(LocalCompute)
        .evaluate(&store, &denied)
        .unwrap_err();
    assert!(matches!(
        err,
        ComputeError::Acl(AclError::Denied {
            ref kind,
            ref property
        }) if kind == "Shipment" && property == "amount"
    ));
}

#[test]
fn stream_overflow_fails_closed_without_silent_drop() {
    let tmp = TempLog::new("stream-overflow");
    let mut store = Store::create(tmp.path()).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let mut stream = StreamIngest::new(2).unwrap();
    stream
        .push(
            &mut store,
            rec(
                "Shipment",
                "s3",
                false,
                &[("order_id", "o1"), ("amount", "1")],
            ),
        )
        .unwrap();
    stream
        .push(
            &mut store,
            rec(
                "Shipment",
                "s4",
                false,
                &[("order_id", "o1"), ("amount", "2")],
            ),
        )
        .unwrap();
    let overflow = stream
        .push(
            &mut store,
            rec(
                "Shipment",
                "s5",
                false,
                &[("order_id", "o1"), ("amount", "99")],
            ),
        )
        .unwrap_err();
    assert!(
        overflow.contains("bound") && overflow.contains("exceeded"),
        "expected fail-closed overflow, got {overflow}"
    );
    assert!(!store.joins().is_visible("Shipment", "s5"));

    let oss = ObjectSet::new(LocalCompute);
    let live = oss.evaluate(&store, &shipment_request()).unwrap();
    assert_eq!(live.sum_amount, 13);
    stream.flush(&mut store).unwrap();
    let committed = oss.evaluate(&store, &shipment_request()).unwrap();
    assert_eq!(committed.sum_amount, 13);
}

#[test]
fn reopen_matches_live_hop_count_and_sum() {
    let tmp = TempLog::new("reopen");
    let mut store = Store::create(tmp.path()).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    store
        .apply_action(Action {
            kind: "Shipment".into(),
            key: "s2".into(),
            props: HashMap::from([
                ("order_id".into(), "o1".into()),
                ("amount".into(), "5".into()),
            ]),
        })
        .unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let live = oss.evaluate(&store, &shipment_request()).unwrap();
    let live_asset = oss.evaluate(&store, &asset_request()).unwrap();
    drop(store);

    let reopened = Store::open(tmp.path()).unwrap();
    let from_log = oss.evaluate(&reopened, &shipment_request()).unwrap();
    let from_log_asset = oss.evaluate(&reopened, &asset_request()).unwrap();
    assert_eq!(from_log.two_hop_count, live.two_hop_count);
    assert_eq!(from_log.sum_amount, live.sum_amount);
    assert_eq!(from_log_asset.two_hop_count, live_asset.two_hop_count);
    assert_eq!(from_log_asset.sum_amount, live_asset.sum_amount);
    assert_eq!(from_log.sum_amount, 15);
    assert_eq!(from_log_asset.sum_amount, 4);
}

#[test]
fn dual_read_after_deleting_sidecar_rebuilds_from_log() {
    let tmp = TempLog::new("dual-read");
    let sidecar = Store::join_map_path(tmp.path());
    let mut store = Store::create(tmp.path()).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let from_projection = oss.evaluate(&store, &shipment_request()).unwrap();
    let from_projection_asset = oss.evaluate(&store, &asset_request()).unwrap();
    drop(store);

    std::fs::remove_file(&sidecar).unwrap();
    let replayed = Store::open(tmp.path()).unwrap();
    let from_log = oss.evaluate(&replayed, &shipment_request()).unwrap();
    let from_log_asset = oss.evaluate(&replayed, &asset_request()).unwrap();
    assert_eq!(from_log.two_hop_count, from_projection.two_hop_count);
    assert_eq!(from_log.sum_amount, from_projection.sum_amount);
    assert_eq!(
        from_log_asset.two_hop_count,
        from_projection_asset.two_hop_count
    );
    assert_eq!(from_log_asset.sum_amount, from_projection_asset.sum_amount);
}

#[test]
fn sidecar_checksum_and_bad_magic_fail_closed() {
    let tmp = TempLog::new("sidecar-corrupt");
    let sidecar = Store::join_map_path(tmp.path());
    let mut store = Store::create(tmp.path()).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let live = oss.evaluate(&store, &shipment_request()).unwrap();
    drop(store);

    let good = std::fs::read(&sidecar).unwrap();
    let mut flipped = good.clone();
    let last = flipped.len() - 5;
    flipped[last] ^= 0x01;
    std::fs::write(&sidecar, &flipped).unwrap();
    let checksum_err = match Store::open(tmp.path()) {
        Err(err) => err,
        Ok(_) => panic!("checksum mismatch should fail closed"),
    };
    assert!(
        checksum_err.contains("checksum"),
        "checksum mismatch should fail closed: {checksum_err}"
    );

    let mut bad_magic = good;
    bad_magic[..8].copy_from_slice(b"MKJOIN99");
    std::fs::write(&sidecar, &bad_magic).unwrap();
    let magic_err = match Store::open(tmp.path()) {
        Err(err) => err,
        Ok(_) => panic!("bad magic should fail closed"),
    };
    assert!(
        magic_err.contains("magic") || magic_err.contains("checksum"),
        "bad magic should fail closed: {magic_err}"
    );

    std::fs::remove_file(&sidecar).unwrap();
    let recovered = Store::open(tmp.path()).unwrap();
    let from_log = oss.evaluate(&recovered, &shipment_request()).unwrap();
    assert_eq!(from_log.two_hop_count, live.two_hop_count);
    assert_eq!(from_log.sum_amount, live.sum_amount);
}
