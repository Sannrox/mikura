use super::super::*;
use super::helpers::*;

#[test]
fn committed_schema_validates_writes_and_rebuilds() {
    let (dir, log) = temp_log("schema-validate");
    let mut store = Store::create(&log).unwrap();
    store
        .append(rec(
            "component",
            "legacy",
            false,
            &[("alias", "pre-schema")],
        ))
        .unwrap();
    let (component_schema, incident_schema) = product_loop_schemas();
    store.append(component_schema.to_record().unwrap()).unwrap();
    store.append(incident_schema.to_record().unwrap()).unwrap();
    store
        .append(rec(
            "component",
            "svc-api",
            false,
            &[("name", "billing-api"), ("tier", "prod")],
        ))
        .unwrap();
    store
        .append(rec(
            "incident",
            "inc-1",
            false,
            &[("name", "elevated latency"), ("affects", "svc-api")],
        ))
        .unwrap();

    let missing = store
        .append(rec("component", "svc-web", false, &[("name", "web")]))
        .unwrap_err();
    assert!(missing.contains("missing required"), "{missing}");
    let extra = store
        .append(rec(
            "incident",
            "inc-2",
            false,
            &[("name", "n"), ("affects", "svc-api"), ("note", "acked")],
        ))
        .unwrap_err();
    assert!(extra.contains("unknown property"), "{extra}");
    assert!(store
        .load("component", "svc-web", &PropertyAcl::allow_all())
        .is_err());

    let historical = store
        .load("component", "legacy", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        historical.props.get("alias").map(String::as_str),
        Some("pre-schema")
    );
    let checked = store
        .load_with_schema(
            "component",
            "svc-api",
            &PropertyAcl::allow_all(),
            &component_schema,
        )
        .unwrap();
    assert_eq!(checked.props.get("tier").map(String::as_str), Some("prod"));
    let historical_check = store
        .load_with_schema(
            "component",
            "legacy",
            &PropertyAcl::allow_all(),
            &component_schema,
        )
        .unwrap_err();
    assert!(
        historical_check.contains("unknown property"),
        "{historical_check}"
    );

    store.append(rec("incident", "inc-1", true, &[])).unwrap();
    let hidden = store
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert!(hidden.hidden);
    assert!(!store.joins().is_visible("incident", "inc-1"));

    store
        .append(rec(
            "incident",
            "inc-3",
            false,
            &[("name", "disk full"), ("affects", "svc-api")],
        ))
        .unwrap();
    let incoming = EvaluateRequest {
        root_kind: "incident".into(),
        hops: vec![Hop {
            far_kind: "component".into(),
            join_property: "affects".into(),
            incoming: true,
            predicate: None,
        }],
        sum_kind: "component".into(),
        sum_property: "tier".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: None,
        object_bound: 0,
        sort: None,
        page_size: 0,
        cursor: None,
    };
    let hopped = ObjectSet::new(LocalCompute)
        .evaluate(&store, &incoming)
        .unwrap();
    assert_eq!(hopped.two_hop_count, 1);
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert_eq!(
        reopened.schema("component").unwrap().unwrap().kind,
        "component"
    );
    assert_eq!(
        reopened
            .load("component", "svc-api", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("name")
            .map(String::as_str),
        Some("billing-api")
    );
    assert!(
        reopened
            .load("incident", "inc-1", &PropertyAcl::allow_all())
            .unwrap()
            .hidden
    );
    drop(reopened);

    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    assert_eq!(
        replayed.schema("incident").unwrap().unwrap(),
        incident_schema
    );
    let rebuilt = replayed
        .load("component", "legacy", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        rebuilt.props.get("alias").map(String::as_str),
        Some("pre-schema")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn evaluate_returns_bounded_product_loop_objects() {
    let (dir, log) = temp_log("evaluate-objects");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, product_loop_seed());
    let oss = ObjectSet::new(LocalCompute);

    let listed = oss.evaluate(&store, &list_prod_components(8)).unwrap();
    assert_eq!(listed.two_hop_count, 1);
    assert_eq!(listed.objects.len(), 1);
    assert_eq!(listed.objects[0].kind, "component");
    assert_eq!(listed.objects[0].key, "svc-api");
    assert_eq!(
        listed.objects[0].props.get("tier").map(String::as_str),
        Some("prod")
    );

    let hopped = oss.evaluate(&store, &hop_incident_to_component(8)).unwrap();
    assert_eq!(hopped.objects.len(), 1);
    assert_eq!(hopped.objects[0].key, "svc-api");

    let count_only = oss.evaluate(&store, &list_prod_components(0)).unwrap();
    assert!(count_only.objects.is_empty());

    store
        .append(rec(
            "component",
            "svc-web",
            false,
            &[("name", "web"), ("tier", "prod")],
        ))
        .unwrap();
    let overflow = oss.evaluate(&store, &list_prod_components(1)).unwrap_err();
    assert!(
        matches!(overflow, ComputeError::ObjectBound { bound: 1, count: 2 }),
        "{overflow:?}"
    );

    let denied = EvaluateRequest {
        root_kind: "component".into(),
        hops: vec![],
        sum_kind: "component".into(),
        sum_property: "tier".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::deny_property("component", "name"),
        filter: Some(ExactMatch {
            property: "tier".into(),
            value: "prod".into(),
        }),
        predicate: None,
        object_bound: 8,
        sort: None,
        page_size: 0,
        cursor: None,
    };
    let redacted = oss.evaluate(&store, &denied).unwrap();
    assert_eq!(redacted.objects.len(), 2);
    assert!(!redacted.objects[0].props.contains_key("name"));
    assert_eq!(
        redacted.objects[0].props.get("tier").map(String::as_str),
        Some("prod")
    );
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert_eq!(
        oss.evaluate(&reopened, &hop_incident_to_component(8))
            .unwrap()
            .objects[0]
            .key,
        "svc-api"
    );
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    assert_eq!(
        oss.evaluate(&replayed, &list_prod_components(8))
            .unwrap()
            .objects
            .len(),
        2
    );
    let _ = std::fs::remove_dir_all(&dir);
}
