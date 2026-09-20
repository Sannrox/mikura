use super::super::*;
use super::helpers::*;

fn component_schema() -> SchemaDescriptor {
    SchemaDescriptor {
        kind: "component".into(),
        properties: vec!["name".into(), "tier".into()],
        required: vec!["name".into(), "tier".into()],
        links: Vec::new(),
        sums: Vec::new(),
        types: Vec::new(),
    }
}

fn incident_schema() -> SchemaDescriptor {
    SchemaDescriptor {
        kind: "incident".into(),
        properties: vec![
            "affects".into(),
            "name".into(),
            "note".into(),
            "open".into(),
            "priority".into(),
        ],
        required: vec!["name".into()],
        links: vec![SchemaLink {
            name: "affects".into(),
            far_kind: "component".into(),
            outgoing: true,
        }],
        sums: Vec::new(),
        types: vec![
            ("open".into(), PropertyType::Boolean),
            ("priority".into(), PropertyType::Integer),
        ],
    }
}

fn proposed_fixture() -> Vec<ObjectRecord> {
    vec![
        rec(
            "component",
            "svc-api",
            false,
            &[("name", "billing-api"), ("tier", "prod")],
        ),
        rec(
            "component",
            "svc-web",
            false,
            &[("name", "web"), ("tier", "prod")],
        ),
        rec(
            "component",
            "svc-batch",
            false,
            &[("name", "batch"), ("tier", "staging")],
        ),
        rec(
            "incident",
            "inc-1",
            false,
            &[
                ("name", "elevated latency"),
                ("affects", "svc-api"),
                ("open", "true"),
                ("priority", "2"),
            ],
        ),
        rec(
            "incident",
            "inc-2",
            false,
            &[
                ("name", "disk full"),
                ("affects", "svc-api"),
                ("open", "false"),
                ("priority", "3"),
                ("note", "pager"),
            ],
        ),
        rec(
            "incident",
            "inc-3",
            false,
            &[
                ("name", "job delay"),
                ("affects", "svc-batch"),
                ("open", "true"),
                ("priority", "1"),
            ],
        ),
    ]
}

fn seed(store: &mut Store) {
    store
        .append(component_schema().to_record().unwrap())
        .unwrap();
    store
        .append(incident_schema().to_record().unwrap())
        .unwrap();
    append_all(store, proposed_fixture());
}

fn list_incidents(predicate: Predicate, bound: usize) -> EvaluateRequest {
    EvaluateRequest {
        root_kind: "incident".into(),
        hops: vec![],
        sum_kind: "incident".into(),
        sum_property: "priority".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: Some(predicate),
        object_bound: bound,
        sort: None,
        page_size: 0,
        cursor: None,
    }
}

fn keys(response: &EvaluateResponse) -> Vec<&str> {
    let mut keys: Vec<&str> = response
        .objects
        .iter()
        .map(|row| row.key.as_str())
        .collect();
    keys.sort();
    keys
}

