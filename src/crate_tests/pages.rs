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

fn seed(store: &mut Store) {
    store
        .append(component_schema().to_record().unwrap())
        .unwrap();
    store
        .append(incident_schema().to_record().unwrap())
        .unwrap();
    append_all(
        store,
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
        ],
    );
}

fn page_incidents(sort: Sort, page_size: usize, cursor: Option<String>) -> EvaluateRequest {
    EvaluateRequest {
        root_kind: "incident".into(),
        hops: vec![],
        sum_kind: "incident".into(),
        sum_property: "priority".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: None,
        object_bound: 8,
        sort: Some(sort),
        page_size,
        cursor,
    }
}

fn keys(response: &EvaluateResponse) -> Vec<&str> {
    response
        .objects
        .iter()
        .map(|row| row.key.as_str())
        .collect()
}

#[test]
fn ordered_pages_bind_snapshot_and_fail_closed() {
    let (dir, log) = temp_log("pages");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);
    let oss = ObjectSet::new(LocalCompute);

    let ordered = oss
        .evaluate(&store, &page_incidents(Sort::by("priority"), 0, None))
        .unwrap();
    assert_eq!(keys(&ordered), ["inc-3", "inc-1", "inc-2"]);
    assert_eq!(ordered.two_hop_count, 3);
    assert!(ordered.cursor.is_none());

    let ties = oss
        .evaluate(&store, &page_incidents(Sort::by("open"), 0, None))
        .unwrap();
    assert_eq!(keys(&ties), ["inc-2", "inc-1", "inc-3"]);

    let missing_last = oss
        .evaluate(&store, &page_incidents(Sort::by("note"), 0, None))
        .unwrap();
    assert_eq!(keys(&missing_last), ["inc-2", "inc-1", "inc-3"]);

    let desc = Sort {
        property: "priority".into(),
        descending: true,
    };
    let descending = oss
        .evaluate(&store, &page_incidents(desc, 0, None))
        .unwrap();
    assert_eq!(keys(&descending), ["inc-2", "inc-1", "inc-3"]);

    let note_desc = Sort {
        property: "note".into(),
        descending: true,
    };
    let missing_last_desc = oss
        .evaluate(&store, &page_incidents(note_desc, 0, None))
        .unwrap();
    assert_eq!(keys(&missing_last_desc), ["inc-2", "inc-1", "inc-3"]);

    let page1 = oss
        .evaluate(&store, &page_incidents(Sort::by("priority"), 1, None))
        .unwrap();
    assert_eq!(keys(&page1), ["inc-3"]);
    assert_eq!(page1.two_hop_count, 3);
    let cursor1 = page1.cursor.clone().expect("page one continues");

    let page2 = oss
        .evaluate(
            &store,
            &page_incidents(Sort::by("priority"), 1, Some(cursor1.clone())),
        )
        .unwrap();
    assert_eq!(keys(&page2), ["inc-1"]);
    let cursor2 = page2.cursor.clone().expect("page two continues");

    let page3 = oss
        .evaluate(
            &store,
            &page_incidents(Sort::by("priority"), 1, Some(cursor2.clone())),
        )
        .unwrap();
    assert_eq!(keys(&page3), ["inc-2"]);
    assert!(page3.cursor.is_none());

    let empty = oss
        .evaluate(&store, &page_incidents(Sort::by("priority"), 8, None))
        .unwrap();
    assert_eq!(keys(&empty), ["inc-3", "inc-1", "inc-2"]);
    assert!(empty.cursor.is_none());

    let overflow = oss.evaluate(
        &store,
        &EvaluateRequest {
            object_bound: 1,
            ..page_incidents(Sort::by("priority"), 1, None)
        },
    );
    assert!(
        matches!(
            overflow,
            Err(ComputeError::ObjectBound { bound: 1, count: 3 })
        ),
        "{overflow:?}"
    );

    let denied = EvaluateRequest {
        acl: PropertyAcl::deny_property("incident", "priority"),
        ..page_incidents(Sort::by("priority"), 1, None)
    };
    assert!(matches!(
        oss.evaluate(&store, &denied),
        Err(ComputeError::Acl(AclError::Denied { .. }))
    ));

    let no_sort = oss.evaluate(
        &store,
        &EvaluateRequest {
            sort: None,
            page_size: 1,
            ..page_incidents(Sort::by("priority"), 1, None)
        },
    );
    assert!(matches!(no_sort, Err(ComputeError::Page(_))), "{no_sort:?}");

    let bad_token = oss.evaluate(
        &store,
        &page_incidents(Sort::by("priority"), 1, Some("x".into())),
    );
    assert!(
        matches!(bad_token, Err(ComputeError::Page(_))),
        "{bad_token:?}"
    );

    let other_query = oss.evaluate(
        &store,
        &EvaluateRequest {
            predicate: Some(Predicate::eq("open", "true")),
            ..page_incidents(Sort::by("priority"), 1, Some(cursor1.clone()))
        },
    );
    assert!(
        matches!(other_query, Err(ComputeError::Page(_))),
        "{other_query:?}"
    );

    let other_acl = oss.evaluate(
        &store,
        &EvaluateRequest {
            acl: PropertyAcl::deny_property("incident", "name"),
            ..page_incidents(Sort::by("priority"), 1, Some(cursor1.clone()))
        },
    );
    assert!(
        matches!(other_acl, Err(ComputeError::Page(_))),
        "{other_acl:?}"
    );

    store
        .append_uncommitted(rec(
            "incident",
            "inc-stream",
            false,
            &[
                ("name", "streamed"),
                ("affects", "svc-web"),
                ("open", "true"),
                ("priority", "4"),
            ],
        ))
        .unwrap();
    let after_uncommitted = oss.evaluate(
        &store,
        &page_incidents(Sort::by("priority"), 1, Some(cursor1.clone())),
    );
    assert!(
        matches!(after_uncommitted, Err(ComputeError::Page(_))),
        "{after_uncommitted:?}"
    );
    store.commit().unwrap();

    store
        .append(rec(
            "incident",
            "inc-4",
            false,
            &[
                ("name", "new"),
                ("affects", "svc-web"),
                ("open", "true"),
                ("priority", "0"),
            ],
        ))
        .unwrap();
    let after_write = oss.evaluate(
        &store,
        &page_incidents(Sort::by("priority"), 1, Some(cursor1.clone())),
    );
    assert!(
        matches!(after_write, Err(ComputeError::Page(_))),
        "{after_write:?}"
    );

    let fresh = oss
        .evaluate(&store, &page_incidents(Sort::by("priority"), 1, None))
        .unwrap();
    assert_eq!(keys(&fresh), ["inc-4"]);
    let fresh_cursor = fresh.cursor.clone().expect("fresh page continues");
    drop(store);

    let reopened = Store::open(&log).unwrap();
    let continued = oss
        .evaluate(
            &reopened,
            &page_incidents(Sort::by("priority"), 1, Some(fresh_cursor.clone())),
        )
        .unwrap();
    assert_eq!(keys(&continued), ["inc-3"]);
    let sidecar = log.with_extension("mikura.joins");
    let _ = std::fs::remove_file(&sidecar);
    let replayed = Store::open(&log).unwrap();
    let rebuilt = oss
        .evaluate(
            &replayed,
            &page_incidents(Sort::by("priority"), 1, Some(fresh_cursor)),
        )
        .unwrap();
    assert_eq!(keys(&rebuilt), ["inc-3"]);
    let _ = std::fs::remove_dir_all(&dir);
}
