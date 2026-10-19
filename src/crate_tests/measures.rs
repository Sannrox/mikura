use super::super::*;
use super::helpers::*;
use crate::joins::{MAX_CACHED_SUMS, MAX_LAZY_MEASURE_LEAVES};

#[test]
fn declared_sum_reads_parent_rollups() {
    let (dir, log) = temp_log("measure-rollup");
    let mut store = Store::create(&log).unwrap();
    store
        .append(shipment_sum_schema().to_record("ns").unwrap())
        .unwrap();
    append_all(&mut store, fixture());
    assert!(store
        .joins(NS)
        .unwrap()
        .has_declared_sum("Shipment", "amount"));
    assert_eq!(
        store
            .joins(NS)
            .unwrap()
            .last_hop_measure("Shipment", "order_id", "amount", "o1"),
        Some((1, 10))
    );
    assert!(store
        .joins(NS)
        .unwrap()
        .last_hop_measure("Shipment", "order_id", "amount", "o0")
        .is_none());
    let oss = ObjectSet::new(LocalCompute);
    let live = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(live.two_hop_count, 1);
    assert_eq!(live.sum_amount, 10);
    assert!(!store.joins(NS).unwrap().is_visible("Shipment", "s0"));

    store
        .append(rec(
            "Shipment",
            "s2",
            false,
            &[("order_id", "o1"), ("amount", "5")],
        ))
        .unwrap();
    assert_eq!(
        store
            .joins(NS)
            .unwrap()
            .last_hop_measure("Shipment", "order_id", "amount", "o1"),
        Some((2, 15))
    );
    let after = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(after.sum_amount, 15);

    let mut denied = fixture_request();
    denied.acl = PropertyAcl::deny_property("Shipment", "amount");
    assert!(matches!(
        oss.evaluate(&store, &denied),
        Err(ComputeError::Acl(AclError::Denied { .. }))
    ));
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert!(reopened
        .joins(NS)
        .unwrap()
        .has_declared_sum("Shipment", "amount"));
    assert_eq!(oss.evaluate(&reopened, &fixture_request()).unwrap(), after);
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    assert_eq!(oss.evaluate(&replayed, &fixture_request()).unwrap(), after);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn undeclared_sum_builds_cache_on_first_evaluate() {
    let (dir, log) = temp_log("measure-lazy");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    assert!(!store
        .joins(NS)
        .unwrap()
        .has_declared_sum("Shipment", "amount"));
    assert!(!store
        .joins(NS)
        .unwrap()
        .has_measure_sum("Shipment", "amount"));
    assert!(store
        .joins(NS)
        .unwrap()
        .last_hop_measure("Shipment", "order_id", "amount", "o1")
        .is_none());
    let oss = ObjectSet::new(LocalCompute);
    let live = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(live.two_hop_count, 1);
    assert_eq!(live.sum_amount, 10);
    assert!(!store
        .joins(NS)
        .unwrap()
        .has_declared_sum("Shipment", "amount"));
    assert!(store
        .joins(NS)
        .unwrap()
        .has_measure_sum("Shipment", "amount"));
    assert_eq!(
        store
            .joins(NS)
            .unwrap()
            .last_hop_measure("Shipment", "order_id", "amount", "o1"),
        Some((1, 10))
    );

    store
        .append(rec(
            "Shipment",
            "s2",
            false,
            &[("order_id", "o1"), ("amount", "5")],
        ))
        .unwrap();
    assert_eq!(
        store
            .joins(NS)
            .unwrap()
            .last_hop_measure("Shipment", "order_id", "amount", "o1"),
        Some((2, 15))
    );
    let after = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(after.sum_amount, 15);
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert!(!reopened
        .joins(NS)
        .unwrap()
        .has_declared_sum("Shipment", "amount"));
    assert!(reopened
        .joins(NS)
        .unwrap()
        .has_measure_sum("Shipment", "amount"));
    assert_eq!(
        reopened
            .joins(NS)
            .unwrap()
            .last_hop_measure("Shipment", "order_id", "amount", "o1"),
        Some((2, 15))
    );
    assert_eq!(oss.evaluate(&reopened, &fixture_request()).unwrap(), after);
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    assert_eq!(oss.evaluate(&replayed, &fixture_request()).unwrap(), after);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn undeclared_sum_overlay_and_hide_update_cache() {
    let (dir, log) = temp_log("measure-lazy-overlay");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    let oss = ObjectSet::new(LocalCompute);
    assert_eq!(
        oss.evaluate(&store, &fixture_request()).unwrap().sum_amount,
        10
    );
    store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "Shipment".into(),
                key: "s1".into(),
                props: HashMap::from([("amount".into(), "12".into())]),
                cleared: Vec::new(),
                action_id: None,
                ..Default::default()
            },
            "act-s1-amount".into(),
            Some(1),
        )
        .unwrap();
    assert_eq!(
        store
            .joins(NS)
            .unwrap()
            .last_hop_measure("Shipment", "order_id", "amount", "o1"),
        Some((1, 12))
    );
    store.append(rec("Order", "o1", true, &[])).unwrap();
    assert!(store
        .joins(NS)
        .unwrap()
        .last_hop_measure("Shipment", "order_id", "amount", "o1")
        .is_none());
    assert_eq!(
        oss.evaluate(&store, &fixture_request())
            .unwrap()
            .two_hop_count,
        0
    );
    store
        .append(rec("Order", "o1", false, &[("customer_id", "c1")]))
        .unwrap();
    assert_eq!(
        store
            .joins(NS)
            .unwrap()
            .last_hop_measure("Shipment", "order_id", "amount", "o1"),
        Some((1, 12))
    );
    assert_eq!(
        oss.evaluate(&store, &fixture_request()).unwrap().sum_amount,
        12
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn undeclared_sum_above_leaf_budget_fails_closed() {
    let (dir, log) = temp_log("measure-lazy-budget");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    let err = store
        .joins(NS)
        .unwrap()
        .ensure_cached_sum_limited("Shipment", "amount", 0)
        .unwrap_err();
    assert!(err.contains("budget"));
    let unknown = store
        .joins(NS)
        .unwrap()
        .ensure_cached_sum_limited("Shipment", "weight", 0)
        .unwrap_err();
    assert!(unknown.contains("budget"));
    let not_a_measure = store
        .joins(NS)
        .unwrap()
        .ensure_cached_sum_limited("Shipment", "s1", MAX_LAZY_MEASURE_LEAVES)
        .unwrap();
    assert!(!not_a_measure);
    assert!(store
        .joins(NS)
        .unwrap()
        .last_hop_measure("Shipment", "order_id", "amount", "o1")
        .is_none());
    let response = ObjectSet::new(LocalCompute)
        .evaluate(&store, &fixture_request())
        .unwrap();
    assert_eq!(response.sum_amount, 10);
    assert_eq!(
        store
            .joins(NS)
            .unwrap()
            .last_hop_measure("Shipment", "order_id", "amount", "o1"),
        Some((1, 10))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn undeclared_sum_cache_cap_fails_closed() {
    let (dir, log) = temp_log("measure-lazy-cap");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    for i in 0..MAX_CACHED_SUMS {
        let name = format!("p{i}");
        let key = format!("c{i}");
        store
            .append(rec(
                "Shipment",
                &key,
                false,
                &[("order_id", "o1"), (name.as_str(), "1")],
            ))
            .unwrap();
        assert!(store
            .joins(NS)
            .unwrap()
            .ensure_cached_sum_limited("Shipment", &name, MAX_LAZY_MEASURE_LEAVES)
            .unwrap());
    }
    store
        .append(rec(
            "Shipment",
            "cx",
            false,
            &[("order_id", "o1"), ("px", "1")],
        ))
        .unwrap();
    let cap = store
        .joins(NS)
        .unwrap()
        .ensure_cached_sum_limited("Shipment", "px", MAX_LAZY_MEASURE_LEAVES)
        .unwrap_err();
    assert!(cap.contains("cap"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn undeclared_sum_schema_refresh_keeps_apply_cache() {
    let (dir, log) = temp_log("measure-lazy-schema");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    let oss = ObjectSet::new(LocalCompute);
    assert_eq!(
        oss.evaluate(&store, &fixture_request()).unwrap().sum_amount,
        10
    );
    let mut descriptor = shipment_sum_schema();
    descriptor.sums.clear();
    store.append(descriptor.to_record("ns").unwrap()).unwrap();
    store
        .append(rec(
            "Shipment",
            "s2",
            false,
            &[("order_id", "o1"), ("amount", "5")],
        ))
        .unwrap();
    assert_eq!(
        store
            .joins(NS)
            .unwrap()
            .last_hop_measure("Shipment", "order_id", "amount", "o1"),
        Some((2, 15))
    );
    let after = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(after.sum_amount, 15);
    drop(store);
    let reopened = Store::open(&log).unwrap();
    assert_eq!(oss.evaluate(&reopened, &fixture_request()).unwrap(), after);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn undeclared_sum_keeps_shared_parent_key() {
    let (dir, log) = temp_log("measure-lazy-shared-key");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    store
        .append(rec("Order", "o2", false, &[("customer_id", "c1")]))
        .unwrap();
    store
        .append(rec(
            "Shipment",
            "s2",
            false,
            &[("order_id", "o2"), ("amount", "3")],
        ))
        .unwrap();
    store
        .append(rec("Other", "o1", false, &[("label", "x")]))
        .unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let live = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(live.two_hop_count, 1);
    assert_eq!(live.sum_amount, 13);
    store.append(rec("Other", "o1", true, &[])).unwrap();
    assert_eq!(
        store
            .joins(NS)
            .unwrap()
            .last_hop_measure("Shipment", "order_id", "amount", "o1"),
        Some((1, 10))
    );
    let after = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(after.two_hop_count, 1);
    assert_eq!(after.sum_amount, 13);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn undeclared_sum_hides_leaf_sharing_parent_key() {
    let (dir, log) = temp_log("measure-lazy-leaf-share");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    store
        .append(rec(
            "Shipment",
            "o1",
            false,
            &[("order_id", "o1"), ("amount", "7")],
        ))
        .unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let live = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(live.sum_amount, 17);
    store.append(rec("Shipment", "o1", true, &[])).unwrap();
    assert_eq!(
        store
            .joins(NS)
            .unwrap()
            .last_hop_measure("Shipment", "order_id", "amount", "o1"),
        Some((1, 10))
    );
    let after = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(after.sum_amount, 10);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn undeclared_sum_checkpoint_persists_cache() {
    let (dir, log) = temp_log("measure-lazy-log-ckpt");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    store.checkpoint().unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let live = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(live.sum_amount, 10);
    store.checkpoint().unwrap();
    drop(store);
    let reopened = Store::open(&log).unwrap();
    assert!(reopened
        .joins(NS)
        .unwrap()
        .has_measure_sum("Shipment", "amount"));
    assert_eq!(
        reopened
            .joins(NS)
            .unwrap()
            .last_hop_measure("Shipment", "order_id", "amount", "o1"),
        Some((1, 10))
    );
    assert_eq!(oss.evaluate(&reopened, &fixture_request()).unwrap(), live);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hide_and_overlay_update_parent_rollups() {
    let (dir, log) = temp_log("measure-hide-overlay");
    let mut store = Store::create(&log).unwrap();
    store
        .append(shipment_sum_schema().to_record("ns").unwrap())
        .unwrap();
    append_all(&mut store, fixture());
    store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "Shipment".into(),
                key: "s1".into(),
                props: HashMap::from([("amount".into(), "12".into())]),
                cleared: Vec::new(),
                action_id: None,
                ..Default::default()
            },
            "act-s1-amount".into(),
            Some(1),
        )
        .unwrap();
    assert_eq!(
        store
            .joins(NS)
            .unwrap()
            .last_hop_measure("Shipment", "order_id", "amount", "o1"),
        Some((1, 12))
    );
    let oss = ObjectSet::new(LocalCompute);
    assert_eq!(
        oss.evaluate(&store, &fixture_request()).unwrap().sum_amount,
        12
    );

    store.append(rec("Shipment", "s1", true, &[])).unwrap();
    assert!(store
        .joins(NS)
        .unwrap()
        .last_hop_measure("Shipment", "order_id", "amount", "o1")
        .is_none());
    assert_eq!(
        oss.evaluate(&store, &fixture_request()).unwrap().sum_amount,
        0
    );

    store
        .append(rec(
            "Shipment",
            "s1",
            false,
            &[("order_id", "o1"), ("amount", "10")],
        ))
        .unwrap();
    store.append(rec("Order", "o1", true, &[])).unwrap();
    assert!(store
        .joins(NS)
        .unwrap()
        .last_hop_measure("Shipment", "order_id", "amount", "o1")
        .is_none());
    assert_eq!(
        oss.evaluate(&store, &fixture_request())
            .unwrap()
            .two_hop_count,
        0
    );
    store
        .append(rec("Order", "o1", false, &[("customer_id", "c1")]))
        .unwrap();
    assert_eq!(
        store
            .joins(NS)
            .unwrap()
            .last_hop_measure("Shipment", "order_id", "amount", "o1"),
        Some((1, 10))
    );
    assert_eq!(
        oss.evaluate(&store, &fixture_request()).unwrap().sum_amount,
        10
    );
    let _ = std::fs::remove_dir_all(&dir);
}
