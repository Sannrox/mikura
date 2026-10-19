use super::super::*;
use super::helpers::*;

fn component_schema(indexes: &[&str]) -> SchemaDescriptor {
    SchemaDescriptor {
        kind: "component".into(),
        properties: vec!["name".into(), "tier".into()],
        required: vec!["name".into(), "tier".into()],
        indexes: indexes.iter().map(|name| (*name).into()).collect(),
        ..Default::default()
    }
}

fn incident_schema(indexes: &[&str]) -> SchemaDescriptor {
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
        types: vec![
            ("open".into(), PropertyType::Boolean),
            ("priority".into(), PropertyType::Integer),
        ],
        indexes: indexes.iter().map(|name| (*name).into()).collect(),
        ..Default::default()
    }
}

fn fixture() -> Vec<ObjectRecord> {
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

fn seed(store: &mut Store, incident_indexes: &[&str], component_indexes: &[&str]) {
    store
        .append(component_schema(component_indexes).to_record("ns").unwrap())
        .unwrap();
    store
        .append(incident_schema(incident_indexes).to_record("ns").unwrap())
        .unwrap();
    append_all(store, fixture());
}

fn list_incidents(predicate: Predicate) -> EvaluateRequest {
    EvaluateRequest {
        namespace: NS.into(),
        root_kind: "incident".into(),
        hops: vec![],
        sum_kind: "incident".into(),
        sum_property: "priority".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: Some(predicate),
        object_bound: 8,
        sort: None,
        page_size: 0,
        cursor: None,
        breakdown: None,
        then_by: Vec::new(),
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

fn dual_read(name: &str, request: &EvaluateRequest) -> (EvaluateResponse, std::path::PathBuf) {
    let (scan_dir, scan_log) = temp_log(&format!("{name}-scan"));
    let mut scan = Store::create(&scan_log).unwrap();
    seed(&mut scan, &[], &[]);
    let (idx_dir, idx_log) = temp_log(&format!("{name}-idx"));
    let mut indexed = Store::create(&idx_log).unwrap();
    seed(&mut indexed, &["open", "priority"], &["tier"]);
    let oss = ObjectSet::new(LocalCompute);
    let scan_resp = oss.evaluate(&scan, request).unwrap();
    let idx_resp = oss.evaluate(&indexed, request).unwrap();
    assert_eq!(scan_resp, idx_resp);
    drop(scan);
    drop(indexed);
    let _ = std::fs::remove_dir_all(&scan_dir);
    (idx_resp, idx_dir)
}

#[test]
fn indexed_list_and_hop_match_scan_oracle() {
    let open = dual_read("idx-open", &list_incidents(Predicate::eq("open", "true")));
    assert_eq!(keys(&open.0), ["inc-1", "inc-3"]);

    let range = dual_read(
        "idx-range",
        &list_incidents(Predicate::range("priority", Some("2"), Some("3"))),
    );
    assert_eq!(keys(&range.0), ["inc-1", "inc-2"]);

    let composed = dual_read(
        "idx-and",
        &list_incidents(Predicate::and(vec![
            Predicate::eq("open", "true"),
            Predicate::range("priority", Some("2"), Some("3")),
        ])),
    );
    assert_eq!(keys(&composed.0), ["inc-1"]);

    let hop_then_filter = EvaluateRequest {
        namespace: NS.into(),
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
        breakdown: None,
        then_by: Vec::new(),
    };
    let incoming = dual_read("idx-hop-in", &hop_then_filter);
    assert_eq!(keys(&incoming.0), ["svc-api"]);
    assert_eq!(incoming.0.two_hop_count, 2);

    let outgoing = EvaluateRequest {
        namespace: NS.into(),
        root_kind: "component".into(),
        hops: vec![Hop {
            far_kind: "incident".into(),
            join_property: "affects".into(),
            incoming: false,
            predicate: Some(Predicate::range("priority", Some("2"), Some("3"))),
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
        then_by: Vec::new(),
    };
    let far = dual_read("idx-hop-out", &outgoing);
    assert_eq!(keys(&far.0), ["inc-1", "inc-2"]);

    let undeclared = dual_read(
        "idx-name-scan",
        &list_incidents(Predicate::eq("name", "disk full")),
    );
    assert_eq!(keys(&undeclared.0), ["inc-2"]);

    let _ = std::fs::remove_dir_all(&open.1);
    let _ = std::fs::remove_dir_all(&range.1);
    let _ = std::fs::remove_dir_all(&composed.1);
    let _ = std::fs::remove_dir_all(&incoming.1);
    let _ = std::fs::remove_dir_all(&far.1);
    let _ = std::fs::remove_dir_all(&undeclared.1);
}

#[test]
fn indexed_schema_reopens_from_sidecar_and_log() {
    let (dir, log) = temp_log("idx-reopen");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store, &["open", "priority"], &["tier"]);
    let schema = store.schema(NS, "incident").unwrap().unwrap();
    assert_eq!(
        schema.indexes,
        vec!["open".to_string(), "priority".to_string()]
    );
    let request = list_incidents(Predicate::eq("open", "true"));
    let oss = ObjectSet::new(LocalCompute);
    let first = oss.evaluate(&store, &request).unwrap();
    assert_eq!(keys(&first), ["inc-1", "inc-3"]);
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert_eq!(oss.evaluate(&reopened, &request).unwrap(), first);
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    assert_eq!(oss.evaluate(&replayed, &request).unwrap(), first);
    let hop = EvaluateRequest {
        namespace: NS.into(),
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
        breakdown: None,
        then_by: Vec::new(),
    };
    let hopped = oss.evaluate(&replayed, &hop).unwrap();
    assert_eq!(keys(&hopped), ["svc-api"]);
    let _ = std::fs::remove_dir_all(&dir);
}
