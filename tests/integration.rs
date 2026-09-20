//! Public-API integration suite for `mikura` and `mikura-ingest`.
//!
//! Run with `cargo test --test integration --locked`. Also included in
//! `cargo test --workspace --locked`. These tests do not spawn `mikura-host`.

use mikura::{
    AclError, Action, Aggregate, ComputeError, EvaluateRequest, ExactMatch, Hop, LocalCompute,
    ObjectRecord, ObjectSet, OverlayPatch, Predicate, PropertyAcl, PropertyType, SchemaDescriptor,
    SchemaLink, Sort, Store, SCHEMA_SUMS, SCHEMA_TYPES,
};
use mikura_ingest::{snapshot_changelog, BatchIngest, StreamIngest};
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
        action_id: None,
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

fn shipment_sum_schema() -> SchemaDescriptor {
    SchemaDescriptor {
        kind: "Shipment".into(),
        properties: vec!["amount".into(), "order_id".into()],
        required: Vec::new(),
        links: vec![SchemaLink {
            name: "order_id".into(),
            far_kind: "Order".into(),
            outgoing: true,
        }],
        sums: vec!["amount".into()],
        types: Vec::new(),
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
                predicate: None,
            },
            Hop {
                far_kind: "Shipment".into(),
                join_property: "order_id".into(),
                incoming: false,
                predicate: None,
            },
        ],
        sum_kind: "Shipment".into(),
        sum_property: "amount".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: None,
        object_bound: 0,
        sort: None,
        page_size: 0,
        cursor: None,
    }
}

fn asset_request() -> EvaluateRequest {
    EvaluateRequest {
        root_kind: "Customer".into(),
        hops: vec![Hop {
            far_kind: "Asset".into(),
            join_property: "owner_id".into(),
            incoming: false,
            predicate: None,
        }],
        sum_kind: "Asset".into(),
        sum_property: "mass".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: None,
        object_bound: 0,
        sort: None,
        page_size: 0,
        cursor: None,
    }
}

#[test]
fn schema_named_measure_evaluates_and_rebuilds() {
    let tmp = TempLog::new("measure-eval");
    let mut store = Store::create(tmp.path()).unwrap();
    let schema = shipment_sum_schema();
    BatchIngest::run(
        &mut store,
        std::iter::once(schema.to_record().unwrap())
            .chain(fixture())
            .collect(),
    )
    .unwrap();
    assert_eq!(
        store
            .schema("Shipment")
            .unwrap()
            .unwrap()
            .sums
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["amount"]
    );
    assert_eq!(
        schema
            .to_record()
            .unwrap()
            .props
            .get(SCHEMA_SUMS)
            .map(String::as_str),
        Some("amount")
    );
    let oss = ObjectSet::new(LocalCompute);
    let live = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(live.two_hop_count, 1);
    assert_eq!(live.sum_amount, 10);

    let mut denied = fixture_request();
    denied.acl = PropertyAcl::deny_property("Shipment", "amount");
    assert!(matches!(
        oss.evaluate(&store, &denied),
        Err(ComputeError::Acl(AclError::Denied {
            ref kind,
            ref property
        })) if kind == "Shipment" && property == "amount"
    ));
    drop(store);

    let reopened = Store::open(tmp.path()).unwrap();
    let from_sidecar = oss.evaluate(&reopened, &fixture_request()).unwrap();
    assert_eq!(from_sidecar.two_hop_count, live.two_hop_count);
    assert_eq!(from_sidecar.sum_amount, live.sum_amount);
    drop(reopened);

    std::fs::remove_file(Store::join_map_path(tmp.path())).unwrap();
    let replayed = Store::open(tmp.path()).unwrap();
    let from_log = oss.evaluate(&replayed, &fixture_request()).unwrap();
    assert_eq!(from_log.two_hop_count, live.two_hop_count);
    assert_eq!(from_log.sum_amount, live.sum_amount);
}

