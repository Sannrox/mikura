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
    let edited = store
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(edited.props.get("note").map(String::as_str), Some("acked"));
    assert_eq!(edited.action_id.as_deref(), Some("act-inc-1-note"));
    let stale = store
        .apply_overlay(
            OverlayPatch {
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([("note".into(), "again".into())]),
                cleared: Vec::new(),
                action_id: None,
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
        .load("incident", "inc-1", &PropertyAcl::allow_all())
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
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert!(!recreated.hidden);
    assert!(!recreated.props.contains_key("note"));
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert!(reopened.overlay("incident", "inc-1").unwrap().is_none());
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    assert!(!replayed
        .load("incident", "inc-1", &PropertyAcl::allow_all())
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
    let live = store
        .load("incident", "inc-1", &PropertyAcl::allow_all())
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
        .load("incident", "inc-1", &PropertyAcl::allow_all())
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
        kind: "incident".into(),
        key: "inc-1".into(),
        props: HashMap::from([("note".into(), "acked".into())]),
        cleared: Vec::new(),
        action_id: None,
    };
    store
        .apply_overlay(patch.clone(), "act-inc-1-note".into(), Some(1))
        .unwrap();
    let committed = store
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    let fsync = store.log_fsync_count();
    store
        .apply_overlay(patch.clone(), "act-inc-1-note".into(), Some(0))
        .unwrap();
    let replayed = store
        .load("incident", "inc-1", &PropertyAcl::allow_all())
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
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([("note".into(), "other".into())]),
                cleared: Vec::new(),
                action_id: None,
            },
            "act-inc-1-note".into(),
            None,
        )
        .unwrap_err();
    assert!(conflict.contains("body conflict"), "{conflict}");
    let stolen = store
        .apply_action(
            Action {
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
            .load("incident", "inc-1", &PropertyAcl::allow_all())
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
    assert_eq!(
        replaced.props.get("name").map(String::as_str),
        Some("elevated latency")
    );
    assert_eq!(replaced.action_id.as_deref(), Some("act-inc-1-rename"));
    let _ = std::fs::remove_dir_all(&dir);
}
