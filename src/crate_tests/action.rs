use super::super::*;
use super::helpers::*;

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
