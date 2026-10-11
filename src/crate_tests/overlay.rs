use super::super::*;
use super::helpers::*;
use std::collections::HashMap;

#[test]
fn overlay_survives_refresh_and_stale_gen_fails() {
    let (dir, log) = temp_log("overlay-refresh");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, product_loop_seed());
    store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([("note".into(), "acked".into())]),
                cleared: Vec::new(),
                action_id: None,
                ..Default::default()
            },
            "act-inc-1-note".into(),
            Some(1),
        )
        .unwrap();
    let edited = store
        .load(NS, "incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(edited.props.get("note").map(String::as_str), Some("acked"));
    assert_eq!(edited.action_id.as_deref(), Some("act-inc-1-note"));
    let stale = store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([("note".into(), "again".into())]),
                cleared: Vec::new(),
                action_id: None,
                ..Default::default()
            },
            "act-inc-1-note-2".into(),
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
        .load(NS, "incident", "inc-1", &PropertyAcl::allow_all())
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
        .load(NS, "incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert!(!recreated.hidden);
    assert!(!recreated.props.contains_key("note"));
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert!(reopened.overlay(NS, "incident", "inc-1").unwrap().is_none());
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    assert!(!replayed
        .load(NS, "incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap()
        .props
        .contains_key("note"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn apply_overlay_logs_the_overlay_and_a_rematerialized_instance() {
    let (dir, log) = temp_log("overlay-rematerializes");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, product_loop_seed());
    store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([("note".into(), "acked".into())]),
                cleared: Vec::new(),
                action_id: None,
                ..Default::default()
            },
            "act-inc-1-note".into(),
            Some(1),
        )
        .unwrap();
    let live = store
        .load(NS, "incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(live.props.get("note").map(String::as_str), Some("acked"));
    assert_eq!(live.action_id.as_deref(), Some("act-inc-1-note"));

    let records = crate::log::read_records(&log).unwrap();
    let overlays = records
        .iter()
        .filter(|record| record.kind == OVERLAY_KIND)
        .count();
    let instances: Vec<_> = records
        .iter()
        .filter(|record| record.kind == "incident" && record.key == "inc-1")
        .collect();
    assert_eq!(overlays, 1);
    assert_eq!(
        instances.len(),
        2,
        "the log keeps the seed and one rematerialized instance (ADR 0027)"
    );
    assert_eq!(instances[1].gen, live.gen);
    assert_eq!(
        instances[1].props.get("note").map(String::as_str),
        Some("acked")
    );
    assert!(!instances[0].props.contains_key("note"));
    drop(store);

    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let rebuilt = Store::open(&log).unwrap();
    let replayed = rebuilt
        .load(NS, "incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(replayed, live);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn apply_overlay_replays_matching_id_and_fails_closed_on_conflict() {
    let (dir, log) = temp_log("overlay-retry");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, product_loop_seed());
    let patch = OverlayPatch {
        namespace: NS.into(),
        kind: "incident".into(),
        key: "inc-1".into(),
        props: HashMap::from([("note".into(), "acked".into())]),
        cleared: Vec::new(),
        action_id: None,
        ..Default::default()
    };
    store
        .apply_overlay(patch.clone(), "act-inc-1-note".into(), Some(1))
        .unwrap();
    let committed = store
        .load(NS, "incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    let fsync = store.log_fsync_count();
    store
        .apply_overlay(patch.clone(), "act-inc-1-note".into(), Some(0))
        .unwrap();
    let replayed = store
        .load(NS, "incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(replayed.gen, committed.gen);
    assert_eq!(
        replayed.props.get("note").map(String::as_str),
        Some("acked")
    );
    assert_eq!(store.log_fsync_count(), fsync);
    let conflict = store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([("note".into(), "other".into())]),
                cleared: Vec::new(),
                action_id: None,
                ..Default::default()
            },
            "act-inc-1-note".into(),
            None,
        )
        .unwrap_err();
    assert!(conflict.contains("body conflict"), "{conflict}");
    let stolen = store
        .apply_action(
            Action {
                namespace: NS.into(),
                id: "act-inc-1-note".into(),
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([("name".into(), "x".into())]),
            },
            None,
        )
        .unwrap_err();
    assert!(stolen.contains("already committed"), "{stolen}");
    drop(store);
    let mut reopened = Store::open(&log).unwrap();
    reopened
        .apply_overlay(patch, "act-inc-1-note".into(), Some(0))
        .unwrap();
    assert_eq!(
        reopened
            .load(NS, "incident", "inc-1", &PropertyAcl::allow_all())
            .unwrap()
            .gen,
        committed.gen
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn apply_action_replaces_without_merging_overlay() {
    let (dir, log) = temp_log("overlay-apply-action");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, product_loop_seed());
    store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([("note".into(), "acked".into())]),
                cleared: Vec::new(),
                action_id: None,
                ..Default::default()
            },
            "act-inc-1-note".into(),
            Some(1),
        )
        .unwrap();
    store
        .apply_action(
            Action {
                namespace: NS.into(),
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
        .load(NS, "incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert!(!replaced.props.contains_key("note"));
    assert_eq!(
        replaced.props.get("name").map(String::as_str),
        Some("elevated latency")
    );
    assert_eq!(replaced.action_id.as_deref(), Some("act-inc-1-rename"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn malformed_source_cursor_fails_closed() {
    let (dir, log) = temp_log("source-cursor-shape");
    let mut store = Store::create(&log).unwrap();
    let err = store
        .append(rec(SOURCE_KIND, "orders", false, &[("note", "bad")]))
        .unwrap_err();
    assert!(
        err.contains("source cursor"),
        "expected cursor shape error, got {err}"
    );
    let hidden = rec(SOURCE_KIND, "orders", true, &[(SOURCE_OFFSET, "1")]);
    let err = store.append(hidden).unwrap_err();
    assert!(
        err.contains("source cursor"),
        "expected hidden cursor error, got {err}"
    );
    store
        .append(rec(SOURCE_KIND, "orders", false, &[(SOURCE_OFFSET, "3")]))
        .unwrap();
    assert_eq!(
        store
            .load(NS, SOURCE_KIND, "orders", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get(SOURCE_OFFSET)
            .map(String::as_str),
        Some("3")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn overlay_cannot_describe_source_cursor() {
    let err = OverlayPatch {
        namespace: NS.into(),
        kind: SOURCE_KIND.into(),
        key: "orders".into(),
        props: HashMap::from([(SOURCE_OFFSET.into(), "1".into())]),
        cleared: Vec::new(),
        action_id: None,
        ..Default::default()
    }
    .to_record()
    .unwrap_err();
    assert!(
        err.contains("overlay cannot describe"),
        "expected reserved-kind rejection, got {err}"
    );
}

fn contact_schema(conflict: Option<ConflictStrategy>) -> SchemaDescriptor {
    SchemaDescriptor {
        kind: "contact".into(),
        properties: vec![
            "name".into(),
            "note".into(),
            "status".into(),
            "updated_at".into(),
        ],
        required: vec!["name".into()],
        links: Vec::new(),
        sums: Vec::new(),
        types: vec![("updated_at".into(), PropertyType::Timestamp)],
        conflict,
        conflict_properties: Vec::new(),
        conflict_time: Some("updated_at".into()),
    }
}

fn contact(
    key: &str,
    name: &str,
    status: &str,
    note: Option<&str>,
    updated_at: &str,
) -> ObjectRecord {
    let mut props = vec![
        ("name", name),
        ("status", status),
        ("updated_at", updated_at),
    ];
    if let Some(note) = note {
        props.push(("note", note));
    }
    rec("contact", key, false, &props)
}

fn overlay_rows(log: &std::path::Path) -> Vec<ObjectRecord> {
    crate::log::read_records(log)
        .unwrap()
        .into_iter()
        .filter(|record| record.kind == OVERLAY_KIND)
        .collect()
}

#[test]
fn default_user_wins_refresh_writes_no_conflict_audit() {
    let (dir, log) = temp_log("conflict-default");
    let mut store = Store::create(&log).unwrap();
    store
        .append(contact_schema(None).to_record("ns").unwrap())
        .unwrap();
    store
        .append(contact(
            "c-1",
            "Ada",
            "open",
            None,
            "2026-10-11T10:00:00.000Z",
        ))
        .unwrap();
    store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "contact".into(),
                key: "c-1".into(),
                props: HashMap::from([("status".into(), "acked".into())]),
                ..Default::default()
            },
            "act-c-1-status".into(),
            Some(1),
        )
        .unwrap();
    store
        .append(contact(
            "c-1",
            "Ada Lovelace",
            "closed",
            None,
            "2026-10-11T12:00:00.000Z",
        ))
        .unwrap();
    let live = store
        .load(NS, "contact", "c-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(live.props.get("status").map(String::as_str), Some("acked"));
    assert_eq!(
        live.props.get("name").map(String::as_str),
        Some("Ada Lovelace")
    );
    let overlays = overlay_rows(&log);
    assert_eq!(
        overlays.len(),
        1,
        "default user-wins writes no audit overlay"
    );
    assert!(!overlays[0].props.contains_key(OVERLAY_AT));
    assert!(!overlays[0].props.contains_key(OVERLAY_CONFLICT));
    assert!(OverlayPatch::recorded_lost(&overlays[0]).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn newest_timestamp_wins_over_refresh_edit_interleavings() {
    let (dir, log) = temp_log("conflict-newest");
    let mut store = Store::create(&log).unwrap();
    store
        .append(
            contact_schema(Some(ConflictStrategy::Newest))
                .to_record("ns")
                .unwrap(),
        )
        .unwrap();
    store
        .append(contact(
            "c-1",
            "Ada",
            "open",
            None,
            "2026-10-11T10:00:00.000Z",
        ))
        .unwrap();
    store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "contact".into(),
                key: "c-1".into(),
                props: HashMap::from([("status".into(), "acked".into())]),
                at: Some("2026-10-11T11:00:00.000Z".into()),
                ..Default::default()
            },
            "act-c-1-status".into(),
            Some(1),
        )
        .unwrap();
    store
        .append(contact(
            "c-1",
            "Ada",
            "closed",
            None,
            "2026-10-11T11:00:00.000Z",
        ))
        .unwrap();
    let tied = store
        .load(NS, "contact", "c-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(tied.props.get("status").map(String::as_str), Some("acked"));

    store
        .append(contact(
            "c-1",
            "Ada Lovelace",
            "closed",
            None,
            "2026-10-11T12:00:00.000Z",
        ))
        .unwrap();
    let source_newer = store
        .load(NS, "contact", "c-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        source_newer.props.get("status").map(String::as_str),
        Some("closed")
    );
    assert_eq!(
        source_newer.props.get("name").map(String::as_str),
        Some("Ada Lovelace")
    );
    let overlays = overlay_rows(&log);
    let audit = overlays.last().unwrap();
    assert_eq!(
        OverlayPatch::recorded_conflict(audit),
        Some("status:newest")
    );
    assert_eq!(
        OverlayPatch::recorded_lost(audit)
            .get("status")
            .map(String::as_str),
        Some("acked")
    );
    let overlay = store.overlay(NS, "contact", "c-1").unwrap().unwrap();
    assert_eq!(
        overlay.props.get("status").map(String::as_str),
        Some("acked")
    );

    store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "contact".into(),
                key: "c-1".into(),
                props: HashMap::from([("status".into(), "hold".into())]),
                at: Some("2026-10-11T13:00:00.000Z".into()),
                ..Default::default()
            },
            "act-c-1-hold".into(),
            Some(source_newer.gen),
        )
        .unwrap();
    let edited = store
        .load(NS, "contact", "c-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(edited.props.get("status").map(String::as_str), Some("hold"));
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert_eq!(
        reopened
            .load(NS, "contact", "c-1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("status")
            .map(String::as_str),
        Some("hold")
    );
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    let rebuilt = replayed
        .load(NS, "contact", "c-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        rebuilt.props.get("status").map(String::as_str),
        Some("hold")
    );
    assert_eq!(
        rebuilt.props.get("name").map(String::as_str),
        Some("Ada Lovelace")
    );
    let overlay = replayed.overlay(NS, "contact", "c-1").unwrap().unwrap();
    assert_eq!(
        overlay.props.get("status").map(String::as_str),
        Some("hold")
    );
    assert_eq!(overlay.at.as_deref(), Some("2026-10-11T13:00:00.000Z"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn per_property_strategy_keeps_user_note_and_takes_newer_status() {
    let (dir, log) = temp_log("conflict-per-property");
    let mut store = Store::create(&log).unwrap();
    let mut schema = contact_schema(Some(ConflictStrategy::User));
    schema.conflict_properties = vec![("status".into(), ConflictStrategy::Newest)];
    store.append(schema.to_record("ns").unwrap()).unwrap();
    store
        .append(contact(
            "c-1",
            "Ada",
            "open",
            Some("keep"),
            "2026-10-11T10:00:00.000Z",
        ))
        .unwrap();
    store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "contact".into(),
                key: "c-1".into(),
                props: HashMap::from([
                    ("status".into(), "acked".into()),
                    ("note".into(), "mine".into()),
                ]),
                at: Some("2026-10-11T11:00:00.000Z".into()),
                ..Default::default()
            },
            "act-c-1-edit".into(),
            Some(1),
        )
        .unwrap();
    store
        .append(contact(
            "c-1",
            "Ada",
            "closed",
            Some("source-note"),
            "2026-10-11T12:00:00.000Z",
        ))
        .unwrap();
    let live = store
        .load(NS, "contact", "c-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(live.props.get("status").map(String::as_str), Some("closed"));
    assert_eq!(live.props.get("note").map(String::as_str), Some("mine"));
    let audit = overlay_rows(&log).into_iter().last().unwrap();
    assert_eq!(
        OverlayPatch::recorded_conflict(&audit),
        Some("note:user,status:newest")
    );
    let lost = OverlayPatch::recorded_lost(&audit);
    assert_eq!(lost.get("status").map(String::as_str), Some("acked"));
    assert_eq!(lost.get("note").map(String::as_str), Some("source-note"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn changing_strategy_is_not_retroactive() {
    let (dir, log) = temp_log("conflict-not-retroactive");
    let mut store = Store::create(&log).unwrap();
    store
        .append(contact_schema(None).to_record("ns").unwrap())
        .unwrap();
    store
        .append(contact(
            "c-1",
            "Ada",
            "open",
            None,
            "2026-10-11T10:00:00.000Z",
        ))
        .unwrap();
    store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "contact".into(),
                key: "c-1".into(),
                props: HashMap::from([("status".into(), "acked".into())]),
                at: Some("2026-10-11T11:00:00.000Z".into()),
                ..Default::default()
            },
            "act-c-1-status".into(),
            Some(1),
        )
        .unwrap();
    store
        .append(contact(
            "c-1",
            "Ada",
            "closed",
            None,
            "2026-10-11T12:00:00.000Z",
        ))
        .unwrap();
    let user_wins = store
        .load(NS, "contact", "c-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        user_wins.props.get("status").map(String::as_str),
        Some("acked")
    );
    let gen = user_wins.gen;
    store
        .append(
            contact_schema(Some(ConflictStrategy::Newest))
                .to_record("ns")
                .unwrap(),
        )
        .unwrap();
    let unchanged = store
        .load(NS, "contact", "c-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(unchanged.gen, gen);
    assert_eq!(
        unchanged.props.get("status").map(String::as_str),
        Some("acked")
    );
    store
        .append(contact(
            "c-1",
            "Ada",
            "closed",
            None,
            "2026-10-11T12:30:00.000Z",
        ))
        .unwrap();
    let after = store
        .load(NS, "contact", "c-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        after.props.get("status").map(String::as_str),
        Some("closed")
    );
    assert!(after.gen > gen);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn newest_without_timestamps_fails_closed() {
    let (dir, log) = temp_log("conflict-missing-ts");
    let mut store = Store::create(&log).unwrap();
    store
        .append(
            contact_schema(Some(ConflictStrategy::Newest))
                .to_record("ns")
                .unwrap(),
        )
        .unwrap();
    store
        .append(contact(
            "c-1",
            "Ada",
            "open",
            None,
            "2026-10-11T10:00:00.000Z",
        ))
        .unwrap();
    let missing_at = store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "contact".into(),
                key: "c-1".into(),
                props: HashMap::from([("status".into(), "acked".into())]),
                ..Default::default()
            },
            "act-c-1-status".into(),
            Some(1),
        )
        .unwrap_err();
    assert!(
        missing_at.contains("overlay at"),
        "expected overlay at error, got {missing_at}"
    );
    store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "contact".into(),
                key: "c-1".into(),
                props: HashMap::from([("status".into(), "acked".into())]),
                at: Some("2026-10-11T11:00:00.000Z".into()),
                ..Default::default()
            },
            "act-c-1-status".into(),
            Some(1),
        )
        .unwrap();
    let mut stale = contact("c-1", "Ada", "closed", None, "2026-10-11T12:00:00.000Z");
    stale.props.remove("updated_at");
    let missing_source = store.append(stale).unwrap_err();
    assert!(
        missing_source.contains("source timestamp") || missing_source.contains("missing required"),
        "expected timestamp failure, got {missing_source}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn apply_overlay_replays_offset_timestamp() {
    let (dir, log) = temp_log("overlay-at-offset");
    let mut store = Store::create(&log).unwrap();
    store
        .append(
            contact_schema(Some(ConflictStrategy::Newest))
                .to_record("ns")
                .unwrap(),
        )
        .unwrap();
    store
        .append(contact(
            "c-1",
            "Ada",
            "open",
            None,
            "2026-10-11T10:00:00.000Z",
        ))
        .unwrap();
    let patch = OverlayPatch {
        namespace: NS.into(),
        kind: "contact".into(),
        key: "c-1".into(),
        props: HashMap::from([("status".into(), "acked".into())]),
        at: Some("2026-10-11T13:00:00+02:00".into()),
        ..Default::default()
    };
    store
        .apply_overlay(patch.clone(), "act-c-1-status".into(), Some(1))
        .unwrap();
    store
        .apply_overlay(patch, "act-c-1-status".into(), Some(0))
        .unwrap();
    let overlay = store.overlay(NS, "contact", "c-1").unwrap().unwrap();
    assert_eq!(overlay.at.as_deref(), Some("2026-10-11T11:00:00.000Z"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn failed_source_write_does_not_record_conflict_audit() {
    let (dir, log) = temp_log("conflict-no-audit-on-fail");
    let mut store = Store::create(&log).unwrap();
    store
        .append(
            contact_schema(Some(ConflictStrategy::Newest))
                .to_record("ns")
                .unwrap(),
        )
        .unwrap();
    store
        .append(contact(
            "c-1",
            "Ada",
            "open",
            None,
            "2026-10-11T10:00:00.000Z",
        ))
        .unwrap();
    store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "contact".into(),
                key: "c-1".into(),
                props: HashMap::from([("status".into(), "acked".into())]),
                at: Some("2026-10-11T11:00:00.000Z".into()),
                ..Default::default()
            },
            "act-c-1-status".into(),
            Some(1),
        )
        .unwrap();
    let overlay_before = store
        .load(NS, OVERLAY_KIND, "contact/c-1", &PropertyAcl::allow_all())
        .unwrap();
    let overlay_gen = overlay_before.gen;
    let committed_overlays = overlay_rows(&log).len();
    let mut bad = contact("c-1", "Ada", "closed", None, "2026-10-11T12:00:00.000Z");
    bad.props.insert("bogus".into(), "x".into());
    let err = store.append(bad).unwrap_err();
    assert!(err.contains("unknown property"), "{err}");
    let live_overlay = store
        .load(NS, OVERLAY_KIND, "contact/c-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        live_overlay.gen, overlay_gen,
        "failed merge must not install audit on the live overlay"
    );
    store
        .append(contact(
            "c-2",
            "Grace",
            "open",
            None,
            "2026-10-11T12:00:00.000Z",
        ))
        .unwrap();
    assert_eq!(
        overlay_rows(&log).len(),
        committed_overlays,
        "failed merge must not leave an audit for a later commit"
    );
    let live = store
        .load(NS, "contact", "c-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(live.props.get("status").map(String::as_str), Some("acked"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn newest_source_defeating_a_clear_records_audit() {
    let (dir, log) = temp_log("conflict-clear-loses");
    let mut store = Store::create(&log).unwrap();
    store
        .append(
            contact_schema(Some(ConflictStrategy::Newest))
                .to_record("ns")
                .unwrap(),
        )
        .unwrap();
    store
        .append(contact(
            "c-1",
            "Ada",
            "open",
            Some("keep"),
            "2026-10-11T10:00:00.000Z",
        ))
        .unwrap();
    store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "contact".into(),
                key: "c-1".into(),
                cleared: vec!["note".into()],
                at: Some("2026-10-11T11:00:00.000Z".into()),
                ..Default::default()
            },
            "act-c-1-clear".into(),
            Some(1),
        )
        .unwrap();
    store
        .append(contact(
            "c-1",
            "Ada",
            "open",
            Some("source-note"),
            "2026-10-11T12:00:00.000Z",
        ))
        .unwrap();
    let live = store
        .load(NS, "contact", "c-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        live.props.get("note").map(String::as_str),
        Some("source-note")
    );
    let audit = overlay_rows(&log).into_iter().last().unwrap();
    assert_eq!(OverlayPatch::recorded_conflict(&audit), Some("note:newest"));
    assert!(OverlayPatch::recorded_lost(&audit).is_empty());
    let overlay = store.overlay(NS, "contact", "c-1").unwrap().unwrap();
    assert_eq!(overlay.cleared, vec!["note".to_string()]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn apply_overlay_admits_patch_against_live_overridden_timestamp() {
    let (dir, log) = temp_log("conflict-admit-vs-live-clock");
    let mut store = Store::create(&log).unwrap();
    store
        .append(
            contact_schema(Some(ConflictStrategy::Newest))
                .to_record("ns")
                .unwrap(),
        )
        .unwrap();
    store
        .append(contact(
            "c-1",
            "Ada",
            "open",
            None,
            "2026-10-11T10:00:00.000Z",
        ))
        .unwrap();
    store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "contact".into(),
                key: "c-1".into(),
                props: HashMap::from([
                    ("status".into(), "acked".into()),
                    ("updated_at".into(), "2026-10-11T15:00:00.000Z".into()),
                ]),
                at: Some("2026-10-11T11:00:00.000Z".into()),
                ..Default::default()
            },
            "act-c-1-acked".into(),
            Some(1),
        )
        .unwrap();
    store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "contact".into(),
                key: "c-1".into(),
                props: HashMap::from([("status".into(), "hold".into())]),
                at: Some("2026-10-11T12:00:00.000Z".into()),
                ..Default::default()
            },
            "act-c-1-hold".into(),
            Some(2),
        )
        .unwrap();
    let live = store
        .load(NS, "contact", "c-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(live.props.get("status").map(String::as_str), Some("hold"));
    store
        .append(contact(
            "c-1",
            "Ada",
            "closed",
            None,
            "2026-10-11T10:30:00.000Z",
        ))
        .unwrap();
    let refreshed = store
        .load(NS, "contact", "c-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        refreshed.props.get("status").map(String::as_str),
        Some("hold")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn oversized_source_write_does_not_record_conflict_audit() {
    let (dir, log) = temp_log("conflict-no-audit-on-oversize");
    let mut store = Store::create(&log).unwrap();
    store
        .append(
            contact_schema(Some(ConflictStrategy::Newest))
                .to_record("ns")
                .unwrap(),
        )
        .unwrap();
    store
        .append(contact(
            "c-1",
            "Ada",
            "open",
            None,
            "2026-10-11T10:00:00.000Z",
        ))
        .unwrap();
    store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "contact".into(),
                key: "c-1".into(),
                props: HashMap::from([("status".into(), "acked".into())]),
                at: Some("2026-10-11T11:00:00.000Z".into()),
                ..Default::default()
            },
            "act-c-1-status".into(),
            Some(1),
        )
        .unwrap();
    let overlay_gen = store
        .load(NS, OVERLAY_KIND, "contact/c-1", &PropertyAcl::allow_all())
        .unwrap()
        .gen;
    let committed_overlays = overlay_rows(&log).len();
    let mut huge = contact("c-1", "Ada", "closed", None, "2026-10-11T12:00:00.000Z");
    huge.props.insert("name".into(), "n".repeat(5000));
    let err = store.append(huge).unwrap_err();
    assert!(err.contains("larger than a page"), "{err}");
    assert_eq!(
        store
            .load(NS, OVERLAY_KIND, "contact/c-1", &PropertyAcl::allow_all())
            .unwrap()
            .gen,
        overlay_gen
    );
    store
        .append(contact(
            "c-2",
            "Grace",
            "open",
            None,
            "2026-10-11T12:00:00.000Z",
        ))
        .unwrap();
    assert_eq!(overlay_rows(&log).len(), committed_overlays);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn oversized_conflict_audit_does_not_install_the_source_write() {
    let (dir, log) = temp_log("conflict-audit-oversize");
    let mut store = Store::create(&log).unwrap();
    store
        .append(
            contact_schema(Some(ConflictStrategy::Newest))
                .to_record("ns")
                .unwrap(),
        )
        .unwrap();
    store
        .append(contact(
            "c-1",
            "Ada",
            "open",
            None,
            "2026-10-11T10:00:00.000Z",
        ))
        .unwrap();
    let big = "s".repeat(2500);
    store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "contact".into(),
                key: "c-1".into(),
                props: HashMap::from([("status".into(), big.clone())]),
                at: Some("2026-10-11T11:00:00.000Z".into()),
                ..Default::default()
            },
            "act-c-1-status".into(),
            Some(1),
        )
        .unwrap();
    let before = store
        .load(NS, "contact", "c-1", &PropertyAcl::allow_all())
        .unwrap();
    let overlay_gen = store
        .load(NS, OVERLAY_KIND, "contact/c-1", &PropertyAcl::allow_all())
        .unwrap()
        .gen;
    let err = store
        .append(contact(
            "c-1",
            "Ada",
            "closed",
            None,
            "2026-10-11T12:00:00.000Z",
        ))
        .unwrap_err();
    assert!(err.contains("larger than a page"), "{err}");
    let live = store
        .load(NS, "contact", "c-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(live.gen, before.gen);
    assert_eq!(
        live.props.get("status").map(String::as_str),
        Some(big.as_str())
    );
    assert_eq!(
        store
            .load(NS, OVERLAY_KIND, "contact/c-1", &PropertyAcl::allow_all())
            .unwrap()
            .gen,
        overlay_gen
    );
    let _ = std::fs::remove_dir_all(&dir);
}