#[test]
fn batch_ingest_evaluates_demo_and_non_demo_kinds() {
    let tmp = TempLog::new("batch-eval");
    let mut store = Store::create(tmp.path()).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();

    let oss = ObjectSet::new(LocalCompute);
    let shipments = oss.evaluate(&store, &fixture_request()).unwrap();
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
    let before = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(before.sum_amount, 10);
    assert!(!store.joins().is_visible("Shipment", "s2"));

    store
        .apply_action(
            Action {
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

    assert!(store.joins().is_visible("Shipment", "s2"));
    assert_eq!(store.joins().prop("Shipment", "s2", "amount"), Some("5"));
    let after = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(after.two_hop_count, 1);
    assert_eq!(after.sum_amount, 15);
}

#[test]
fn apply_action_replays_matching_id_and_fails_closed_on_conflict() {
    let tmp = TempLog::new("action-retry");
    let mut store = Store::create(tmp.path()).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let action = Action {
        id: "act-inc-1-note".into(),
        kind: "Shipment".into(),
        key: "s2".into(),
        props: HashMap::from([
            ("order_id".into(), "o1".into()),
            ("amount".into(), "5".into()),
        ]),
    };
    store.apply_action(action.clone(), None).unwrap();
    let committed = store
        .load("Shipment", "s2", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(committed.action_id.as_deref(), Some("act-inc-1-note"));
    store.apply_action(action.clone(), None).unwrap();
    assert_eq!(
        store
            .load("Shipment", "s2", &PropertyAcl::allow_all())
            .unwrap()
            .gen,
        committed.gen
    );

    let body = store
        .apply_action(
            Action {
                id: "act-inc-1-note".into(),
                kind: "Shipment".into(),
                key: "s2".into(),
                props: HashMap::from([
                    ("order_id".into(), "o1".into()),
                    ("amount".into(), "9".into()),
                ]),
            },
            None,
        )
        .unwrap_err();
    assert!(body.contains("body conflict"), "{body}");

    let other = store
        .apply_action(
            Action {
                id: "act-inc-1-note".into(),
                kind: "Shipment".into(),
                key: "s1".into(),
                props: HashMap::from([
                    ("order_id".into(), "o1".into()),
                    ("amount".into(), "5".into()),
                ]),
            },
            None,
        )
        .unwrap_err();
    assert!(other.contains("already committed"), "{other}");
    drop(store);

    let mut reopened = Store::open(tmp.path()).unwrap();
    reopened.apply_action(action, None).unwrap();
    assert_eq!(
        reopened
            .load("Shipment", "s2", &PropertyAcl::allow_all())
            .unwrap()
            .gen,
        committed.gen
    );
}

#[test]
fn changelog_treats_action_id_as_payload() {
    let mut previous = rec("Shipment", "s1", false, &[("amount", "10")]);
    previous.action_id = Some("act-a".into());
    let mut current = rec("Shipment", "s1", false, &[("amount", "10")]);
    current.action_id = Some("act-b".into());
    let records = snapshot_changelog(vec![previous], vec![current]);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].action_id.as_deref(), Some("act-b"));
}

#[test]
fn acl_deny_of_sum_property_fails_closed() {
    let tmp = TempLog::new("acl-deny");
    let mut store = Store::create(tmp.path()).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();

    let mut denied = fixture_request();
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
    let live = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(live.sum_amount, 13);
    stream.flush(&mut store).unwrap();
    let committed = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(committed.sum_amount, 13);
}

#[test]
fn reopen_matches_live_hop_count_and_sum() {
    let tmp = TempLog::new("reopen");
    let mut store = Store::create(tmp.path()).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    store
        .apply_action(
            Action {
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
    let oss = ObjectSet::new(LocalCompute);
    let live = oss.evaluate(&store, &fixture_request()).unwrap();
    let live_asset = oss.evaluate(&store, &asset_request()).unwrap();
    drop(store);

    let reopened = Store::open(tmp.path()).unwrap();
    let from_log = oss.evaluate(&reopened, &fixture_request()).unwrap();
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
    let from_projection = oss.evaluate(&store, &fixture_request()).unwrap();
    let from_projection_asset = oss.evaluate(&store, &asset_request()).unwrap();
    drop(store);

    std::fs::remove_file(&sidecar).unwrap();
    let replayed = Store::open(tmp.path()).unwrap();
    let from_log = oss.evaluate(&replayed, &fixture_request()).unwrap();
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
    let live = oss.evaluate(&store, &fixture_request()).unwrap();
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
    let from_log = oss.evaluate(&recovered, &fixture_request()).unwrap();
    assert_eq!(from_log.two_hop_count, live.two_hop_count);
    assert_eq!(from_log.sum_amount, live.sum_amount);
}

#[test]
fn load_current_object_after_restart() {
    let tmp = TempLog::new("load");
    let mut store = Store::create(tmp.path()).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let live = store
        .load("Customer", "c1", &PropertyAcl::allow_all())
        .unwrap();
    let hidden = store
        .load("Customer", "c0", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(live.props.get("region").map(String::as_str), Some("us"));
    assert!(hidden.hidden);
    assert_eq!(hidden.props.get("region").map(String::as_str), Some("eu"));
    assert!(!store.joins().is_visible("Customer", "c0"));
    drop(store);

    let reopened = Store::open(tmp.path()).unwrap();
    assert_eq!(
        reopened
            .load("Customer", "c1", &PropertyAcl::allow_all())
            .unwrap(),
        live
    );
    assert_eq!(
        reopened
            .load("Customer", "c0", &PropertyAcl::allow_all())
            .unwrap(),
        hidden
    );

    let mut store = Store::open(tmp.path()).unwrap();
    store
        .append(rec("Customer", "c1", false, &[("region", "ap")]))
        .unwrap();
    drop(store);
    let updated = Store::open(tmp.path()).unwrap();
    let latest = updated
        .load("Customer", "c1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(latest.gen, 2);
    assert_eq!(latest.props.get("region").map(String::as_str), Some("ap"));

    std::fs::remove_file(Store::join_map_path(tmp.path())).unwrap();
    let replayed = Store::open(tmp.path()).unwrap();
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
        hidden
    );
}

#[test]
fn incoming_hop_follows_join_property() {
    let tmp = TempLog::new("incoming");
    let mut store = Store::create(tmp.path()).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let outgoing = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(outgoing.two_hop_count, 1);
    let incoming = EvaluateRequest {
        root_kind: "Order".into(),
        hops: vec![Hop {
            far_kind: "Customer".into(),
            join_property: "customer_id".into(),
            incoming: true,
            predicate: None,
        }],
        sum_kind: "Customer".into(),
        sum_property: "region".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: None,
        object_bound: 0,
        sort: None,
        page_size: 0,
        cursor: None,
    };
    let response = oss.evaluate(&store, &incoming).unwrap();
    assert_eq!(response.two_hop_count, 1);
    assert!(!store.joins().is_visible("Order", "o0"));
    drop(store);
    let reopened = Store::open(tmp.path()).unwrap();
    assert_eq!(oss.evaluate(&reopened, &incoming).unwrap().two_hop_count, 1);
    std::fs::remove_file(Store::join_map_path(tmp.path())).unwrap();
    let replayed = Store::open(tmp.path()).unwrap();
    assert_eq!(oss.evaluate(&replayed, &incoming).unwrap().two_hop_count, 1);
}

#[test]
fn load_omits_denied_properties() {
    let tmp = TempLog::new("load-acl");
    let mut store = Store::create(tmp.path()).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let allow = PropertyAcl::allow_all();
    let live = store.load("Shipment", "s1", &allow).unwrap();
    assert_eq!(live.props.get("amount").map(String::as_str), Some("10"));
    assert_eq!(live.props.get("order_id").map(String::as_str), Some("o1"));

    let deny_amount = PropertyAcl::deny_property("Shipment", "amount");
    let redacted = store.load("Shipment", "s1", &deny_amount).unwrap();
    assert!(!redacted.props.contains_key("amount"));
    assert_eq!(
        redacted.props.get("order_id").map(String::as_str),
        Some("o1")
    );
    assert!(!redacted.props.contains_key("fabricated"));
    assert!(!store.joins().is_visible("Customer", "c0"));

    let mut denied = fixture_request();
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
    drop(store);

    let reopened = Store::open(tmp.path()).unwrap();
    assert!(!reopened
        .load("Shipment", "s1", &deny_amount)
        .unwrap()
        .props
        .contains_key("amount"));
    assert_eq!(
        reopened.load("Shipment", "s1", &allow).unwrap().props,
        live.props
    );
}

#[test]
fn exact_match_filter_on_evaluate() {
    let tmp = TempLog::new("filter");
    let mut store = Store::create(tmp.path()).unwrap();
    BatchIngest::run(
        &mut store,
        vec![
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
    )
    .unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let mut us = fixture_request();
    us.filter = Some(ExactMatch {
        property: "region".into(),
        value: "us".into(),
    });
    let response = oss.evaluate(&store, &us).unwrap();
    assert_eq!(response.two_hop_count, 1);
    assert_eq!(response.sum_amount, 10);
    drop(store);

    let reopened = Store::open(tmp.path()).unwrap();
    assert_eq!(oss.evaluate(&reopened, &us).unwrap(), response);
    std::fs::remove_file(Store::join_map_path(tmp.path())).unwrap();
    let replayed = Store::open(tmp.path()).unwrap();
    assert_eq!(oss.evaluate(&replayed, &us).unwrap(), response);
}

fn query_fixture_schemas() -> (SchemaDescriptor, SchemaDescriptor) {
    (
        SchemaDescriptor {
            kind: "component".into(),
            properties: vec!["name".into(), "tier".into()],
            required: vec!["name".into(), "tier".into()],
            links: Vec::new(),
            sums: Vec::new(),
            types: Vec::new(),
        },
        SchemaDescriptor {
            kind: "incident".into(),
            properties: vec![
                "affects".into(),
                "name".into(),
                "note".into(),
                "open".into(),
                "priority".into(),
            ],
            required: vec!["name".into()],
            links: vec![SchemaLink {
                name: "affects".into(),
                far_kind: "component".into(),
                outgoing: true,
            }],
            sums: Vec::new(),
            types: vec![
                ("open".into(), PropertyType::Boolean),
                ("priority".into(), PropertyType::Integer),
            ],
        },
    )
}

fn query_fixture_records() -> Vec<ObjectRecord> {
    vec![
        rec(
            "component",
            "svc-api",
            false,
            &[("name", "billing-api"), ("tier", "prod")],
        ),
        rec(
            "component",
            "svc-web",
            false,
            &[("name", "web"), ("tier", "prod")],
        ),
        rec(
            "component",
            "svc-batch",
            false,
            &[("name", "batch"), ("tier", "staging")],
        ),
        rec(
            "incident",
            "inc-1",
            false,
            &[
                ("name", "elevated latency"),
                ("affects", "svc-api"),
                ("open", "true"),
                ("priority", "2"),
            ],
        ),
        rec(
            "incident",
            "inc-2",
            false,
            &[
                ("name", "disk full"),
                ("affects", "svc-api"),
                ("open", "false"),
                ("priority", "3"),
                ("note", "pager"),
            ],
        ),
        rec(
            "incident",
            "inc-3",
            false,
            &[
                ("name", "job delay"),
                ("affects", "svc-batch"),
                ("open", "true"),
                ("priority", "1"),
            ],
        ),
    ]
}

fn listed_keys(response: &mikura::EvaluateResponse) -> Vec<&str> {
    let mut keys: Vec<&str> = response
        .objects
        .iter()
        .map(|row| row.key.as_str())
        .collect();
    keys.sort();
    keys
}

#[test]
fn composed_predicates_select_typed_object_sets() {
    let tmp = TempLog::new("predicates");
    let mut store = Store::create(tmp.path()).unwrap();
    let (component_schema, incident_schema) = query_fixture_schemas();
    let mut records = vec![
        component_schema.to_record().unwrap(),
        incident_schema.to_record().unwrap(),
    ];
    records.extend(query_fixture_records());
    BatchIngest::run(&mut store, records).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let open = EvaluateRequest {
        root_kind: "incident".into(),
        hops: vec![],
        sum_kind: "incident".into(),
        sum_property: "priority".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: Some(Predicate::eq("open", "true")),
        object_bound: 8,
        sort: None,
        page_size: 0,
        cursor: None,
    };
    let listed = oss.evaluate(&store, &open).unwrap();
    assert_eq!(listed_keys(&listed), ["inc-1", "inc-3"]);

    let filter_then_hop = EvaluateRequest {
        root_kind: "incident".into(),
        hops: vec![Hop {
            far_kind: "component".into(),
            join_property: "affects".into(),
            incoming: true,
            predicate: None,
        }],
        sum_kind: "component".into(),
        sum_property: "tier".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: Some(Predicate::eq("open", "true")),
        object_bound: 8,
        sort: None,
        page_size: 0,
        cursor: None,
    };
    assert_eq!(
        listed_keys(&oss.evaluate(&store, &filter_then_hop).unwrap()),
        ["svc-api", "svc-batch"]
    );

    let hop_then_filter = EvaluateRequest {
        root_kind: "incident".into(),
        hops: vec![Hop {
            far_kind: "component".into(),
            join_property: "affects".into(),
            incoming: true,
            predicate: Some(Predicate::eq("tier", "prod")),
        }],
        sum_kind: "component".into(),
        sum_property: "tier".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: None,
        object_bound: 8,
        sort: None,
        page_size: 0,
        cursor: None,
    };
    let hopped = oss.evaluate(&store, &hop_then_filter).unwrap();
    assert_eq!(listed_keys(&hopped), ["svc-api"]);
    assert_eq!(hopped.two_hop_count, 2);

    let range = EvaluateRequest {
        root_kind: "incident".into(),
        hops: vec![],
        sum_kind: "incident".into(),
        sum_property: "priority".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: Some(Predicate::range("priority", Some("2"), Some("3"))),
        object_bound: 8,
        sort: None,
        page_size: 0,
        cursor: None,
    };
    assert_eq!(
        listed_keys(&oss.evaluate(&store, &range).unwrap()),
        ["inc-1", "inc-2"]
    );

    let missing = EvaluateRequest {
        root_kind: "incident".into(),
        hops: vec![],
        sum_kind: "incident".into(),
        sum_property: "priority".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: Some(Predicate::missing("note")),
        object_bound: 8,
        sort: None,
        page_size: 0,
        cursor: None,
    };
    assert_eq!(
        listed_keys(&oss.evaluate(&store, &missing).unwrap()),
        ["inc-1", "inc-3"]
    );

    let denied = EvaluateRequest {
        acl: PropertyAcl::deny_property("incident", "open"),
        ..open.clone()
    };
    assert!(matches!(
        oss.evaluate(&store, &denied),
        Err(ComputeError::Acl(AclError::Denied { .. }))
    ));

    store
        .hide("incident", "inc-3", &PropertyAcl::allow_all())
        .unwrap();
    let after_hide = oss.evaluate(&store, &open).unwrap();
    assert_eq!(listed_keys(&after_hide), ["inc-1"]);
    drop(store);

    let reopened = Store::open(tmp.path()).unwrap();
    assert_eq!(
        listed_keys(&oss.evaluate(&reopened, &open).unwrap()),
        ["inc-1"]
    );
    std::fs::remove_file(Store::join_map_path(tmp.path())).unwrap();
    let replayed = Store::open(tmp.path()).unwrap();
    assert_eq!(
        listed_keys(&oss.evaluate(&replayed, &hop_then_filter).unwrap()),
        ["svc-api"]
    );
}

fn ordered_keys(response: &mikura::EvaluateResponse) -> Vec<&str> {
    response
        .objects
        .iter()
        .map(|row| row.key.as_str())
        .collect()
}

#[test]
fn ordered_pages_are_snapshot_bound() {
    let tmp = TempLog::new("pages");
    let mut store = Store::create(tmp.path()).unwrap();
    let (component_schema, incident_schema) = query_fixture_schemas();
    let mut records = vec![
        component_schema.to_record().unwrap(),
        incident_schema.to_record().unwrap(),
    ];
    records.extend(query_fixture_records());
    BatchIngest::run(&mut store, records).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let first = EvaluateRequest {
        root_kind: "incident".into(),
        hops: vec![],
        sum_kind: "incident".into(),
        sum_property: "priority".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: None,
        object_bound: 8,
        sort: Some(Sort::by("priority")),
        page_size: 1,
        cursor: None,
    };
    let page1 = oss.evaluate(&store, &first).unwrap();
    assert_eq!(ordered_keys(&page1), ["inc-3"]);
    assert_eq!(page1.two_hop_count, 3);
    let cursor = page1.cursor.clone().expect("continuation");

    let page2 = oss
        .evaluate(
            &store,
            &EvaluateRequest {
                cursor: Some(cursor.clone()),
                ..first.clone()
            },
        )
        .unwrap();
    assert_eq!(ordered_keys(&page2), ["inc-1"]);

    let ties = oss
        .evaluate(
            &store,
            &EvaluateRequest {
                sort: Some(Sort::by("open")),
                page_size: 0,
                cursor: None,
                ..first.clone()
            },
        )
        .unwrap();
    assert_eq!(ordered_keys(&ties), ["inc-2", "inc-1", "inc-3"]);

    store
        .hide("incident", "inc-2", &PropertyAcl::allow_all())
        .unwrap();
    let after_write = oss.evaluate(
        &store,
        &EvaluateRequest {
            cursor: Some(cursor),
            ..first
        },
    );
    assert!(
        matches!(after_write, Err(ComputeError::Page(_))),
        "{after_write:?}"
    );
}

#[test]
fn schema_validates_product_loop_writes_and_rebuilds() {
    let tmp = TempLog::new("schema");
    let mut store = Store::create(tmp.path()).unwrap();
    BatchIngest::run(
        &mut store,
        vec![rec(
            "component",
            "legacy",
            false,
            &[("alias", "pre-schema")],
        )],
    )
    .unwrap();
    let component_schema = SchemaDescriptor {
        kind: "component".into(),
        properties: vec!["name".into(), "tier".into()],
        required: vec!["name".into(), "tier".into()],
        links: vec![SchemaLink {
            name: "affects".into(),
            far_kind: "incident".into(),
            outgoing: false,
        }],
        sums: Vec::new(),
        types: Vec::new(),
    };
    let incident_schema = SchemaDescriptor {
        kind: "incident".into(),
        properties: vec!["affects".into(), "name".into()],
        required: vec!["name".into()],
        links: vec![SchemaLink {
            name: "affects".into(),
            far_kind: "component".into(),
            outgoing: true,
        }],
        sums: Vec::new(),
        types: Vec::new(),
    };
    BatchIngest::run(
        &mut store,
        vec![
            component_schema.to_record().unwrap(),
            incident_schema.to_record().unwrap(),
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
        ],
    )
    .unwrap();

    let invalid = BatchIngest::run(
        &mut store,
        vec![rec("component", "svc-web", false, &[("name", "web")])],
    )
    .unwrap_err();
    assert!(invalid.contains("missing required"), "{invalid}");
    assert_eq!(
        store
            .load("component", "legacy", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("alias")
            .map(String::as_str),
        Some("pre-schema")
    );
    assert_eq!(
        store
            .load_with_schema(
                "incident",
                "inc-1",
                &PropertyAcl::allow_all(),
                &incident_schema,
            )
            .unwrap()
            .props
            .get("affects")
            .map(String::as_str),
        Some("svc-api")
    );
    drop(store);

    let reopened = Store::open(tmp.path()).unwrap();
    assert_eq!(
        reopened.schema("component").unwrap().unwrap(),
        component_schema
    );
    std::fs::remove_file(Store::join_map_path(tmp.path())).unwrap();
    let replayed = Store::open(tmp.path()).unwrap();
    assert_eq!(
        replayed
            .load("component", "svc-api", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("tier")
            .map(String::as_str),
        Some("prod")
    );
    assert_eq!(
        replayed.schema("incident").unwrap().unwrap(),
        incident_schema
    );
}

#[test]
fn evaluate_lists_product_loop_objects_and_enforces_bound() {
    let tmp = TempLog::new("evaluate-objects");
    let mut store = Store::create(tmp.path()).unwrap();
    BatchIngest::run(
        &mut store,
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
        ],
    )
    .unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let listed = oss
        .evaluate(
            &store,
            &EvaluateRequest {
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
                predicate: None,
                object_bound: 8,
                sort: None,
                page_size: 0,
                cursor: None,
            },
        )
        .unwrap();
    assert_eq!(listed.objects.len(), 1);
    assert_eq!(listed.objects[0].key, "svc-api");

    let hopped = oss
        .evaluate(
            &store,
            &EvaluateRequest {
                root_kind: "incident".into(),
                hops: vec![Hop {
                    far_kind: "component".into(),
                    join_property: "affects".into(),
                    incoming: true,
                    predicate: None,
                }],
                sum_kind: "component".into(),
                sum_property: "tier".into(),
                aggregate: Aggregate::CountAndSum,
                acl: PropertyAcl::allow_all(),
                filter: None,
                predicate: None,
                object_bound: 8,
                sort: None,
                page_size: 0,
                cursor: None,
            },
        )
        .unwrap();
    assert_eq!(hopped.objects[0].key, "svc-api");

    store
        .append(rec(
            "component",
            "svc-web",
            false,
            &[("name", "web"), ("tier", "prod")],
        ))
        .unwrap();
    let overflow = oss
        .evaluate(
            &store,
            &EvaluateRequest {
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
                predicate: None,
                object_bound: 1,
                sort: None,
                page_size: 0,
                cursor: None,
            },
        )
        .unwrap_err();
    assert!(matches!(
        overflow,
        ComputeError::ObjectBound { bound: 1, count: 2 }
    ));
    drop(store);

    let reopened = Store::open(tmp.path()).unwrap();
    assert_eq!(
        oss.evaluate(
            &reopened,
            &EvaluateRequest {
                root_kind: "incident".into(),
                hops: vec![Hop {
                    far_kind: "component".into(),
                    join_property: "affects".into(),
                    incoming: true,
                    predicate: None,
                }],
                sum_kind: "component".into(),
                sum_property: "tier".into(),
                aggregate: Aggregate::CountAndSum,
                acl: PropertyAcl::allow_all(),
                filter: None,
                predicate: None,
                object_bound: 8,
                sort: None,
                page_size: 0,
                cursor: None,
            },
        )
        .unwrap()
        .objects[0]
            .key,
        "svc-api"
    );
    std::fs::remove_file(Store::join_map_path(tmp.path())).unwrap();
    let replayed = Store::open(tmp.path()).unwrap();
    assert_eq!(
        oss.evaluate(
            &replayed,
            &EvaluateRequest {
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
                predicate: None,
                object_bound: 8,
                sort: None,
                page_size: 0,
                cursor: None,
            },
        )
        .unwrap()
        .objects
        .len(),
        2
    );
}

#[test]
fn overlay_refresh_keeps_admitted_note() {
    let tmp = TempLog::new("overlay");
    let mut store = Store::create(tmp.path()).unwrap();
    BatchIngest::run(
        &mut store,
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
        ],
    )
    .unwrap();
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
    BatchIngest::run(
        &mut store,
        vec![rec(
            "incident",
            "inc-1",
            false,
            &[("name", "elevated latency"), ("affects", "svc-api")],
        )],
    )
    .unwrap();
    let live = store
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(live.props.get("note").map(String::as_str), Some("acked"));
    drop(store);
    std::fs::remove_file(Store::join_map_path(tmp.path())).unwrap();
    let replayed = Store::open(tmp.path()).unwrap();
    assert_eq!(
        replayed
            .load("incident", "inc-1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("note")
            .map(String::as_str),
        Some("acked")
    );
    assert_eq!(
        replayed
            .overlay("incident", "inc-1")
            .unwrap()
            .unwrap()
            .props["note"],
        "acked"
    );
}

#[test]
fn apply_action_replaces_without_merging_overlay() {
    let tmp = TempLog::new("overlay-apply-action");
    let mut store = Store::create(tmp.path()).unwrap();
    BatchIngest::run(
        &mut store,
        vec![rec(
            "incident",
            "inc-1",
            false,
            &[("name", "elevated latency"), ("affects", "svc-api")],
        )],
    )
    .unwrap();
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
    store
        .apply_action(
            Action {
                id: "act-inc-1-rename".into(),
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([
                    ("name".into(), "elevated latency".into()),
                    ("affects".into(), "svc-api".into()),
                ]),
            },
            None,
        )
        .unwrap();
    let replaced = store
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert!(!replaced.props.contains_key("note"));
    assert_eq!(replaced.action_id.as_deref(), Some("act-inc-1-rename"));
}

#[test]
fn hide_drops_identity_from_join_maps_and_rebuilds() {
    let tmp = TempLog::new("hide-joins");
    let mut store = Store::create(tmp.path()).unwrap();
    BatchIngest::run(
        &mut store,
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
        ],
    )
    .unwrap();
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
    assert!(store.joins().is_visible("incident", "inc-1"));
    store
        .hide("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert!(!store.joins().is_visible("incident", "inc-1"));
    let hidden = store
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert!(hidden.hidden);
    assert_eq!(hidden.props.get("note").map(String::as_str), Some("acked"));
    let overlay = store
        .load(
            mikura::OVERLAY_KIND,
            &OverlayPatch::identity_key("incident", "inc-1"),
            &PropertyAcl::allow_all(),
        )
        .unwrap();
    assert!(overlay.hidden);
    assert!(store.overlay("incident", "inc-1").unwrap().is_none());
    let listed = ObjectSet::new(LocalCompute)
        .evaluate(
            &store,
            &EvaluateRequest {
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
                predicate: None,
                object_bound: 8,
                sort: None,
                page_size: 0,
                cursor: None,
            },
        )
        .unwrap();
    assert_eq!(listed.objects.len(), 1);
    assert_eq!(listed.objects[0].key, "svc-api");
    let hopped = ObjectSet::new(LocalCompute)
        .evaluate(
            &store,
            &EvaluateRequest {
                root_kind: "incident".into(),
                hops: vec![Hop {
                    far_kind: "component".into(),
                    join_property: "affects".into(),
                    incoming: true,
                    predicate: None,
                }],
                sum_kind: "component".into(),
                sum_property: "tier".into(),
                aggregate: Aggregate::CountAndSum,
                acl: PropertyAcl::allow_all(),
                filter: None,
                predicate: None,
                object_bound: 8,
                sort: None,
                page_size: 0,
                cursor: None,
            },
        )
        .unwrap();
    assert_eq!(hopped.two_hop_count, 0);
    assert!(hopped.objects.is_empty());
    let missing = store
        .hide("incident", "nope", &PropertyAcl::allow_all())
        .unwrap_err();
    assert!(missing.contains("unknown identity"), "{missing}");
    drop(store);

    let reopened = Store::open(tmp.path()).unwrap();
    assert!(!reopened.joins().is_visible("incident", "inc-1"));
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(tmp.path())).unwrap();
    let replayed = Store::open(tmp.path()).unwrap();
    assert!(!replayed.joins().is_visible("incident", "inc-1"));
    assert!(
        replayed
            .load("incident", "inc-1", &PropertyAcl::allow_all())
            .unwrap()
            .hidden
    );
}

fn incident_typed_schema() -> SchemaDescriptor {
    SchemaDescriptor {
        kind: "incident".into(),
        properties: vec![
            "affects".into(),
            "cost".into(),
            "name".into(),
            "open".into(),
            "opened_at".into(),
            "priority".into(),
        ],
        required: vec!["name".into()],
        links: vec![SchemaLink {
            name: "affects".into(),
            far_kind: "component".into(),
            outgoing: true,
        }],
        sums: Vec::new(),
        types: vec![
            ("cost".into(), PropertyType::Decimal { scale: 2 }),
            ("open".into(), PropertyType::Boolean),
            ("opened_at".into(), PropertyType::Timestamp),
            ("priority".into(), PropertyType::Integer),
        ],
    }
}

#[test]
fn typed_scalars_round_trip_ingest_overlay_and_rebuild() {
    let tmp = TempLog::new("typed-values");
    let mut store = Store::create(tmp.path()).unwrap();
    let schema = incident_typed_schema();
    BatchIngest::run(
        &mut store,
        vec![
            schema.to_record().unwrap(),
            rec(
                "incident",
                "inc-1",
                false,
                &[
                    ("name", "elevated latency"),
                    ("affects", "svc-api"),
                    ("open", "true"),
                    ("priority", "2"),
                    ("opened_at", "2026-09-19T17:00:00Z"),
                    ("cost", "1500.00"),
                ],
            ),
        ],
    )
    .unwrap();
    let loaded = store
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        loaded.props.get("opened_at").map(String::as_str),
        Some("2026-09-19T17:00:00.000Z")
    );
    let invalid = BatchIngest::run(
        &mut store,
        vec![rec(
            "incident",
            "inc-2",
            false,
            &[("name", "n"), ("cost", "1.5")],
        )],
    )
    .unwrap_err();
    assert!(invalid.to_string().contains("decimal"), "{invalid}");
    store
        .apply_overlay(
            OverlayPatch {
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([("open".into(), "false".into())]),
                cleared: Vec::new(),
                action_id: None,
            },
            "act-inc-1-open".into(),
            None,
        )
        .unwrap();
    let overlayed = store
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        overlayed.props.get("open").map(String::as_str),
        Some("false")
    );
    let descriptor = store.schema("incident").unwrap().unwrap();
    assert!(descriptor
        .to_record()
        .unwrap()
        .props
        .get(SCHEMA_TYPES)
        .unwrap()
        .contains("open:boolean"));
    drop(store);

    let reopened = Store::open(tmp.path()).unwrap();
    assert_eq!(
        reopened
            .load("incident", "inc-1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("open")
            .map(String::as_str),
        Some("false")
    );
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(tmp.path())).unwrap();
    let replayed = Store::open(tmp.path()).unwrap();
    let rebuilt = replayed
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        rebuilt.props.get("cost").map(String::as_str),
        Some("1500.00")
    );
    assert_eq!(
        replayed.schema("incident").unwrap().unwrap().types,
        schema.types
    );
}

fn evolving_incident_schema() -> SchemaDescriptor {
    SchemaDescriptor {
        kind: "incident".into(),
        properties: vec!["affects".into(), "name".into(), "note".into()],
        required: vec!["name".into()],
        links: vec![SchemaLink {
            name: "affects".into(),
            far_kind: "component".into(),
            outgoing: true,
        }],
        sums: Vec::new(),
        types: Vec::new(),
    }
}

#[test]
fn schema_evolution_preserves_meaning_and_rejects_recast() {
    let tmp = TempLog::new("schema-evolution");
    let mut store = Store::create(tmp.path()).unwrap();
    let initial = evolving_incident_schema();
    BatchIngest::run(
        &mut store,
        vec![
            initial.to_record().unwrap(),
            rec(
                "incident",
                "inc-1",
                false,
                &[
                    ("name", "elevated latency"),
                    ("affects", "svc-api"),
                    ("note", "acked"),
                ],
            ),
        ],
    )
    .unwrap();

    let mut recast = initial.clone();
    recast.types = vec![("name".into(), PropertyType::Integer)];
    let recast_err = store.append(recast.to_record().unwrap()).unwrap_err();
    assert!(recast_err.contains("cannot recast name"), "{recast_err}");
    let mut retarget = initial.clone();
    retarget.links[0].far_kind = "service".into();
    let link_err = store.append(retarget.to_record().unwrap()).unwrap_err();
    assert!(link_err.contains("cannot retarget"), "{link_err}");
    assert_eq!(store.schema("incident").unwrap().unwrap(), initial);

    let mut evolved = initial.clone();
    evolved
        .properties
        .extend(["open".into(), "priority".into()]);
    evolved.required.push("priority".into());
    evolved.types = vec![
        ("open".into(), PropertyType::Boolean),
        ("priority".into(), PropertyType::Integer),
    ];
    evolved.links.push(SchemaLink {
        name: "opened_by".into(),
        far_kind: "component".into(),
        outgoing: false,
    });
    evolved.sums = vec!["name".into()];
    store.append(evolved.to_record().unwrap()).unwrap();
    let historical = store
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        historical.props.get("note").map(String::as_str),
        Some("acked")
    );
    evolved.required.retain(|name| name != "priority");
    store.append(evolved.to_record().unwrap()).unwrap();

    store
        .apply_overlay(
            OverlayPatch {
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([("note".into(), "paged".into())]),
                cleared: Vec::new(),
                action_id: None,
            },
            "act-inc-1-note".into(),
            None,
        )
        .unwrap();
    store
        .apply_action(
            Action {
                id: "act-inc-1-body".into(),
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([
                    ("name".into(), "elevated latency".into()),
                    ("affects".into(), "svc-api".into()),
                    ("note".into(), "paged".into()),
                    ("open".into(), "true".into()),
                ]),
            },
            None,
        )
        .unwrap();

    let mut shrunk = evolved.clone();
    shrunk.properties.retain(|name| name != "note");
    store.append(shrunk.to_record().unwrap()).unwrap();
    assert_eq!(
        store
            .load("incident", "inc-1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("note")
            .map(String::as_str),
        Some("paged")
    );
    let overlay_err = store
        .apply_overlay(
            OverlayPatch {
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([("note".into(), "stale".into())]),
                cleared: Vec::new(),
                action_id: None,
            },
            "act-inc-1-note-2".into(),
            None,
        )
        .unwrap_err();
    assert!(overlay_err.contains("unknown property"), "{overlay_err}");
    let action_err = store
        .apply_action(
            Action {
                id: "act-inc-1-stale".into(),
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([
                    ("name".into(), "elevated latency".into()),
                    ("affects".into(), "svc-api".into()),
                    ("note".into(), "paged".into()),
                ]),
            },
            None,
        )
        .unwrap_err();
    assert!(action_err.contains("unknown property"), "{action_err}");
    let replay_err = store
        .apply_action(
            Action {
                id: "act-inc-1-body".into(),
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([
                    ("name".into(), "elevated latency".into()),
                    ("affects".into(), "svc-api".into()),
                    ("note".into(), "paged".into()),
                    ("open".into(), "true".into()),
                ]),
            },
            None,
        )
        .unwrap_err();
    assert!(replay_err.contains("unknown property"), "{replay_err}");

    let mut hidden = shrunk.to_record().unwrap();
    hidden.hidden = true;
    store.append(hidden).unwrap();
    store.append(shrunk.to_record().unwrap()).unwrap();
    drop(store);

    let reopened = Store::open(tmp.path()).unwrap();
    assert_eq!(reopened.schema("incident").unwrap().unwrap(), shrunk);
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(tmp.path())).unwrap();
    let replayed = Store::open(tmp.path()).unwrap();
    assert_eq!(replayed.schema("incident").unwrap().unwrap(), shrunk);
    assert_eq!(
        replayed
            .load("incident", "inc-1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("open")
            .map(String::as_str),
        Some("true")
    );
}

fn association_schemas() -> Vec<SchemaDescriptor> {
    vec![
        SchemaDescriptor {
            kind: "component".into(),
            properties: vec!["name".into(), "tier".into()],
            required: vec!["name".into(), "tier".into()],
            links: vec![SchemaLink {
                name: "affects".into(),
                far_kind: "incident".into(),
                outgoing: false,
            }],
            sums: Vec::new(),
            types: Vec::new(),
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
            sums: Vec::new(),
            types: Vec::new(),
        },
        SchemaDescriptor {
            kind: "label".into(),
            properties: vec!["name".into()],
            required: vec!["name".into()],
            links: Vec::new(),
            sums: Vec::new(),
            types: Vec::new(),
        },
        SchemaDescriptor {
            kind: "incident_label".into(),
            properties: vec!["incident".into(), "label".into()],
            required: vec!["incident".into(), "label".into()],
            links: vec![
                SchemaLink {
                    name: "incident".into(),
                    far_kind: "incident".into(),
                    outgoing: true,
                },
                SchemaLink {
                    name: "label".into(),
                    far_kind: "label".into(),
                    outgoing: true,
                },
            ],
            sums: Vec::new(),
            types: Vec::new(),
        },
    ]
}

fn association_seed() -> Vec<ObjectRecord> {
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
        rec("label", "sev-high", false, &[("name", "high")]),
        rec("label", "region-eu", false, &[("name", "eu")]),
        rec(
            "incident_label",
            "il-inc-1-sev-high",
            false,
            &[("incident", "inc-1"), ("label", "sev-high")],
        ),
        rec(
            "incident_label",
            "il-inc-1-region-eu",
            false,
            &[("incident", "inc-1"), ("label", "region-eu")],
        ),
    ]
}

fn hop_incident_to_labels(bound: usize) -> EvaluateRequest {
    EvaluateRequest {
        root_kind: "incident".into(),
        hops: vec![
            Hop {
                far_kind: "incident_label".into(),
                join_property: "incident".into(),
                incoming: false,
                predicate: None,
            },
            Hop {
                far_kind: "label".into(),
                join_property: "label".into(),
                incoming: true,
                predicate: None,
            },
        ],
        sum_kind: "label".into(),
        sum_property: "name".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: None,
        object_bound: bound,
        sort: None,
        page_size: 0,
        cursor: None,
    }
}

#[test]
fn association_objects_persist_and_traverse() {
    let tmp = TempLog::new("associates");
    let mut store = Store::create(tmp.path()).unwrap();
    let mut records: Vec<ObjectRecord> = association_schemas()
        .into_iter()
        .map(|schema| schema.to_record().unwrap())
        .collect();
    records.extend(association_seed());
    BatchIngest::run(&mut store, records).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let labels = oss.evaluate(&store, &hop_incident_to_labels(8)).unwrap();
    let mut keys: Vec<String> = labels.objects.iter().map(|row| row.key.clone()).collect();
    keys.sort();
    assert_eq!(keys, vec!["region-eu", "sev-high"]);

    store
        .hide(
            "incident_label",
            "il-inc-1-region-eu",
            &PropertyAcl::allow_all(),
        )
        .unwrap();
    let after_hide = oss.evaluate(&store, &hop_incident_to_labels(8)).unwrap();
    assert_eq!(
        after_hide
            .objects
            .iter()
            .map(|row| row.key.as_str())
            .collect::<Vec<_>>(),
        vec!["sev-high"]
    );

    let mut denied = hop_incident_to_labels(8);
    denied.acl = PropertyAcl::deny_property("incident_label", "label");
    assert!(matches!(
        oss.evaluate(&store, &denied),
        Err(ComputeError::Acl(AclError::Denied { .. }))
    ));

    let affects = oss
        .evaluate(
            &store,
            &EvaluateRequest {
                root_kind: "incident".into(),
                hops: vec![Hop {
                    far_kind: "component".into(),
                    join_property: "affects".into(),
                    incoming: true,
                    predicate: None,
                }],
                sum_kind: "component".into(),
                sum_property: "tier".into(),
                aggregate: Aggregate::CountAndSum,
                acl: PropertyAcl::allow_all(),
                filter: None,
                predicate: None,
                object_bound: 8,
                sort: None,
                page_size: 0,
                cursor: None,
            },
        )
        .unwrap();
    assert_eq!(affects.objects[0].key, "svc-api");
    drop(store);

    let reopened = Store::open(tmp.path()).unwrap();
    let live = oss.evaluate(&reopened, &hop_incident_to_labels(8)).unwrap();
    assert_eq!(live.objects[0].key, "sev-high");
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(tmp.path())).unwrap();
    let rebuilt = Store::open(tmp.path()).unwrap();
    let replayed = oss.evaluate(&rebuilt, &hop_incident_to_labels(8)).unwrap();
    assert_eq!(replayed.objects[0].key, "sev-high");
}

