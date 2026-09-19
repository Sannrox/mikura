use super::super::*;
use super::helpers::*;

#[test]
fn hide_drops_identity_from_join_maps_and_rebuilds() {
    let (dir, log) = temp_log("hide-joins");
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
    assert!(store.joins().is_visible("incident", "inc-1"));
    store
        .hide("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert!(!store.joins().is_visible("incident", "inc-1"));
    let hidden = store
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert!(hidden.hidden);
    assert_eq!(
        hidden.props.get("name").map(String::as_str),
        Some("elevated latency")
    );
    assert_eq!(hidden.props.get("note").map(String::as_str), Some("acked"));
    let overlay = store
        .load(
            OVERLAY_KIND,
            &OverlayPatch::identity_key("incident", "inc-1"),
            &PropertyAcl::allow_all(),
        )
        .unwrap();
    assert!(overlay.hidden);
    assert!(store.overlay("incident", "inc-1").unwrap().is_none());
    let oss = ObjectSet::new(LocalCompute);
    let listed = oss.evaluate(&store, &list_prod_components(8)).unwrap();
    assert_eq!(listed.objects.len(), 1);
    assert_eq!(listed.objects[0].key, "svc-api");
    let hopped = oss.evaluate(&store, &hop_incident_to_component(8)).unwrap();
    assert_eq!(hopped.two_hop_count, 0);
    assert!(hopped.objects.is_empty());
    let missing = store
        .hide("incident", "nope", &PropertyAcl::allow_all())
        .unwrap_err();
    assert!(missing.contains("unknown identity"), "{missing}");
    let denied = store
        .hide(
            "component",
            "svc-api",
            &PropertyAcl::deny_property("component", "tier"),
        )
        .unwrap_err();
    assert!(denied.contains("Denied"), "{denied}");
    assert!(store.joins().is_visible("component", "svc-api"));
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert!(!reopened.joins().is_visible("incident", "inc-1"));
    assert!(
        reopened
            .load("incident", "inc-1", &PropertyAcl::allow_all())
            .unwrap()
            .hidden
    );
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    assert!(!replayed.joins().is_visible("incident", "inc-1"));
    let rebuilt = replayed
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert!(rebuilt.hidden);
    assert_eq!(rebuilt.props.get("note").map(String::as_str), Some("acked"));
    let _ = std::fs::remove_dir_all(&dir);
}
