use super::super::*;
use super::helpers::*;

fn incident_v1() -> SchemaDescriptor {
    SchemaDescriptor {
        kind: "incident".into(),
        properties: vec![
            "affects".into(),
            "name".into(),
            "note".into(),
            "priority".into(),
        ],
        required: vec!["name".into()],
        links: vec![SchemaLink {
            name: "affects".into(),
            far_kind: "component".into(),
            outgoing: true,
        }],
        sums: Vec::new(),
        types: Vec::new(),
        ..Default::default()
    }
}

fn seed(store: &mut Store) {
    store
        .append(
            SchemaDescriptor {
                kind: "component".into(),
                properties: vec!["name".into()],
                required: vec!["name".into()],
                links: Vec::new(),
                sums: Vec::new(),
                types: Vec::new(),
                ..Default::default()
            }
            .to_record("ns")
            .unwrap(),
        )
        .unwrap();
    store
        .append(incident_v1().to_record("ns").unwrap())
        .unwrap();
    store
        .append(rec(
            "component",
            "svc-api",
            false,
            &[("name", "billing-api")],
        ))
        .unwrap();
    store
        .append(rec(
            "incident",
            "inc-1",
            false,
            &[
                ("name", "elevated latency"),
                ("affects", "svc-api"),
                ("note", "acked"),
                ("priority", "1"),
            ],
        ))
        .unwrap();
}

fn overlay(store: &mut Store, key: &str, props: &[(&str, &str)], id: &str) -> Result<(), String> {
    store.apply_overlay(
        OverlayPatch {
            namespace: NS.into(),
            kind: "incident".into(),
            key: key.into(),
            props: props
                .iter()
                .map(|(name, value)| ((*name).into(), (*value).into()))
                .collect(),
            cleared: Vec::new(),
            action_id: None,
            ..Default::default()
        },
        id.into(),
        None,
    )
}

fn load_props(store: &Store, as_of: Option<u32>) -> HashMap<String, String> {
    let acl = PropertyAcl::allow_all();
    match as_of {
        None => store.load(NS, "incident", "inc-1", &acl).unwrap().props,
        Some(version) => {
            store
                .load_at_schema(NS, "incident", "inc-1", &acl, version)
                .unwrap()
                .props
        }
    }
}

fn reopen_and_rebuild(log: &std::path::Path) -> Store {
    drop(Store::open(log).unwrap());
    std::fs::remove_file(Store::join_map_path(log)).unwrap();
    let _ = std::fs::remove_file(Store::join_delta_path(log));
    let _ = std::fs::remove_file(Store::checkpoint_path(log));
    let _ = std::fs::remove_file(Store::checkpoint_prev_path(log));
    Store::open(log).unwrap()
}