#[test]
fn composed_predicates_filter_then_hop_and_rebuild() {
    let (dir, log) = temp_log("predicate-compose");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    let oss = ObjectSet::new(LocalCompute);

    let open = oss
        .evaluate(&store, &list_incidents(Predicate::eq("open", "true"), 8))
        .unwrap();
    assert_eq!(keys(&open), ["inc-1", "inc-3"]);

    let range = oss
        .evaluate(
            &store,
            &list_incidents(Predicate::range("priority", Some("2"), Some("3")), 8),
        )
        .unwrap();
    assert_eq!(keys(&range), ["inc-1", "inc-2"]);

    let missing = oss
        .evaluate(&store, &list_incidents(Predicate::missing("note"), 8))
        .unwrap();
    assert_eq!(keys(&missing), ["inc-1", "inc-3"]);

    let composed = oss
        .evaluate(
            &store,
            &list_incidents(
                Predicate::and(vec![
                    Predicate::eq("open", "true"),
                    Predicate::neq("priority", "1"),
                ]),
                8,
            ),
        )
        .unwrap();
    assert_eq!(keys(&composed), ["inc-1"]);

    let negated = oss
        .evaluate(&store, &list_incidents(!Predicate::eq("open", "true"), 8))
        .unwrap();
    assert_eq!(keys(&negated), ["inc-2"]);

    let filter_then_hop = EvaluateRequest {
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
        predicate: Some(Predicate::eq("open", "true")),
        object_bound: 8,
        sort: None,
        page_size: 0,
        cursor: None,
    };
    let hopped = oss.evaluate(&store, &filter_then_hop).unwrap();
    assert_eq!(keys(&hopped), ["svc-api", "svc-batch"]);

    let hop_then_filter = EvaluateRequest {
        root_kind: "incident".into(),
        hops: vec![Hop {
            far_kind: "component".into(),
            join_property: "affects".into(),
            incoming: true,
            predicate: Some(Predicate::eq("tier", "prod")),
        }],
        sum_kind: "component".into(),
        sum_property: "tier".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: None,
        object_bound: 8,
        sort: None,
        page_size: 0,
        cursor: None,
    };
    let filtered = oss.evaluate(&store, &hop_then_filter).unwrap();
    assert_eq!(keys(&filtered), ["svc-api"]);
    assert_eq!(filtered.two_hop_count, 2);

    let integer_is_not_string =
        oss.evaluate(&store, &list_incidents(Predicate::eq("priority", "01"), 8));
    assert!(
        matches!(integer_is_not_string, Err(ComputeError::Predicate(_))),
        "{integer_is_not_string:?}"
    );

    let denied = EvaluateRequest {
        root_kind: "incident".into(),
        hops: vec![],
        sum_kind: "incident".into(),
        sum_property: "priority".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::deny_property("incident", "open"),
        filter: None,
        predicate: Some(Predicate::eq("open", "true")),
        object_bound: 8,
        sort: None,
        page_size: 0,
        cursor: None,
    };
    assert!(matches!(
        oss.evaluate(&store, &denied),
        Err(ComputeError::Acl(AclError::Denied { .. }))
    ));

    let boolean_range = oss.evaluate(
        &store,
        &list_incidents(Predicate::range("open", Some("false"), Some("true")), 8),
    );
    assert!(
        matches!(boolean_range, Err(ComputeError::Predicate(_))),
        "{boolean_range:?}"
    );

    let both = EvaluateRequest {
        root_kind: "incident".into(),
        hops: vec![],
        sum_kind: "incident".into(),
        sum_property: "priority".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: Some(ExactMatch {
            property: "name".into(),
            value: "disk full".into(),
        }),
        predicate: Some(Predicate::eq("open", "true")),
        object_bound: 8,
        sort: None,
        page_size: 0,
        cursor: None,
    };
    assert!(matches!(
        oss.evaluate(&store, &both),
        Err(ComputeError::Predicate(_))
    ));

    store
        .apply_overlay(
            OverlayPatch {
                kind: "incident".into(),
                key: "inc-2".into(),
                props: [("open".into(), "true".into())].into(),
                cleared: Vec::new(),
                action_id: None,
            },
            "edit-open".into(),
            None,
        )
        .unwrap();
    let after_overlay = oss
        .evaluate(&store, &list_incidents(Predicate::eq("open", "true"), 8))
        .unwrap();
    assert_eq!(keys(&after_overlay), ["inc-1", "inc-2", "inc-3"]);

    store
        .hide("incident", "inc-3", &PropertyAcl::allow_all())
        .unwrap();
    let after_hide = oss
        .evaluate(&store, &list_incidents(Predicate::eq("open", "true"), 8))
        .unwrap();
    assert_eq!(keys(&after_hide), ["inc-1", "inc-2"]);

    drop(store);
    let reopened = Store::open(&log).unwrap();
    assert_eq!(
        keys(
            &oss.evaluate(&reopened, &list_incidents(Predicate::eq("open", "true"), 8))
                .unwrap()
        ),
        ["inc-1", "inc-2"]
    );
    let sidecar = log.with_extension("mikura.joins");
    let _ = std::fs::remove_file(&sidecar);
    let replayed = Store::open(&log).unwrap();
    assert_eq!(
        keys(&oss.evaluate(&replayed, &hop_then_filter).unwrap()),
        ["svc-api"]
    );
    let _ = std::fs::remove_dir_all(&dir);
}
