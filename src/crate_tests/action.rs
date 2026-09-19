use super::super::*;
use super::helpers::*;

#[test]
fn action_id_round_trips_and_empty_id_fails_closed() {
    let (dir, log) = temp_log("action-id");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
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
    let live = store
        .load("Shipment", "s2", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(live.action_id.as_deref(), Some("act-s2"));
    for i in 0..8 {
        store
            .apply_action(
                Action {
                    id: format!("act-s2-{i}"),
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
        .apply_action(
            Action {
                id: String::new(),
                kind: "Shipment".into(),
                key: "s3".into(),
                props: HashMap::from([("order_id".into(), "o1".into())]),
            },
            None,
        )
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

fn shipment_amount(amount: &str) -> HashMap<String, String> {
    HashMap::from([
        ("order_id".into(), "o1".into()),
        ("amount".into(), amount.into()),
    ])
}

#[test]
fn apply_action_replays_matching_id_and_fails_closed_on_conflict() {
    let (dir, log) = temp_log("action-retry");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    let first = Action {
        id: "act-inc-1-note".into(),
        kind: "Shipment".into(),
        key: "s2".into(),
        props: shipment_amount("5"),
    };
    store.apply_action(first.clone(), None).unwrap();
    let committed = store
        .load("Shipment", "s2", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(committed.action_id.as_deref(), Some("act-inc-1-note"));
    assert_eq!(committed.gen, 1);
    let fsync = store.log_fsync_count();
    store.apply_action(first.clone(), Some(0)).unwrap();
    let replayed = store
        .load("Shipment", "s2", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(replayed.gen, committed.gen);
    assert_eq!(replayed.action_id.as_deref(), Some("act-inc-1-note"));
    assert_eq!(replayed.props.get("amount").map(String::as_str), Some("5"));
    assert_eq!(store.log_fsync_count(), fsync);

    let body_conflict = store
        .apply_action(
            Action {
                id: "act-inc-1-note".into(),
                kind: "Shipment".into(),
                key: "s2".into(),
                props: shipment_amount("9"),
            },
            None,
        )
        .unwrap_err();
    assert!(body_conflict.contains("body conflict"), "{body_conflict}");
    assert_eq!(
        store
            .load("Shipment", "s2", &PropertyAcl::allow_all())
            .unwrap()
            .gen,
        committed.gen
    );

    let other_identity = store
        .apply_action(
            Action {
                id: "act-inc-1-note".into(),
                kind: "Shipment".into(),
                key: "s1".into(),
                props: shipment_amount("5"),
            },
            None,
        )
        .unwrap_err();
    assert!(
        other_identity.contains("already committed"),
        "{other_identity}"
    );

    store
        .apply_action(
            Action {
                id: "act-s2-later".into(),
                kind: "Shipment".into(),
                key: "s2".into(),
                props: shipment_amount("7"),
            },
            None,
        )
        .unwrap();
    let later = store
        .load("Shipment", "s2", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(later.action_id.as_deref(), Some("act-s2-later"));
    assert_eq!(later.gen, committed.gen + 1);
    let after_later = store.log_fsync_count();
    store.apply_action(first.clone(), None).unwrap();
    let after_first_replay = store
        .load("Shipment", "s2", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(after_first_replay.gen, later.gen);
    assert_eq!(
        after_first_replay.action_id.as_deref(),
        Some("act-s2-later")
    );
    assert_eq!(store.log_fsync_count(), after_later);

    let stale = store
        .apply_action(
            Action {
                id: "act-s2-stale".into(),
                kind: "Shipment".into(),
                key: "s2".into(),
                props: shipment_amount("8"),
            },
            Some(committed.gen),
        )
        .unwrap_err();
    assert!(stale.contains("stale generation"), "{stale}");
    assert_eq!(
        store
            .load("Shipment", "s2", &PropertyAcl::allow_all())
            .unwrap()
            .gen,
        later.gen
    );

    drop(store);
    let mut reopened = Store::open(&log).unwrap();
    let before = reopened
        .load("Shipment", "s2", &PropertyAcl::allow_all())
        .unwrap();
    reopened.apply_action(first.clone(), None).unwrap();
    let after_reopen = reopened
        .load("Shipment", "s2", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(after_reopen.gen, before.gen);
    assert_eq!(after_reopen.action_id.as_deref(), Some("act-s2-later"));
    drop(reopened);

    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let mut rebuilt = Store::open(&log).unwrap();
    let before_rebuild = rebuilt
        .load("Shipment", "s2", &PropertyAcl::allow_all())
        .unwrap();
    rebuilt.apply_action(first, None).unwrap();
    assert_eq!(
        rebuilt
            .load("Shipment", "s2", &PropertyAcl::allow_all())
            .unwrap()
            .gen,
        before_rebuild.gen
    );
    let _ = std::fs::remove_dir_all(&dir);
}
