use super::super::*;
use super::helpers::*;

fn hide_incident() -> PropertyAcl {
    let mut acl = PropertyAcl::allow_all();
    acl.insert_hide_identity("incident", "inc-1").unwrap();
    acl
}

#[test]
fn restriction_hides_load_evaluate_and_mutations() {
    let (dir, log) = temp_log("restriction-view");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, product_loop_seed());
    let open = PropertyAcl::allow_all();
    let hidden = hide_incident();
    let missing = store.load("incident", "inc-1", &hidden).unwrap_err();
    assert!(missing.contains("unknown identity"), "{missing}");
    let visible = store.load("incident", "inc-1", &open).unwrap();
    assert_eq!(
        visible.props.get("name").map(String::as_str),
        Some("elevated latency")
    );

    let oss = ObjectSet::new(LocalCompute);
    let listed_open = oss.evaluate(&store, &list_prod_components(8)).unwrap();
    assert_eq!(listed_open.objects.len(), 1);
    let mut listed_hidden = list_prod_components(8);
    listed_hidden.acl = hidden.clone();
    let listed_hidden = oss.evaluate(&store, &listed_hidden).unwrap();
    assert_eq!(listed_hidden.objects.len(), 1);
    assert_eq!(listed_hidden.objects[0].key, "svc-api");

    let hopped_open = oss.evaluate(&store, &hop_incident_to_component(8)).unwrap();
    assert_eq!(hopped_open.two_hop_count, 1);
    let mut hopped_hidden = hop_incident_to_component(8);
    hopped_hidden.acl = hidden.clone();
    let hopped_hidden = oss.evaluate(&store, &hopped_hidden).unwrap();
    assert_eq!(hopped_hidden.two_hop_count, 0);
    assert!(hopped_hidden.objects.is_empty());

    let overlay_err = store
        .apply_overlay_in_view(
            OverlayPatch {
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([("note".into(), "secret".into())]),
                cleared: Vec::new(),
                action_id: None,
            },
            "act-hidden".into(),
            None,
            &hidden,
        )
        .unwrap_err();
    assert!(overlay_err.contains("not in this view"), "{overlay_err}");
    let action_err = store
        .apply_action_in_view(
            Action {
                id: "act-replace".into(),
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([("name".into(), "x".into())]),
            },
            None,
            &hidden,
        )
        .unwrap_err();
    assert!(action_err.contains("not in this view"), "{action_err}");
    let hide_err = store.hide("incident", "inc-1", &hidden).unwrap_err();
    assert!(hide_err.contains("not in this view"), "{hide_err}");
    assert_eq!(
        store
            .load("incident", "inc-1", &open)
            .unwrap()
            .props
            .get("name")
            .map(String::as_str),
        Some("elevated latency")
    );

    drop(store);
    let reopened = Store::open(&log).unwrap();
    assert!(reopened
        .load("incident", "inc-1", &hidden)
        .unwrap_err()
        .contains("unknown identity"));
    assert_eq!(
        reopened
            .load("incident", "inc-1", &open)
            .unwrap()
            .props
            .get("name")
            .map(String::as_str),
        Some("elevated latency")
    );
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let rebuilt = Store::open(&log).unwrap();
    assert!(rebuilt
        .load("incident", "inc-1", &hidden)
        .unwrap_err()
        .contains("unknown identity"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn restriction_cursor_binds_hide_list() {
    let (dir, log) = temp_log("restriction-cursor");
    let mut store = Store::create(&log).unwrap();
    append_all(
        &mut store,
        vec![
            rec(
                "incident",
                "inc-1",
                false,
                &[("name", "a"), ("priority", "1")],
            ),
            rec(
                "incident",
                "inc-2",
                false,
                &[("name", "b"), ("priority", "2")],
            ),
        ],
    );
    let oss = ObjectSet::new(LocalCompute);
    let first = oss
        .evaluate(
            &store,
            &EvaluateRequest {
                root_kind: "incident".into(),
                hops: vec![],
                sum_kind: "incident".into(),
                sum_property: "priority".into(),
                aggregate: Aggregate::CountAndSum,
                acl: PropertyAcl::allow_all(),
                filter: None,
                predicate: None,
                object_bound: 8,
                sort: Some(Sort::by("priority")),
                page_size: 1,
                cursor: None,
            },
        )
        .unwrap();
    let cursor = first.cursor.expect("page cursor");
    let mut other = PropertyAcl::allow_all();
    other.insert_hide_identity("incident", "inc-2").unwrap();
    let mismatched = oss.evaluate(
        &store,
        &EvaluateRequest {
            root_kind: "incident".into(),
            hops: vec![],
            sum_kind: "incident".into(),
            sum_property: "priority".into(),
            aggregate: Aggregate::CountAndSum,
            acl: other,
            filter: None,
            predicate: None,
            object_bound: 8,
            sort: Some(Sort::by("priority")),
            page_size: 1,
            cursor: Some(cursor),
        },
    );
    assert!(
        matches!(mismatched, Err(ComputeError::Page(_))),
        "{mismatched:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
