use super::super::*;
use super::helpers::*;

#[test]
fn declared_sum_reads_parent_rollups() {
    let (dir, log) = temp_log("measure-rollup");
    let mut store = Store::create(&log).unwrap();
    store
        .append(shipment_sum_schema().to_record().unwrap())
        .unwrap();
    append_all(&mut store, fixture());
    assert!(store.joins().has_declared_sum("Shipment", "amount"));
    assert_eq!(
        store
            .joins()
            .last_hop_measure("Shipment", "order_id", "amount", "o1"),
        Some((1, 10))
    );
    assert!(store
        .joins()
        .last_hop_measure("Shipment", "order_id", "amount", "o0")
        .is_none());
    let oss = ObjectSet::new(LocalCompute);
    let live = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(live.two_hop_count, 1);
    assert_eq!(live.sum_amount, 10);
    assert!(!store.joins().is_visible("Shipment", "s0"));

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
            .joins()
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
    assert!(reopened.joins().has_declared_sum("Shipment", "amount"));
    assert_eq!(oss.evaluate(&reopened, &fixture_request()).unwrap(), after);
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    assert_eq!(oss.evaluate(&replayed, &fixture_request()).unwrap(), after);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn undeclared_sum_still_leaf_walks() {
    let (dir, log) = temp_log("measure-undeclared");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    assert!(!store.joins().has_declared_sum("Shipment", "amount"));
    assert!(store
        .joins()
        .last_hop_measure("Shipment", "order_id", "amount", "o1")
        .is_none());
    let response = ObjectSet::new(LocalCompute)
        .evaluate(&store, &fixture_request())
        .unwrap();
    assert_eq!(response.two_hop_count, 1);
    assert_eq!(response.sum_amount, 10);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hide_and_overlay_update_parent_rollups() {
    let (dir, log) = temp_log("measure-hide-overlay");
    let mut store = Store::create(&log).unwrap();
    store
        .append(shipment_sum_schema().to_record().unwrap())
        .unwrap();
    append_all(&mut store, fixture());
    store
        .apply_overlay(
            OverlayPatch {
                kind: "Shipment".into(),
                key: "s1".into(),
                props: HashMap::from([("amount".into(), "12".into())]),
                cleared: Vec::new(),
                action_id: None,
            },
            "act-s1-amount".into(),
            Some(1),
        )
        .unwrap();
    assert_eq!(
        store
            .joins()
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
        .joins()
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
        .joins()
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
            .joins()
            .last_hop_measure("Shipment", "order_id", "amount", "o1"),
        Some((1, 10))
    );
    assert_eq!(
        oss.evaluate(&store, &fixture_request()).unwrap().sum_amount,
        10
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn old_mkjoin03_sidecar_fails_closed() {
    let (dir, log) = temp_log("measure-old-magic");
    let sidecar = Store::join_map_path(&log);
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    drop(store);
    let mut bytes = std::fs::read(&sidecar).unwrap();
    let crc_at = bytes.len() - 4;
    bytes[..8].copy_from_slice(b"MKJOIN03");
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&bytes[..crc_at]);
    bytes[crc_at..].copy_from_slice(&hasher.finalize().to_le_bytes());
    std::fs::write(&sidecar, &bytes).unwrap();
    let err = match Store::open(&log) {
        Err(err) => err,
        Ok(_) => panic!("MKJOIN03 should fail closed"),
    };
    assert!(err.contains("magic") || err.contains("checksum"), "{err}");
    std::fs::remove_file(&sidecar).unwrap();
    let recovered = Store::open(&log).unwrap();
    assert!(recovered.joins().is_visible("Customer", "c1"));
    let _ = std::fs::remove_dir_all(&dir);
}
