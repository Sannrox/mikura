use super::super::*;
use super::helpers::*;
use std::collections::HashMap;

fn twin(namespace: &str, amount: &str) -> Vec<ObjectRecord> {
    vec![
        rec_in(namespace, "Customer", "c1", false, &[("region", namespace)]),
        rec_in(
            namespace,
            "Order",
            "o1",
            false,
            &[("customer_id", "c1"), ("tag", namespace)],
        ),
        rec_in(
            namespace,
            "Shipment",
            "s1",
            false,
            &[("order_id", "o1"), ("amount", amount)],
        ),
    ]
}

fn request(namespace: &str) -> EvaluateRequest {
    let mut request = fixture_request();
    request.namespace = namespace.into();
    request
}

fn assert_isolated(store: &Store, namespace: &str, amount: i64, region: &str) {
    let allow = PropertyAcl::allow_all();
    let loaded = store.load(namespace, "Customer", "c1", &allow).unwrap();
    assert_eq!(loaded.namespace, namespace);
    assert_eq!(loaded.props.get("region").map(String::as_str), Some(region));
    let oss = ObjectSet::new(LocalCompute);
    let eval = oss.evaluate(store, &request(namespace)).unwrap();
    assert_eq!(eval.two_hop_count, 1);
    assert_eq!(eval.sum_amount, amount);
    let listed = oss
        .evaluate(
            store,
            &EvaluateRequest {
                namespace: namespace.into(),
                root_kind: "Customer".into(),
                hops: vec![],
                sum_kind: "Customer".into(),
                sum_property: "region".into(),
                aggregate: Aggregate::CountAndSum,
                acl: allow.clone(),
                filter: None,
                predicate: None,
                object_bound: 8,
                sort: None,
                page_size: 0,
                cursor: None,
            },
        )
        .unwrap();
    assert_eq!(listed.objects.len(), 1);
    assert_eq!(listed.objects[0].namespace, namespace);
    assert_eq!(
        listed.objects[0].props.get("region").map(String::as_str),
        Some(region)
    );
    assert!(store.joins(namespace).unwrap().is_visible("Customer", "c1"));
}