#[test]
fn restriction_hides_identities_across_load_evaluate_and_rebuild() {
    let tmp = TempLog::new("restriction-view");
    let mut store = Store::create(tmp.path()).unwrap();
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
    let mut hidden = PropertyAcl::allow_all();
    hidden.insert_hide_identity("incident", "inc-1").unwrap();
    assert!(store
        .load("incident", "inc-1", &hidden)
        .unwrap_err()
        .contains("unknown identity"));
    let oss = ObjectSet::new(LocalCompute);
    let mut hop = EvaluateRequest {
        root_kind: "incident".into(),
        hops: vec![Hop {
            far_kind: "component".into(),
            join_property: "affects".into(),
            incoming: true,
            predicate: None,
        }],
        sum_kind: "component".into(),
        sum_property: "tier".into(),
        aggregate: Aggregate::CountAndSum,
        acl: hidden.clone(),
        filter: None,
        predicate: None,
        object_bound: 8,
        sort: None,
        page_size: 0,
        cursor: None,
    };
    let closed = oss.evaluate(&store, &hop).unwrap();
    assert_eq!(closed.two_hop_count, 0);
    hop.acl = PropertyAcl::allow_all();
    let open = oss.evaluate(&store, &hop).unwrap();
    assert_eq!(open.two_hop_count, 1);
    let overlay_err = store
        .apply_overlay_in_view(
            OverlayPatch {
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([("note".into(), "x".into())]),
                cleared: Vec::new(),
                action_id: None,
            },
            "act-hidden".into(),
            None,
            &hidden,
        )
        .unwrap_err();
    assert!(overlay_err.contains("not in this view"), "{overlay_err}");
    drop(store);
    std::fs::remove_file(Store::join_map_path(tmp.path())).unwrap();
    let rebuilt = Store::open(tmp.path()).unwrap();
    assert!(rebuilt
        .load("incident", "inc-1", &hidden)
        .unwrap_err()
        .contains("unknown identity"));
    assert_eq!(
        rebuilt
            .load("incident", "inc-1", &PropertyAcl::allow_all())
            .unwrap()
            .key,
        "inc-1"
    );
}
