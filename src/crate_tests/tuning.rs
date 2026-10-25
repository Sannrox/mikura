use super::super::*;
use super::helpers::*;

fn incident_schema(indexes: &[&str], sums: &[&str]) -> SchemaDescriptor {
    SchemaDescriptor {
        kind: "incident".into(),
        properties: vec![
            "affects".into(),
            "name".into(),
            "open".into(),
            "priority".into(),
            "weight".into(),
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
            ("weight".into(), PropertyType::Integer),
        ],
        indexes: indexes.iter().map(|name| (*name).into()).collect(),
        sums: sums.iter().map(|name| (*name).into()).collect(),
        ..Default::default()
    }
}

fn component_schema() -> SchemaDescriptor {
    SchemaDescriptor {
        kind: "component".into(),
        properties: vec!["name".into(), "tier".into()],
        required: vec!["name".into(), "tier".into()],
        ..Default::default()
    }
}

fn seed(store: &mut Store, n: usize, indexes: &[&str], sums: &[&str]) {
    store
        .append(component_schema().to_record(NS).unwrap())
        .unwrap();
    store
        .append(incident_schema(indexes, sums).to_record(NS).unwrap())
        .unwrap();
    store
        .append(rec(
            "component",
            "svc-api",
            false,
            &[("name", "billing-api"), ("tier", "prod")],
        ))
        .unwrap();
    for i in 0..n {
        let open = if i == 0 { "true" } else { "false" };
        store
            .append(rec(
                "incident",
                &format!("inc-{i}"),
                false,
                &[
                    ("name", "row"),
                    ("affects", "svc-api"),
                    ("open", open),
                    ("priority", "1"),
                    ("weight", "3"),
                ],
            ))
            .unwrap();
    }
}

fn list_open() -> EvaluateRequest {
    EvaluateRequest {
        namespace: NS.into(),
        root_kind: "incident".into(),
        hops: vec![],
        sum_kind: "incident".into(),
        sum_property: "priority".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: Some(Predicate::eq("open", "true")),
        object_bound: 0,
        sort: None,
        page_size: 0,
        cursor: None,
        breakdown: None,
        then_by: Vec::new(),
    }
}

fn list_all() -> EvaluateRequest {
    let mut request = list_open();
    request.predicate = None;
    request
}

fn list_open_and_priority() -> EvaluateRequest {
    let mut request = list_open();
    request.predicate = Some(Predicate::and(vec![
        Predicate::eq("open", "true"),
        Predicate::eq("priority", "1"),
    ]));
    request
}

fn list_open_or_priority() -> EvaluateRequest {
    let mut request = list_open();
    request.predicate = Some(Predicate::or(vec![
        Predicate::eq("open", "true"),
        Predicate::eq("priority", "1"),
    ]));
    request
}

fn paged_priority() -> EvaluateRequest {
    let mut request = list_open();
    request.predicate = Some(Predicate::eq("priority", "1"));
    request.sort = Some(Sort::by("name"));
    request.page_size = 1;
    request.object_bound = 64;
    request
}

fn hop_open_then_tier() -> EvaluateRequest {
    EvaluateRequest {
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
        predicate: Some(Predicate::eq("open", "true")),
        object_bound: 0,
        sort: None,
        page_size: 0,
        cursor: None,
        breakdown: None,
        then_by: Vec::new(),
    }
}

fn hop_weight() -> EvaluateRequest {
    EvaluateRequest {
        namespace: NS.into(),
        root_kind: "component".into(),
        hops: vec![Hop {
            far_kind: "incident".into(),
            join_property: "affects".into(),
            incoming: false,
            predicate: None,
        }],
        sum_kind: "incident".into(),
        sum_property: "weight".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: None,
        object_bound: 0,
        sort: None,
        page_size: 0,
        cursor: None,
        breakdown: None,
        then_by: Vec::new(),
    }
}

fn suggestion<'a>(
    suggestions: &'a [Suggestion],
    kind: SuggestionKind,
    property: &str,
) -> &'a Suggestion {
    suggestions
        .iter()
        .find(|row| row.kind == kind && row.property == property)
        .unwrap_or_else(|| panic!("missing {kind:?} {property} in {suggestions:?}"))
}