/// Two namespaces with identical `(kind, key)` sets never share members,
/// counts, sums, or payloads across load, evaluate, hop, overlay, Action,
/// or hide.
#[test]
fn two_namespaces_with_identical_keys_never_see_each_other() {
    let (dir, log) = temp_log("namespace-isolate");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, twin("alpha", "10"));
    append_all(&mut store, twin("beta", "99"));

    assert_isolated(&store, "alpha", 10, "alpha");
    assert_isolated(&store, "beta", 99, "beta");

    store
        .apply_overlay(
            OverlayPatch {
                namespace: "alpha".into(),
                kind: "Customer".into(),
                key: "c1".into(),
                props: HashMap::from([("note".into(), "alpha-note".into())]),
                cleared: Vec::new(),
                action_id: None,
            },
            "act-alpha-note".into(),
            None,
        )
        .unwrap();
    let alpha_note = store
        .load("alpha", "Customer", "c1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        alpha_note.props.get("note").map(String::as_str),
        Some("alpha-note")
    );
    let beta_customer = store
        .load("beta", "Customer", "c1", &PropertyAcl::allow_all())
        .unwrap();
    assert!(!beta_customer.props.contains_key("note"));

    store
        .apply_action(
            Action {
                id: "act-s2".into(),
                namespace: "alpha".into(),
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
    assert_eq!(
        oss.evaluate(&store, &request("alpha")).unwrap().sum_amount,
        15
    );
    assert_eq!(
        oss.evaluate(&store, &request("beta")).unwrap().sum_amount,
        99
    );
    store
        .apply_action(
            Action {
                id: "act-s2".into(),
                namespace: "beta".into(),
                kind: "Shipment".into(),
                key: "s2".into(),
                props: HashMap::from([
                    ("order_id".into(), "o1".into()),
                    ("amount".into(), "1".into()),
                ]),
            },
            None,
        )
        .unwrap();
    assert_eq!(
        oss.evaluate(&store, &request("beta")).unwrap().sum_amount,
        100
    );

    store
        .hide("alpha", "Shipment", "s2", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        oss.evaluate(&store, &request("alpha")).unwrap().sum_amount,
        10
    );
    assert_eq!(
        oss.evaluate(&store, &request("beta")).unwrap().sum_amount,
        100
    );
    assert!(store
        .load("beta", "Shipment", "s2", &PropertyAcl::allow_all())
        .unwrap()
        .props
        .contains_key("amount"));

    drop(store);
    let reopened = Store::open(&log).unwrap();
    assert_isolated(&reopened, "alpha", 10, "alpha");
    assert_isolated(&reopened, "beta", 100, "beta");
    let note = reopened
        .load("alpha", "Customer", "c1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        note.props.get("note").map(String::as_str),
        Some("alpha-note")
    );
    drop(reopened);

    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let _ = std::fs::remove_file(Store::join_delta_path(&log));
    let rebuilt = Store::open(&log).unwrap();
    assert_isolated(&rebuilt, "alpha", 10, "alpha");
    assert_isolated(&rebuilt, "beta", 100, "beta");
    let rebuilt_note = rebuilt
        .load("alpha", "Customer", "c1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        rebuilt_note.props.get("note").map(String::as_str),
        Some("alpha-note")
    );

    let missing = rebuilt
        .load("", "Customer", "c1", &PropertyAcl::allow_all())
        .unwrap_err();
    assert!(missing.contains("namespace"), "{missing}");
    let unknown = rebuilt
        .load("ghost", "Customer", "c1", &PropertyAcl::allow_all())
        .unwrap_err();
    assert!(unknown.contains("unknown namespace"), "{unknown}");
    let eval_unknown = oss.evaluate(&rebuilt, &request("ghost")).unwrap_err();
    match eval_unknown {
        ComputeError::Load(err) => assert!(err.contains("unknown namespace"), "{err}"),
        other => panic!("expected load error, got {other:?}"),
    }
    let eval_missing = oss.evaluate(&rebuilt, &request("")).unwrap_err();
    match eval_missing {
        ComputeError::Load(err) => assert!(err.contains("namespace"), "{err}"),
        other => panic!("expected load error, got {other:?}"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn append_without_namespace_fails_closed() {
    let (dir, log) = temp_log("namespace-required");
    let mut store = Store::create(&log).unwrap();
    let mut record = rec("Customer", "c1", false, &[("region", "eu")]);
    record.namespace.clear();
    let err = store.append(record).unwrap_err();
    assert!(err.contains("namespace"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unknown_namespace_fails_closed_on_action_overlay_and_reserved_reads() {
    let (dir, log) = temp_log("namespace-unknown-writes");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, twin("alpha", "10"));

    let action_err = store
        .apply_action(
            Action {
                id: "act-ghost".into(),
                namespace: "ghost".into(),
                kind: "Shipment".into(),
                key: "s2".into(),
                props: HashMap::from([("order_id".into(), "o1".into())]),
            },
            None,
        )
        .unwrap_err();
    assert!(action_err.contains("unknown namespace"), "{action_err}");
    let overlay_err = store
        .apply_overlay(
            OverlayPatch {
                namespace: "ghost".into(),
                kind: "Customer".into(),
                key: "c1".into(),
                props: HashMap::from([("note".into(), "x".into())]),
                cleared: Vec::new(),
                action_id: None,
            },
            "act-ghost-note".into(),
            None,
        )
        .unwrap_err();
    assert!(overlay_err.contains("unknown namespace"), "{overlay_err}");
    let schema_err = store.schema("ghost", "Customer").unwrap_err();
    assert!(schema_err.contains("unknown namespace"), "{schema_err}");
    let overlay_read = store.overlay("ghost", "Customer", "c1").unwrap_err();
    assert!(overlay_read.contains("unknown namespace"), "{overlay_read}");
    let empty_schema = store.schema("", "Customer").unwrap_err();
    assert!(empty_schema.contains("namespace"), "{empty_schema}");
    assert!(store.schema("alpha", "Customer").unwrap().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}
