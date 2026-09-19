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
            "act-inc-1-note".into(),
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
        .apply_action(Action {
            id: "act-inc-1-rename".into(),
            kind: "incident".into(),
            key: "inc-1".into(),
            props: HashMap::from([
                ("name".into(), "elevated latency".into()),
                ("affects".into(), "svc-api".into()),
            ]),
        })
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