#[test]
fn filter_workload_suggests_index_and_apply_cuts_scans() {
    let (dir, log) = temp_log("tune-add-index");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store, 64, &[], &["priority"]);
    let oss = ObjectSet::new(LocalCompute);
    let request = list_open();
    let before = inspect_cost(&store, &request).unwrap();
    assert!(!before.used_index);
    assert_eq!(before.identities_scanned, 64);

    let mut recorder = QueryRecorder::new();
    for _ in 0..3 {
        let response = recorder.record(&store, &oss, &request).unwrap();
        assert_eq!(response.two_hop_count, 1);
    }
    let suggestions = suggest(&store, &recorder).unwrap();
    let add = suggestion(&suggestions, SuggestionKind::AddIndex, "open");
    assert_eq!(add.object_kind, "incident");
    assert_eq!(add.estimated_bytes, 0);

    let change = apply_measured(&mut store, &oss, add, &request).unwrap();
    assert!(!change.rolled_back);
    assert!(change.after.used_index);
    assert!(change.after.identities_scanned < change.before.identities_scanned);
    assert_eq!(change.after.identities_scanned, 1);
    let schema = store.schema(NS, "incident").unwrap().unwrap();
    assert!(schema.indexes.iter().any(|name| name == "open"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unused_index_is_suggested_for_removal() {
    let (dir, log) = temp_log("tune-drop-index");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store, 8, &["open", "priority"], &["priority"]);
    let oss = ObjectSet::new(LocalCompute);
    let mut recorder = QueryRecorder::new();
    recorder.record(&store, &oss, &list_open()).unwrap();
    let suggestions = suggest(&store, &recorder).unwrap();
    let drop = suggestion(&suggestions, SuggestionKind::DropIndex, "priority");
    assert_eq!(drop.object_kind, "incident");
    assert!(suggestions
        .iter()
        .all(|row| !(row.kind == SuggestionKind::DropIndex && row.property == "open")));
    apply(&mut store, drop).unwrap();
    let schema = store.schema(NS, "incident").unwrap().unwrap();
    assert_eq!(schema.indexes, vec!["open".to_string()]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn auto_apply_stays_inside_memory_budget() {
    let (dir, log) = temp_log("tune-budget");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store, 8, &[], &["priority"]);
    let oss = ObjectSet::new(LocalCompute);
    let mut recorder = QueryRecorder::new();
    recorder.record(&store, &oss, &list_open()).unwrap();
    recorder.record(&store, &oss, &hop_weight()).unwrap();
    let suggestions = suggest(&store, &recorder).unwrap();
    let cache = suggestion(&suggestions, SuggestionKind::CacheMeasure, "weight");
    assert!(cache.estimated_bytes > 0);
    let add = suggestion(&suggestions, SuggestionKind::AddIndex, "open");
    assert_eq!(add.estimated_bytes, 0);
    assert!(suggestions
        .iter()
        .any(|row| row.kind == SuggestionKind::DenormalizeLink));

    let budget = MemoryBudget {
        max_bytes: cache.estimated_bytes.saturating_sub(1),
    };
    let applied = auto_apply(&mut store, &oss, &recorder, &suggestions, budget).unwrap();
    let extra: u64 = applied
        .iter()
        .filter(|change| {
            !change.rolled_back
                && matches!(
                    change.suggestion.kind,
                    SuggestionKind::AddIndex | SuggestionKind::CacheMeasure
                )
        })
        .map(|change| change.suggestion.estimated_bytes)
        .sum();
    assert!(extra <= budget.max_bytes, "{applied:?}");
    assert!(applied.iter().any(|change| {
        change.suggestion.kind == SuggestionKind::AddIndex && !change.rolled_back
    }));
    assert!(applied.iter().all(|change| {
        change.suggestion.kind != SuggestionKind::CacheMeasure || change.rolled_back
    }));
    assert!(applied
        .iter()
        .all(|change| change.suggestion.kind != SuggestionKind::DenormalizeLink));
    let denorm = suggestions
        .iter()
        .find(|row| row.kind == SuggestionKind::DenormalizeLink)
        .unwrap();
    let err = apply(&mut store, denorm).unwrap_err();
    assert!(err.contains("clerk applies"), "{err}");
    let schema = store.schema(NS, "incident").unwrap().unwrap();
    assert!(schema.indexes.iter().any(|name| name == "open"));
    assert!(!schema.sums.iter().any(|name| name == "weight"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cache_measure_apply_declares_sum() {
    let (dir, log) = temp_log("tune-cache-sum");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store, 8, &[], &["priority"]);
    let oss = ObjectSet::new(LocalCompute);
    let mut recorder = QueryRecorder::new();
    recorder.record(&store, &oss, &hop_weight()).unwrap();
    let suggestions = suggest(&store, &recorder).unwrap();
    let cache = suggestion(&suggestions, SuggestionKind::CacheMeasure, "weight");
    let change = apply_measured(&mut store, &oss, cache, &hop_weight()).unwrap();
    assert!(!change.rolled_back);
    assert!(change.after.measure_cached);
    let schema = store.schema(NS, "incident").unwrap().unwrap();
    assert!(schema.sums.iter().any(|name| name == "weight"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn and_predicate_keeps_both_indexes() {
    let (dir, log) = temp_log("tune-and-index");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store, 16, &[], &["priority"]);
    let oss = ObjectSet::new(LocalCompute);
    let mut recorder = QueryRecorder::new();
    recorder
        .record(&store, &oss, &list_open_and_priority())
        .unwrap();
    let suggestions = suggest(&store, &recorder).unwrap();
    suggestion(&suggestions, SuggestionKind::AddIndex, "open");
    suggestion(&suggestions, SuggestionKind::AddIndex, "priority");
    let applied = auto_apply(
        &mut store,
        &oss,
        &recorder,
        &suggestions,
        MemoryBudget { max_bytes: 1024 },
    )
    .unwrap();
    assert!(applied
        .iter()
        .all(|change| !change.rolled_back || change.suggestion.kind != SuggestionKind::AddIndex));
    let schema = store.schema(NS, "incident").unwrap().unwrap();
    assert!(schema.indexes.iter().any(|name| name == "open"));
    assert!(schema.indexes.iter().any(|name| name == "priority"));
    let cost = inspect_cost(&store, &list_open_and_priority()).unwrap();
    assert!(cost.used_index);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn indexed_root_still_suggests_unindexed_hop() {
    let (dir, log) = temp_log("tune-hop-index");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store, 8, &["open"], &["priority"]);
    let oss = ObjectSet::new(LocalCompute);
    let mut recorder = QueryRecorder::new();
    recorder
        .record(&store, &oss, &hop_open_then_tier())
        .unwrap();
    let suggestions = suggest(&store, &recorder).unwrap();
    let add = suggestion(&suggestions, SuggestionKind::AddIndex, "tier");
    assert_eq!(add.object_kind, "component");
    assert!(suggestions
        .iter()
        .all(|row| !(row.kind == SuggestionKind::AddIndex && row.property == "open")));
    assert!(suggestions
        .iter()
        .all(|row| row.kind != SuggestionKind::CacheMeasure));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn zero_hop_sum_does_not_suggest_measure_cache() {
    let (dir, log) = temp_log("tune-zero-hop-sum");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store, 8, &[], &[]);
    let oss = ObjectSet::new(LocalCompute);
    let mut recorder = QueryRecorder::new();
    recorder.record(&store, &oss, &list_open()).unwrap();
    let suggestions = suggest(&store, &recorder).unwrap();
    assert!(suggestions
        .iter()
        .all(|row| row.kind != SuggestionKind::CacheMeasure));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn overlapping_or_indexes_do_not_rollback() {
    let (dir, log) = temp_log("tune-or-index");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store, 16, &[], &["priority"]);
    let oss = ObjectSet::new(LocalCompute);
    let mut recorder = QueryRecorder::new();
    recorder
        .record(&store, &oss, &list_open_or_priority())
        .unwrap();
    let suggestions = suggest(&store, &recorder).unwrap();
    suggestion(&suggestions, SuggestionKind::AddIndex, "open");
    suggestion(&suggestions, SuggestionKind::AddIndex, "priority");
    let applied = auto_apply(
        &mut store,
        &oss,
        &recorder,
        &suggestions,
        MemoryBudget { max_bytes: 1024 },
    )
    .unwrap();
    assert!(applied.iter().all(|change| {
        change.suggestion.kind != SuggestionKind::AddIndex || !change.rolled_back
    }));
    let schema = store.schema(NS, "incident").unwrap().unwrap();
    assert!(schema.indexes.iter().any(|name| name == "open"));
    assert!(schema.indexes.iter().any(|name| name == "priority"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paged_probe_strips_cursor_before_apply() {
    let (dir, log) = temp_log("tune-paged-cursor");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store, 8, &[], &["priority"]);
    let oss = ObjectSet::new(LocalCompute);
    let first = paged_priority();
    let page = oss.evaluate(&store, &first).unwrap();
    let cursor = page.cursor.expect("continuation");
    let mut next = first.clone();
    next.cursor = Some(cursor);
    let mut recorder = QueryRecorder::new();
    recorder.record(&store, &oss, &next).unwrap();
    let suggestions = suggest(&store, &recorder).unwrap();
    let add = suggestion(&suggestions, SuggestionKind::AddIndex, "priority");
    let change = apply_measured(&mut store, &oss, add, &next).unwrap();
    assert!(!change.rolled_back);
    assert!(change.after.used_index);
    let schema = store.schema(NS, "incident").unwrap().unwrap();
    assert!(schema.indexes.iter().any(|name| name == "priority"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn add_index_probes_the_filtered_query() {
    let (dir, log) = temp_log("tune-index-probe");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store, 16, &[], &["priority"]);
    let oss = ObjectSet::new(LocalCompute);
    let mut recorder = QueryRecorder::new();
    recorder.record(&store, &oss, &list_all()).unwrap();
    recorder.record(&store, &oss, &list_open()).unwrap();
    let suggestions = suggest(&store, &recorder).unwrap();
    let add = suggestion(&suggestions, SuggestionKind::AddIndex, "open");
    let applied = auto_apply(
        &mut store,
        &oss,
        &recorder,
        std::slice::from_ref(add),
        MemoryBudget { max_bytes: 0 },
    )
    .unwrap();
    assert_eq!(applied.len(), 1);
    assert!(!applied[0].rolled_back);
    assert!(applied[0].after.used_index);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unused_measure_is_auto_dropped() {
    let (dir, log) = temp_log("tune-drop-sum");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store, 8, &["open"], &["priority", "weight"]);
    let oss = ObjectSet::new(LocalCompute);
    let mut recorder = QueryRecorder::new();
    recorder.record(&store, &oss, &list_open()).unwrap();
    let suggestions = suggest(&store, &recorder).unwrap();
    suggestion(&suggestions, SuggestionKind::DropMeasure, "weight");
    let applied = auto_apply(
        &mut store,
        &oss,
        &recorder,
        &suggestions,
        MemoryBudget { max_bytes: 0 },
    )
    .unwrap();
    assert!(applied.iter().any(|change| {
        change.suggestion.kind == SuggestionKind::DropMeasure
            && change.suggestion.property == "weight"
            && !change.rolled_back
    }));
    let schema = store.schema(NS, "incident").unwrap().unwrap();
    assert!(!schema.sums.iter().any(|name| name == "weight"));
    assert!(schema.sums.iter().any(|name| name == "priority"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_predicate_does_not_suggest_index() {
    let (dir, log) = temp_log("tune-missing-pred");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store, 8, &[], &["priority"]);
    let oss = ObjectSet::new(LocalCompute);
    let mut request = list_open();
    request.predicate = Some(Predicate::missing("open"));
    let mut recorder = QueryRecorder::new();
    recorder.record(&store, &oss, &request).unwrap();
    let suggestions = suggest(&store, &recorder).unwrap();
    assert!(suggestions
        .iter()
        .all(|row| !(row.kind == SuggestionKind::AddIndex && row.property == "open")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn boolean_sum_does_not_suggest_measure_cache() {
    let (dir, log) = temp_log("tune-bool-sum");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store, 8, &[], &["priority"]);
    let oss = ObjectSet::new(LocalCompute);
    let mut request = hop_weight();
    request.sum_property = "open".into();
    let mut recorder = QueryRecorder::new();
    recorder.record(&store, &oss, &request).unwrap();
    let suggestions = suggest(&store, &recorder).unwrap();
    assert!(suggestions
        .iter()
        .all(|row| { !(row.kind == SuggestionKind::CacheMeasure && row.property == "open") }));
    let _ = std::fs::remove_dir_all(&dir);
}