#[test]
fn recast_migrates_or_rejects_overlays() {
    let (dir, log) = temp_log("schema-migrate-cast");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    overlay(&mut store, "inc-1", &[("priority", "2")], "act-cast-ok").unwrap();

    let mut recast = incident_v1();
    recast.types = vec![("priority".into(), PropertyType::Integer)];
    recast.migration.cast = vec![("priority".into(), PropertyType::Integer)];
    store.append(recast.to_record("ns").unwrap()).unwrap();

    let current = load_props(&store, None);
    assert_eq!(current.get("priority").map(String::as_str), Some("2"));
    assert_eq!(
        store
            .load(NS, "incident", "inc-1", &PropertyAcl::allow_all())
            .unwrap()
            .schema_version,
        2
    );
    assert_eq!(
        store
            .load_at_schema(NS, "incident", "inc-1", &PropertyAcl::allow_all(), 1)
            .unwrap()
            .schema_version,
        1
    );
    // Canonical live bytes; the pre-cast body is read through history (ADR 0036).
    assert_eq!(
        load_props(&store, Some(1))
            .get("priority")
            .map(String::as_str),
        Some("2")
    );

    let (dir2, log2) = temp_log("schema-migrate-cast-reject");
    let mut reject = Store::create(&log2).unwrap();
    seed(&mut reject);
    overlay(
        &mut reject,
        "inc-1",
        &[("priority", "paged")],
        "act-cast-bad",
    )
    .unwrap();
    let err = reject.append(recast.to_record("ns").unwrap()).unwrap_err();
    assert!(
        err.contains("cannot migrate overlays") && err.contains("priority"),
        "{err}"
    );
    assert!(reject
        .schema(NS, "incident")
        .unwrap()
        .unwrap()
        .types
        .is_empty());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

#[test]
fn rename_migrates_overlays_and_named_reads() {
    let (dir, log) = temp_log("schema-migrate-rename");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    overlay(&mut store, "inc-1", &[("note", "paged")], "act-note").unwrap();

    let mut renamed = incident_v1();
    renamed.properties = vec![
        "affects".into(),
        "memo".into(),
        "name".into(),
        "priority".into(),
    ];
    renamed.migration.rename = vec![("note".into(), "memo".into())];
    store.append(renamed.to_record("ns").unwrap()).unwrap();

    let current = load_props(&store, None);
    assert_eq!(current.get("memo").map(String::as_str), Some("paged"));
    assert!(!current.contains_key("note"));
    assert_eq!(
        load_props(&store, Some(1)).get("note").map(String::as_str),
        Some("paged")
    );

    let overlay_err = overlay(&mut store, "inc-1", &[("note", "stale")], "act-note-2").unwrap_err();
    assert!(overlay_err.contains("unknown property"), "{overlay_err}");
    overlay(&mut store, "inc-1", &[("memo", "keep")], "act-memo").unwrap();
    assert_eq!(
        load_props(&store, None).get("memo").map(String::as_str),
        Some("keep")
    );

    let rebuilt = reopen_and_rebuild(&log);
    assert_eq!(
        rebuilt
            .load(NS, "incident", "inc-1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("memo")
            .map(String::as_str),
        Some("keep")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn retarget_survives_overlay_on_the_link() {
    let (dir, log) = temp_log("schema-migrate-retarget");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    overlay(&mut store, "inc-1", &[("affects", "svc-api")], "act-link").unwrap();

    let mut retarget = incident_v1();
    retarget.links[0].far_kind = "service".into();
    retarget.migration.retarget = vec![SchemaLink {
        name: "affects".into(),
        far_kind: "service".into(),
        outgoing: true,
    }];
    store.append(retarget.to_record("ns").unwrap()).unwrap();
    assert_eq!(
        store.schema(NS, "incident").unwrap().unwrap().links[0].far_kind,
        "service"
    );
    assert_eq!(
        load_props(&store, None).get("affects").map(String::as_str),
        Some("svc-api")
    );
    let rebuilt = reopen_and_rebuild(&log);
    assert_eq!(
        rebuilt.schema(NS, "incident").unwrap().unwrap().links[0].far_kind,
        "service"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn drop_omits_current_load_and_rejects_overlay_edits() {
    let (dir, log) = temp_log("schema-migrate-drop");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    overlay(&mut store, "inc-1", &[("name", "paged")], "act-name").unwrap();

    let mut dropped = incident_v1();
    dropped.properties.retain(|name| name != "note");
    dropped.migration.drop = vec!["note".into()];
    store.append(dropped.to_record("ns").unwrap()).unwrap();
    assert!(!load_props(&store, None).contains_key("note"));
    assert_eq!(
        load_props(&store, Some(1)).get("note").map(String::as_str),
        Some("acked")
    );
    assert_eq!(
        load_props(&store, None).get("name").map(String::as_str),
        Some("paged")
    );
    let mut filtered = PropertyAcl::allow_all();
    filtered
        .insert_row_filter("incident", "note", ["acked"])
        .unwrap();
    assert!(store.load(NS, "incident", "inc-1", &filtered).is_err());
    assert_eq!(
        store
            .load_at_schema(NS, "incident", "inc-1", &filtered, 1)
            .unwrap()
            .props
            .get("note")
            .map(String::as_str),
        Some("acked")
    );

    let (dir2, log2) = temp_log("schema-migrate-drop-reject");
    let mut reject = Store::create(&log2).unwrap();
    seed(&mut reject);
    overlay(&mut reject, "inc-1", &[("note", "keep")], "act-drop-note").unwrap();
    let err = reject.append(dropped.to_record("ns").unwrap()).unwrap_err();
    assert!(
        err.contains("cannot migrate overlays") && err.contains("dropped note"),
        "{err}"
    );
    assert!(reject
        .schema(NS, "incident")
        .unwrap()
        .unwrap()
        .properties
        .contains(&"note".to_string()));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

#[test]
fn rollback_round_trip_restores_original_load() {
    let (dir, log) = temp_log("schema-migrate-rollback");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    let original = load_props(&store, None);

    let mut breaking = incident_v1();
    breaking.properties = vec![
        "affects".into(),
        "memo".into(),
        "name".into(),
        "priority".into(),
    ];
    breaking.migration.rename = vec![("note".into(), "memo".into())];
    store.append(breaking.to_record("ns").unwrap()).unwrap();
    assert_eq!(
        load_props(&store, None).get("memo").map(String::as_str),
        Some("acked")
    );

    let mut restored = incident_v1();
    restored.migration.rename = vec![("memo".into(), "note".into())];
    store.append(restored.to_record("ns").unwrap()).unwrap();
    assert_eq!(load_props(&store, None), original);
    assert_eq!(
        store
            .load(NS, "incident", "inc-1", &PropertyAcl::allow_all())
            .unwrap()
            .schema_version,
        3
    );

    let reopened = Store::open(&log).unwrap();
    assert_eq!(load_props(&reopened, None), original);
    drop(reopened);
    let rebuilt = reopen_and_rebuild(&log);
    assert_eq!(load_props(&rebuilt, None), original);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn first_descriptor_without_version_is_one() {
    let (dir, log) = temp_log("schema-migrate-v1-absent");
    let mut store = Store::create(&log).unwrap();
    let schema = incident_v1();
    let record = schema.to_record("ns").unwrap();
    assert!(!record.props.contains_key(SCHEMA_VERSION));
    store.append(record).unwrap();
    let loaded = store.schema(NS, "incident").unwrap().unwrap();
    assert_eq!(loaded.version, 1);
    assert_eq!(loaded.effective_version(), 1);
    store
        .append(rec(
            "incident",
            "inc-1",
            false,
            &[("name", "n"), ("affects", "svc-api")],
        ))
        .unwrap();
    assert_eq!(
        store
            .load(NS, "incident", "inc-1", &PropertyAcl::allow_all())
            .unwrap()
            .schema_version,
        1
    );
    let mut bump = incident_v1();
    bump.version = 1;
    let err = store.append(bump.to_record("ns").unwrap()).unwrap_err();
    assert!(err.contains("must be 2"), "{err}");
    bump.version = 3;
    let err = store.append(bump.to_record("ns").unwrap()).unwrap_err();
    assert!(err.contains("must be 2"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn instance_cast_failure_rejects_the_descriptor() {
    let (dir, log) = temp_log("schema-migrate-cast-instance");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    store
        .append(rec(
            "incident",
            "inc-2",
            false,
            &[
                ("name", "disk"),
                ("affects", "svc-api"),
                ("note", "n"),
                ("priority", "paged"),
            ],
        ))
        .unwrap();
    let mut recast = incident_v1();
    recast.types = vec![("priority".into(), PropertyType::Integer)];
    recast.migration.cast = vec![("priority".into(), PropertyType::Integer)];
    let err = store.append(recast.to_record("ns").unwrap()).unwrap_err();
    assert!(
        err.contains("cannot migrate instances") && err.contains("priority"),
        "{err}"
    );
    assert!(store
        .schema(NS, "incident")
        .unwrap()
        .unwrap()
        .types
        .is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rename_and_cast_rejects_invalid_source_values() {
    let (dir, log) = temp_log("schema-migrate-rename-cast");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    let mut breaking = incident_v1();
    breaking.properties = vec![
        "affects".into(),
        "name".into(),
        "priority".into(),
        "rank".into(),
    ];
    breaking.types = vec![("rank".into(), PropertyType::Integer)];
    breaking.migration.rename = vec![("note".into(), "rank".into())];
    breaking.migration.cast = vec![("rank".into(), PropertyType::Integer)];
    let err = store.append(breaking.to_record("ns").unwrap()).unwrap_err();
    assert!(
        err.contains("cannot migrate instances") && err.contains("rank"),
        "{err}"
    );
    assert_eq!(
        store
            .schema(NS, "incident")
            .unwrap()
            .unwrap()
            .effective_version(),
        1
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn checkpoint_reopen_keeps_named_schema_reads() {
    let (dir, log) = temp_log("schema-migrate-ckpt");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    let mut renamed = incident_v1();
    renamed.properties = vec![
        "affects".into(),
        "memo".into(),
        "name".into(),
        "priority".into(),
    ];
    renamed.migration.rename = vec![("note".into(), "memo".into())];
    store.append(renamed.to_record("ns").unwrap()).unwrap();
    store.checkpoint().unwrap();
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert_eq!(
        load_props(&reopened, None).get("memo").map(String::as_str),
        Some("acked")
    );
    assert_eq!(
        load_props(&reopened, Some(1))
            .get("note")
            .map(String::as_str),
        Some("acked")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn named_read_omits_denied_historical_names() {
    let (dir, log) = temp_log("schema-migrate-acl");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    let mut renamed = incident_v1();
    renamed.properties = vec![
        "affects".into(),
        "memo".into(),
        "name".into(),
        "priority".into(),
    ];
    renamed.migration.rename = vec![("note".into(), "memo".into())];
    store.append(renamed.to_record("ns").unwrap()).unwrap();
    let denied = PropertyAcl::deny_property("incident", "note");
    let historical = store
        .load_at_schema(NS, "incident", "inc-1", &denied, 1)
        .unwrap();
    assert!(!historical.props.contains_key("note"));
    assert_eq!(
        historical.props.get("name").map(String::as_str),
        Some("elevated latency")
    );
    let denied_current = PropertyAcl::deny_property("incident", "memo");
    let historical_allowed = store
        .load_at_schema(NS, "incident", "inc-1", &denied_current, 1)
        .unwrap();
    assert_eq!(
        historical_allowed.props.get("note").map(String::as_str),
        Some("acked")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn restoring_a_hidden_descriptor_advances_the_version() {
    let (dir, log) = temp_log("schema-migrate-hide-restore");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    let mut renamed = incident_v1();
    renamed.properties = vec![
        "affects".into(),
        "memo".into(),
        "name".into(),
        "priority".into(),
    ];
    renamed.migration.rename = vec![("note".into(), "memo".into())];
    store.append(renamed.to_record("ns").unwrap()).unwrap();
    let mut hidden = renamed.to_record("ns").unwrap();
    hidden.hidden = true;
    store.append(hidden).unwrap();
    let mut restored_body = renamed.clone();
    restored_body.migration = Default::default();
    restored_body.version = 0;
    store
        .append(restored_body.to_record("ns").unwrap())
        .unwrap();
    let restored = store.schema(NS, "incident").unwrap().unwrap();
    assert_eq!(restored.effective_version(), 3);
    assert_eq!(restored.properties, renamed.properties);
    assert_eq!(
        load_props(&store, None).get("memo").map(String::as_str),
        Some("acked")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sidecar_without_checkpoint_rebuilds_schema_history() {
    let (dir, log) = temp_log("schema-migrate-sidecar-history");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    let mut renamed = incident_v1();
    renamed.properties = vec![
        "affects".into(),
        "memo".into(),
        "name".into(),
        "priority".into(),
    ];
    renamed.migration.rename = vec![("note".into(), "memo".into())];
    store.append(renamed.to_record("ns").unwrap()).unwrap();
    drop(store);
    let _ = std::fs::remove_file(Store::checkpoint_path(&log));
    let _ = std::fs::remove_file(Store::checkpoint_prev_path(&log));
    let reopened = Store::open(&log).unwrap();
    assert_eq!(
        load_props(&reopened, Some(1))
            .get("note")
            .map(String::as_str),
        Some("acked")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unnamed_kind_rejects_unknown_schema_versions() {
    let (dir, log) = temp_log("schema-migrate-no-desc");
    let mut store = Store::create(&log).unwrap();
    store
        .append(rec("incident", "inc-1", false, &[("name", "n")]))
        .unwrap();
    store
        .load_at_schema(NS, "incident", "inc-1", &PropertyAcl::allow_all(), 1)
        .unwrap();
    let err = store
        .load_at_schema(NS, "incident", "inc-1", &PropertyAcl::allow_all(), 9)
        .unwrap_err();
    assert!(err.contains("unknown schema version 9"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn schema_action_id_retries_without_bumping_version() {
    let (dir, log) = temp_log("schema-migrate-schema-retry");
    let mut store = Store::create(&log).unwrap();
    let mut first = incident_v1().to_record("ns").unwrap();
    first.action_id = Some("act-schema-1".into());
    store.append(first.clone()).unwrap();
    assert_eq!(
        store
            .schema(NS, "incident")
            .unwrap()
            .unwrap()
            .effective_version(),
        1
    );
    store.append(first.clone()).unwrap();
    assert_eq!(
        store
            .schema(NS, "incident")
            .unwrap()
            .unwrap()
            .effective_version(),
        1
    );
    let mut mismatched = incident_v1();
    mismatched.version = 999;
    let mut mismatched = mismatched.to_record("ns").unwrap();
    mismatched.action_id = Some("act-schema-1".into());
    let err = store.append(mismatched).unwrap_err();
    assert!(
        err.contains("already committed") || err.contains("must be"),
        "{err}"
    );
    assert_eq!(
        store
            .schema(NS, "incident")
            .unwrap()
            .unwrap()
            .effective_version(),
        1
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn incident_eval(predicate: Option<Predicate>, hops: Vec<Hop>) -> EvaluateRequest {
    EvaluateRequest {
        namespace: NS.into(),
        root_kind: "incident".into(),
        hops,
        sum_kind: "incident".into(),
        sum_property: "priority".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate,
        object_bound: 8,
        sort: None,
        page_size: 0,
        cursor: None,
        breakdown: None,
    }
}

#[test]
fn drop_omits_current_evaluate() {
    let (dir, log) = temp_log("schema-migrate-drop-eval");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    let mut dropped = incident_v1();
    dropped
        .properties
        .retain(|name| name != "note" && name != "affects");
    dropped.links.clear();
    dropped.migration.drop = vec!["note".into(), "affects".into()];
    store.append(dropped.to_record("ns").unwrap()).unwrap();

    let oss = ObjectSet::new(LocalCompute);
    let missing = oss
        .evaluate(
            &store,
            &incident_eval(
                Some(Predicate::Missing {
                    property: "note".into(),
                }),
                Vec::new(),
            ),
        )
        .unwrap();
    assert_eq!(missing.two_hop_count, 1);
    assert!(!missing.objects[0].props.contains_key("note"));

    let eq_note = oss
        .evaluate(
            &store,
            &incident_eval(
                Some(Predicate::Eq {
                    property: "note".into(),
                    value: "acked".into(),
                }),
                Vec::new(),
            ),
        )
        .unwrap();
    assert_eq!(eq_note.two_hop_count, 0);

    let hopped = oss
        .evaluate(
            &store,
            &incident_eval(
                None,
                vec![Hop {
                    far_kind: "component".into(),
                    join_property: "affects".into(),
                    incoming: true,
                    predicate: None,
                }],
            ),
        )
        .unwrap();
    assert_eq!(hopped.two_hop_count, 0);
    assert!(hopped.objects.is_empty());

    let reverse = EvaluateRequest {
        namespace: NS.into(),
        root_kind: "component".into(),
        hops: vec![Hop {
            far_kind: "incident".into(),
            join_property: "affects".into(),
            incoming: false,
            predicate: None,
        }],
        sum_kind: "incident".into(),
        sum_property: "priority".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: None,
        object_bound: 8,
        sort: None,
        page_size: 0,
        cursor: None,
        breakdown: None,
    };
    let reverse = oss.evaluate(&store, &reverse).unwrap();
    assert_eq!(reverse.two_hop_count, 0);
    assert!(reverse.objects.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn abort_rebuilds_schema_history_without_checkpoint() {
    let (dir, log) = temp_log("schema-migrate-abort-history");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    let mut dropped = incident_v1();
    dropped.properties.retain(|name| name != "note");
    dropped.migration.drop = vec!["note".into()];
    store.append(dropped.to_record("ns").unwrap()).unwrap();
    let _ = std::fs::remove_file(Store::checkpoint_path(&log));
    let _ = std::fs::remove_file(Store::checkpoint_prev_path(&log));

    let mut first = rec(
        "incident",
        "inc-a",
        false,
        &[("name", "a"), ("affects", "svc-api")],
    );
    first.action_id = Some("act-same".into());
    store.append(first).unwrap();
    let mut retry = rec(
        "incident",
        "inc-b",
        false,
        &[("name", "b"), ("affects", "svc-api")],
    );
    retry.action_id = Some("act-same".into());
    store
        .append_batch(vec![
            rec(
                "incident",
                "inc-new",
                false,
                &[("name", "n"), ("affects", "svc-api")],
            ),
            retry,
        ])
        .unwrap_err();

    assert!(store
        .load(NS, "incident", "inc-new", &PropertyAcl::allow_all())
        .is_err());
    assert!(!load_props(&store, None).contains_key("note"));
    assert_eq!(
        load_props(&store, Some(1)).get("note").map(String::as_str),
        Some("acked")
    );
    assert_eq!(
        store
            .schema(NS, "incident")
            .unwrap()
            .unwrap()
            .effective_version(),
        2
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reused_name_after_rename_keeps_historical_value() {
    let (dir, log) = temp_log("schema-migrate-reuse-note");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    let mut renamed = incident_v1();
    renamed.properties = vec![
        "affects".into(),
        "memo".into(),
        "name".into(),
        "priority".into(),
    ];
    renamed.migration.rename = vec![("note".into(), "memo".into())];
    store.append(renamed.to_record("ns").unwrap()).unwrap();
    let mut restored = renamed.clone();
    restored.properties.push("note".into());
    restored.properties.sort();
    restored.migration = Default::default();
    store.append(restored.to_record("ns").unwrap()).unwrap();
    store
        .append(rec(
            "incident",
            "inc-1",
            false,
            &[
                ("name", "elevated latency"),
                ("affects", "svc-api"),
                ("memo", "acked"),
                ("note", "brand-new"),
                ("priority", "1"),
            ],
        ))
        .unwrap();
    assert_eq!(
        load_props(&store, None).get("note").map(String::as_str),
        Some("brand-new")
    );
    assert_eq!(
        load_props(&store, Some(1)).get("note").map(String::as_str),
        Some("acked")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn legacy_checkpoint_open_rewrites_complete_schema_history() {
    let (dir, log) = temp_log("schema-migrate-legacy-ckpt");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    let mut renamed = incident_v1();
    renamed.properties = vec![
        "affects".into(),
        "memo".into(),
        "name".into(),
        "priority".into(),
    ];
    renamed.migration.rename = vec![("note".into(), "memo".into())];
    store.append(renamed.to_record("ns").unwrap()).unwrap();
    store.checkpoint().unwrap();
    store
        .persist_legacy_log_checkpoint_without_schema_history()
        .unwrap();
    drop(store);
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let _ = std::fs::remove_file(Store::join_delta_path(&log));

    let recovered = Store::open(&log).unwrap();
    assert_eq!(
        load_props(&recovered, Some(1))
            .get("note")
            .map(String::as_str),
        Some("acked")
    );
    drop(recovered);

    let trusted = Store::open(&log).unwrap();
    assert_eq!(
        load_props(&trusted, Some(1))
            .get("note")
            .map(String::as_str),
        Some("acked")
    );
    assert_eq!(
        load_props(&trusted, None).get("memo").map(String::as_str),
        Some("acked")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn named_read_omits_properties_added_later() {
    let (dir, log) = temp_log("schema-migrate-later-prop");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    let mut added = incident_v1();
    added.properties.push("memo".into());
    added.properties.sort();
    store.append(added.to_record("ns").unwrap()).unwrap();
    store
        .append(rec(
            "incident",
            "inc-1",
            false,
            &[
                ("name", "elevated latency"),
                ("affects", "svc-api"),
                ("note", "acked"),
                ("priority", "1"),
                ("memo", "later"),
            ],
        ))
        .unwrap();
    assert_eq!(
        load_props(&store, None).get("memo").map(String::as_str),
        Some("later")
    );
    assert!(!load_props(&store, Some(1)).contains_key("memo"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hidden_descriptor_does_not_apply_current_omissions() {
    let (dir, log) = temp_log("schema-migrate-hidden-desc");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    let mut dropped = incident_v1();
    dropped.properties.retain(|name| name != "note");
    dropped.migration.drop = vec!["note".into()];
    store.append(dropped.to_record("ns").unwrap()).unwrap();
    let mut hidden = dropped.to_record("ns").unwrap();
    hidden.hidden = true;
    store.append(hidden).unwrap();
    store
        .append(rec(
            "incident",
            "inc-2",
            false,
            &[("name", "open"), ("note", "visible-again")],
        ))
        .unwrap();
    assert_eq!(
        store
            .load(NS, "incident", "inc-2", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("note")
            .map(String::as_str),
        Some("visible-again")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rename_onto_previously_dropped_name_is_rejected() {
    let (dir, log) = temp_log("schema-migrate-rename-onto-drop");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    let mut dropped = incident_v1();
    dropped.properties.retain(|name| name != "note");
    dropped.migration.drop = vec!["note".into()];
    store.append(dropped.to_record("ns").unwrap()).unwrap();
    let mut renamed = dropped.clone();
    renamed.properties = vec!["affects".into(), "note".into(), "priority".into()];
    renamed.required = vec!["note".into()];
    renamed.migration = Default::default();
    renamed.migration.rename = vec![("name".into(), "note".into())];
    let err = store.append(renamed.to_record("ns").unwrap()).unwrap_err();
    assert!(err.contains("collides with a dropped property"), "{err}");
    assert_eq!(
        load_props(&store, Some(1)).get("note").map(String::as_str),
        Some("acked")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stale_sidecar_replays_chained_renames() {
    let (dir, log) = temp_log("schema-migrate-stale-sidecar");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    store.checkpoint().unwrap();
    let sidecar = Store::join_map_path(&log);
    let snapshot = dir.join("joins.v1");
    std::fs::copy(&sidecar, &snapshot).unwrap();

    let mut to_memo = incident_v1();
    to_memo.properties = vec![
        "affects".into(),
        "memo".into(),
        "name".into(),
        "priority".into(),
    ];
    to_memo.migration.rename = vec![("note".into(), "memo".into())];
    store.append(to_memo.to_record("ns").unwrap()).unwrap();

    let mut to_summary = to_memo.clone();
    to_summary.properties = vec![
        "affects".into(),
        "name".into(),
        "priority".into(),
        "summary".into(),
    ];
    to_summary.migration = Default::default();
    to_summary.migration.rename = vec![("memo".into(), "summary".into())];
    store.append(to_summary.to_record("ns").unwrap()).unwrap();
    assert_eq!(
        load_props(&store, None).get("summary").map(String::as_str),
        Some("acked")
    );
    drop(store);

    std::fs::copy(&snapshot, &sidecar).unwrap();
    let _ = std::fs::remove_file(Store::join_delta_path(&log));
    let reopened = Store::open(&log).unwrap();
    assert_eq!(
        load_props(&reopened, None)
            .get("summary")
            .map(String::as_str),
        Some("acked")
    );
    assert_eq!(
        load_props(&reopened, Some(1))
            .get("note")
            .map(String::as_str),
        Some("acked")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn compact_to_keeps_dropped_values_for_named_reads() {
    let (dir, log) = temp_log("schema-migrate-compact");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    let mut dropped = incident_v1();
    dropped.properties.retain(|name| name != "note");
    dropped.migration.drop = vec!["note".into()];
    store.append(dropped.to_record("ns").unwrap()).unwrap();
    let compact = dir.join("compact.mikura");
    store.compact_to(&compact).unwrap();
    drop(store);
    let compacted = Store::open(&compact).unwrap();
    assert!(!load_props(&compacted, None).contains_key("note"));
    assert_eq!(
        compacted
            .load_at_schema(NS, "incident", "inc-1", &PropertyAcl::allow_all(), 1)
            .unwrap()
            .props
            .get("note")
            .map(String::as_str),
        Some("acked")
    );
    drop(compacted);
    std::fs::remove_file(Store::join_map_path(&compact)).unwrap();
    let _ = std::fs::remove_file(Store::join_delta_path(&compact));
    let from_ckpt = Store::open(&compact).unwrap();
    assert_eq!(
        from_ckpt
            .load_at_schema(NS, "incident", "inc-1", &PropertyAcl::allow_all(), 1)
            .unwrap()
            .props
            .get("note")
            .map(String::as_str),
        Some("acked")
    );
    drop(from_ckpt);
    std::fs::remove_file(Store::checkpoint_path(&compact)).unwrap();
    let _ = std::fs::remove_file(Store::checkpoint_prev_path(&compact));
    let from_log = Store::open(&compact).unwrap();
    assert!(!load_props(&from_log, None).contains_key("note"));
    assert_eq!(
        from_log
            .load_at_schema(NS, "incident", "inc-1", &PropertyAcl::allow_all(), 1)
            .unwrap()
            .props
            .get("note")
            .map(String::as_str),
        Some("acked")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn compact_to_preserves_live_schema_generation_and_action_id() {
    let (dir, log) = temp_log("schema-migrate-compact-schema-meta");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    let mut dropped = incident_v1();
    dropped.properties.retain(|name| name != "note");
    dropped.migration.drop = vec!["note".into()];
    let mut dropped = dropped.to_record("ns").unwrap();
    dropped.action_id = Some("act-schema-v2".into());
    store.append(dropped).unwrap();
    let live_incident = store
        .load(NS, SCHEMA_KIND, "incident", &PropertyAcl::allow_all())
        .unwrap();
    let live_component = store
        .load(NS, SCHEMA_KIND, "component", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(live_incident.gen, 2);
    assert_eq!(live_incident.action_id.as_deref(), Some("act-schema-v2"));
    assert_eq!(live_component.gen, 1);
    let compact = dir.join("compact.mikura");
    store.compact_to(&compact).unwrap();
    drop(store);

    let from_ckpt = Store::open(&compact).unwrap();
    let ckpt_incident = from_ckpt
        .load(NS, SCHEMA_KIND, "incident", &PropertyAcl::allow_all())
        .unwrap();
    let ckpt_component = from_ckpt
        .load(NS, SCHEMA_KIND, "component", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(ckpt_incident.gen, live_incident.gen);
    assert_eq!(ckpt_incident.action_id, live_incident.action_id);
    assert_eq!(ckpt_component.gen, live_component.gen);
    assert_eq!(ckpt_component.action_id, live_component.action_id);
    drop(from_ckpt);

    std::fs::remove_file(Store::checkpoint_path(&compact)).unwrap();
    let _ = std::fs::remove_file(Store::checkpoint_prev_path(&compact));
    let from_log = Store::open(&compact).unwrap();
    let log_incident = from_log
        .load(NS, SCHEMA_KIND, "incident", &PropertyAcl::allow_all())
        .unwrap();
    let log_component = from_log
        .load(NS, SCHEMA_KIND, "component", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(log_incident.gen, live_incident.gen);
    assert_eq!(log_incident.action_id, live_incident.action_id);
    assert_eq!(log_component.gen, live_component.gen);
    assert_eq!(log_component.action_id, live_component.action_id);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn compact_to_preserves_instance_schema_version() {
    let (dir, log) = temp_log("schema-migrate-compact-schema-version");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    let mut dropped = incident_v1();
    dropped.properties.retain(|name| name != "note");
    dropped.migration.drop = vec!["note".into()];
    store.append(dropped.to_record("ns").unwrap()).unwrap();
    store
        .append(rec(
            "incident",
            "inc-2",
            false,
            &[
                ("name", "second"),
                ("affects", "svc-api"),
                ("priority", "3"),
            ],
        ))
        .unwrap();
    assert_eq!(
        store
            .load(NS, "incident", "inc-2", &PropertyAcl::allow_all())
            .unwrap()
            .schema_version,
        2
    );
    drop(store);
    let mut reopened = Store::open(&log).unwrap();
    let compact = dir.join("compact.mikura");
    reopened.compact_to(&compact).unwrap();
    drop(reopened);
    let compacted = crate::log::read_records(&compact).unwrap();
    let inc2 = compacted
        .iter()
        .find(|record| record.kind == "incident" && record.key == "inc-2")
        .unwrap();
    assert_eq!(inc2.schema_version, 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rename_preserves_overlay_conflict_audit() {
    let (dir, log) = temp_log("schema-migrate-overlay-audit");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    let mut with_conflict = incident_v1();
    with_conflict.conflict = Some(ConflictStrategy::User);
    store
        .append(with_conflict.to_record("ns").unwrap())
        .unwrap();
    overlay(
        &mut store,
        "inc-1",
        &[("priority", "2")],
        "act-priority-audit",
    )
    .unwrap();
    store
        .append(rec(
            "incident",
            "inc-1",
            false,
            &[
                ("name", "elevated latency"),
                ("affects", "svc-api"),
                ("note", "acked"),
                ("priority", "9"),
            ],
        ))
        .unwrap();
    let overlays = crate::log::read_records(&log)
        .unwrap()
        .into_iter()
        .filter(|record| record.kind == OVERLAY_KIND)
        .collect::<Vec<_>>();
    let audit = overlays.last().unwrap();
    assert_eq!(
        OverlayPatch::recorded_conflict(audit),
        Some("priority:user")
    );
    assert_eq!(
        OverlayPatch::recorded_lost(audit)
            .get("priority")
            .map(String::as_str),
        Some("9")
    );

    let mut renamed = incident_v1();
    renamed.conflict = Some(ConflictStrategy::User);
    renamed.properties = vec![
        "affects".into(),
        "name".into(),
        "note".into(),
        "severity".into(),
    ];
    renamed.migration.rename = vec![("priority".into(), "severity".into())];
    store.append(renamed.to_record("ns").unwrap()).unwrap();

    let overlays = crate::log::read_records(&log)
        .unwrap()
        .into_iter()
        .filter(|record| record.kind == OVERLAY_KIND)
        .collect::<Vec<_>>();
    let migrated = overlays.last().unwrap();
    assert_eq!(
        OverlayPatch::recorded_conflict(migrated),
        Some("severity:user")
    );
    assert_eq!(
        OverlayPatch::recorded_lost(migrated)
            .get("severity")
            .map(String::as_str),
        Some("9")
    );
    assert!(!migrated.props.keys().any(|name| name.contains("priority")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rename_onto_a_declared_property_is_rejected() {
    let (dir, log) = temp_log("schema-migrate-rename-onto-declared");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    // inc-1 has no `priority` value, so instance migration alone would accept
    // the rename; but `priority` is declared on version 1, so reading version 1
    // could not tell a renamed `note` from an original `priority`.
    store
        .append(rec(
            "incident",
            "inc-1",
            false,
            &[
                ("name", "elevated latency"),
                ("affects", "svc-api"),
                ("note", "acked"),
            ],
        ))
        .unwrap();
    let mut renamed = incident_v1();
    renamed.properties = vec!["affects".into(), "name".into(), "priority".into()];
    renamed.migration.rename = vec![("note".into(), "priority".into())];
    let err = store.append(renamed.to_record("ns").unwrap()).unwrap_err();
    assert!(err.contains("already declared"), "{err}");
    assert_eq!(
        load_props(&store, None).get("note").map(String::as_str),
        Some("acked")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reopen_keeps_instance_versions_newer_than_the_log_checkpoint() {
    let (dir, log) = temp_log("schema-migrate-stale-checkpoint-versions");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    let mut dropped = incident_v1();
    dropped.properties.retain(|name| name != "note");
    dropped.migration.drop = vec!["note".into()];
    store.append(dropped.to_record("ns").unwrap()).unwrap();
    let second = |name: &str| {
        rec(
            "incident",
            "inc-2",
            false,
            &[("name", name), ("affects", "svc-api"), ("priority", "3")],
        )
    };
    store.append(second("second")).unwrap();
    store.checkpoint().unwrap();

    // An additive replacement is version 3; rewrite inc-2 under it.
    let mut tagged = incident_v1();
    tagged.properties.retain(|name| name != "note");
    tagged.properties.push("tag".into());
    store.append(tagged.to_record("ns").unwrap()).unwrap();
    store.append(second("second, again")).unwrap();
    drop(store);

    let mut reopened = Store::open(&log).unwrap();
    assert_eq!(
        reopened
            .load(NS, "incident", "inc-2", &PropertyAcl::allow_all())
            .unwrap()
            .schema_version,
        3
    );
    let compact = dir.join("compact.mikura");
    reopened.compact_to(&compact).unwrap();
    let compacted = crate::log::read_records(&compact).unwrap();
    let inc2 = compacted
        .iter()
        .find(|record| record.kind == "incident" && record.key == "inc-2")
        .unwrap();
    assert_eq!(inc2.schema_version, 3);
    let _ = std::fs::remove_dir_all(&dir);
}
