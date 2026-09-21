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

#[test]
fn ingest_append_replays_matching_action_id_and_fails_closed_on_remap() {
    let (dir, log) = temp_log("ingest-action-unique");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    let mut first = rec(
        "Shipment",
        "s2",
        false,
        &[("order_id", "o1"), ("amount", "5")],
    );
    first.action_id = Some("act-ingest-1".into());
    store.append(first.clone()).unwrap();
    let committed = store
        .load("Shipment", "s2", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(committed.action_id.as_deref(), Some("act-ingest-1"));
    let pages = store.committed_pages();

    store.append_uncommitted(first.clone()).unwrap();
    let replayed = store
        .load("Shipment", "s2", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(replayed.gen, committed.gen);
    assert_eq!(replayed.props.get("amount").map(String::as_str), Some("5"));
    assert_eq!(store.committed_pages(), pages);

    let mut remapped = rec(
        "Shipment",
        "s3",
        false,
        &[("order_id", "o1"), ("amount", "7")],
    );
    remapped.action_id = Some("act-ingest-1".into());
    let remap = store.append_uncommitted(remapped).unwrap_err();
    assert!(remap.contains("already committed"), "{remap}");
    assert!(store
        .load("Shipment", "s3", &PropertyAcl::allow_all())
        .is_err());

    let mut conflict = rec(
        "Shipment",
        "s2",
        false,
        &[("order_id", "o1"), ("amount", "9")],
    );
    conflict.action_id = Some("act-ingest-1".into());
    let body = store.append_uncommitted(conflict).unwrap_err();
    assert!(body.contains("body conflict"), "{body}");
    assert_eq!(
        store
            .load("Shipment", "s2", &PropertyAcl::allow_all())
            .unwrap()
            .gen,
        committed.gen
    );

    store
        .hide("Shipment", "s2", &PropertyAcl::allow_all())
        .unwrap();
    let hidden = store
        .load("Shipment", "s2", &PropertyAcl::allow_all())
        .unwrap();
    assert!(hidden.hidden);
    assert_eq!(hidden.action_id.as_deref(), Some("act-ingest-1"));
    drop(store);

    let mut reopened = Store::open(&log).unwrap();
    let mut steal = rec(
        "Shipment",
        "s4",
        false,
        &[("order_id", "o1"), ("amount", "1")],
    );
    steal.action_id = Some("act-ingest-1".into());
    let after_open = reopened.append_uncommitted(steal.clone()).unwrap_err();
    assert!(after_open.contains("already committed"), "{after_open}");
    drop(reopened);

    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let mut rebuilt = Store::open(&log).unwrap();
    let after_rebuild = rebuilt.append_uncommitted(steal).unwrap_err();
    assert!(
        after_rebuild.contains("already committed"),
        "{after_rebuild}"
    );
    rebuilt.append_uncommitted(first).unwrap();
    assert_eq!(
        rebuilt
            .load("Shipment", "s2", &PropertyAcl::allow_all())
            .unwrap()
            .gen,
        hidden.gen
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ingest_action_id_replays_merged_overlay_and_canonical_typed_body() {
    let (dir, log) = temp_log("ingest-action-transform");
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
    let mut source = rec(
        "incident",
        "inc-1",
        false,
        &[("name", "elevated latency"), ("affects", "svc-api")],
    );
    source.action_id = Some("act-src-1".into());
    store.append(source.clone()).unwrap();
    let merged = store
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(merged.props.get("note").map(String::as_str), Some("acked"));
    assert_eq!(merged.action_id.as_deref(), Some("act-src-1"));
    let merged_gen = merged.gen;
    store.append_uncommitted(source).unwrap();
    assert_eq!(
        store
            .load("incident", "inc-1", &PropertyAcl::allow_all())
            .unwrap()
            .gen,
        merged_gen
    );

    store
        .append(
            SchemaDescriptor {
                kind: "incident".into(),
                properties: vec!["affects".into(), "name".into(), "opened_at".into()],
                required: vec!["name".into()],
                links: vec![SchemaLink {
                    name: "affects".into(),
                    far_kind: "component".into(),
                    outgoing: true,
                }],
                sums: Vec::new(),
                types: vec![("opened_at".into(), PropertyType::Timestamp)],
            }
            .to_record()
            .unwrap(),
        )
        .unwrap();
    let mut typed = rec(
        "incident",
        "inc-2",
        false,
        &[
            ("name", "other"),
            ("affects", "svc-api"),
            ("opened_at", "2026-09-19T18:00:00+01:00"),
        ],
    );
    typed.action_id = Some("act-typed-1".into());
    store.append(typed.clone()).unwrap();
    let canonical = store
        .load("incident", "inc-2", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        canonical.props.get("opened_at").map(String::as_str),
        Some("2026-09-19T17:00:00.000Z")
    );
    let typed_gen = canonical.gen;
    store.append_uncommitted(typed).unwrap();
    assert_eq!(
        store
            .load("incident", "inc-2", &PropertyAcl::allow_all())
            .unwrap()
            .gen,
        typed_gen
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ingest_hide_copies_action_id_without_claiming_a_new_identity() {
    let (dir, log) = temp_log("ingest-action-hide");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    let mut first = rec(
        "Shipment",
        "s2",
        false,
        &[("order_id", "o1"), ("amount", "5")],
    );
    first.action_id = Some("act-ingest-1".into());
    store.append(first.clone()).unwrap();
    let mut hide = first.clone();
    hide.hidden = true;
    store.append_uncommitted(hide).unwrap();
    store.commit().unwrap();
    let hidden = store
        .load("Shipment", "s2", &PropertyAcl::allow_all())
        .unwrap();
    assert!(hidden.hidden);
    assert_eq!(hidden.action_id.as_deref(), Some("act-ingest-1"));
    let mut stolen = rec(
        "Shipment",
        "s3",
        true,
        &[("order_id", "o1"), ("amount", "5")],
    );
    stolen.action_id = Some("act-ingest-1".into());
    let remap = store.append_uncommitted(stolen).unwrap_err();
    assert!(remap.contains("already committed"), "{remap}");
    assert!(store
        .load("Shipment", "s3", &PropertyAcl::allow_all())
        .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
