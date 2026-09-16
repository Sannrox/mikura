use super::*;
use mikura::{
    Action, Aggregate, ComputeError, EvaluateRequest, Hop, LocalCompute, ObjectSet, PropertyAcl,
    Store,
};
use std::collections::HashMap;

fn rec(kind: &str, key: &str, hidden: bool, props: &[(&str, &str)]) -> ObjectRecord {
    ObjectRecord {
        gen: 1,
        kind: kind.into(),
        key: key.into(),
        hidden,
        props: props
            .iter()
            .map(|(name, value)| ((*name).into(), (*value).into()))
            .collect(),
    }
}

fn fixture() -> Vec<ObjectRecord> {
    vec![
        rec("Customer", "c0", true, &[("region", "eu")]),
        rec("Customer", "c1", false, &[("region", "us")]),
        rec("Order", "o1", false, &[("customer_id", "c1")]),
        rec("Order", "o0", true, &[("customer_id", "c0")]),
        rec(
            "Shipment",
            "s1",
            false,
            &[("order_id", "o1"), ("amount", "10")],
        ),
        rec(
            "Shipment",
            "s0",
            true,
            &[("order_id", "o0"), ("amount", "99")],
        ),
    ]
}

fn fixture_request() -> EvaluateRequest {
    EvaluateRequest {
        root_kind: "Customer".into(),
        hops: vec![
            Hop {
                far_kind: "Order".into(),
                join_property: "customer_id".into(),
            },
            Hop {
                far_kind: "Shipment".into(),
                join_property: "order_id".into(),
            },
        ],
        sum_kind: "Shipment".into(),
        sum_property: "amount".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
    }
}

fn temp_log(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("mikura-ingest-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("objects.mikura");
    (dir, log)
}

#[test]
fn batch_ingest_evaluate_rebuild_matches_live() {
    let (dir, log) = temp_log("batch-eval");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let visible = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(visible.two_hop_count, 1);
    assert_eq!(visible.sum_amount, 10);

    store
        .apply_action(Action {
            kind: "Shipment".into(),
            key: "s2".into(),
            props: HashMap::from([
                ("order_id".into(), "o1".into()),
                ("amount".into(), "5".into()),
            ]),
        })
        .unwrap();
    let after = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(after.sum_amount, 15);

    let reopened = Store::open(&log).unwrap();
    let from_log = oss.evaluate(&reopened, &fixture_request()).unwrap();
    assert_eq!(from_log.two_hop_count, after.two_hop_count);
    assert_eq!(from_log.sum_amount, after.sum_amount);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_flush_appends_pending_records() {
    let (dir, log) = temp_log("stream");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream
        .push(
            &mut store,
            rec(
                "Shipment",
                "s3",
                false,
                &[("order_id", "o1"), ("amount", "7")],
            ),
        )
        .unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let live = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(live.sum_amount, 17);
    stream.flush(&mut store).unwrap();
    let streamed = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(streamed.sum_amount, 17);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_bound_n_plus_one_fails_closed_without_drop() {
    let (dir, log) = temp_log("stream-bound");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let mut stream = StreamIngest::new(2).unwrap();
    stream
        .push(
            &mut store,
            rec(
                "Shipment",
                "s3",
                false,
                &[("order_id", "o1"), ("amount", "1")],
            ),
        )
        .unwrap();
    stream
        .push(
            &mut store,
            rec(
                "Shipment",
                "s4",
                false,
                &[("order_id", "o1"), ("amount", "2")],
            ),
        )
        .unwrap();
    let overflow = rec(
        "Shipment",
        "s5",
        false,
        &[("order_id", "o1"), ("amount", "99")],
    );
    let err = stream.push(&mut store, overflow).unwrap_err();
    assert!(
        err.contains("bound 2 exceeded"),
        "expected fail-closed bound error, got {err}"
    );
    let oss = ObjectSet::new(LocalCompute);
    let live = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(live.sum_amount, 13);
    stream.flush(&mut store).unwrap();
    let reopened = Store::open(&log).unwrap();
    let committed = oss.evaluate(&reopened, &fixture_request()).unwrap();
    assert_eq!(committed.sum_amount, 13);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_unflushed_records_are_not_on_rebuild() {
    let (dir, log) = temp_log("stream-unflushed");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let mut stream = StreamIngest::new(4).unwrap();
    stream
        .push(
            &mut store,
            rec(
                "Shipment",
                "s3",
                false,
                &[("order_id", "o1"), ("amount", "7")],
            ),
        )
        .unwrap();
    drop(store);
    let reopened = Store::open(&log).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let from_log = oss.evaluate(&reopened, &fixture_request()).unwrap();
    assert_eq!(from_log.sum_amount, 10);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_flush_survives_reopen() {
    let (dir, log) = temp_log("stream-flush-reopen");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let mut stream = StreamIngest::new(4).unwrap();
    stream
        .push(
            &mut store,
            rec(
                "Shipment",
                "s3",
                false,
                &[("order_id", "o1"), ("amount", "7")],
            ),
        )
        .unwrap();
    stream.flush(&mut store).unwrap();
    drop(store);
    let reopened = Store::open(&log).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let from_log = oss.evaluate(&reopened, &fixture_request()).unwrap();
    assert_eq!(from_log.sum_amount, 17);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_hidden_records_excluded_and_acl_fails_closed() {
    let (dir, log) = temp_log("stream-hidden-acl");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let mut stream = StreamIngest::new(4).unwrap();
    stream
        .push(
            &mut store,
            rec(
                "Shipment",
                "s_hidden",
                true,
                &[("order_id", "o1"), ("amount", "50")],
            ),
        )
        .unwrap();
    stream.flush(&mut store).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let visible = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(visible.sum_amount, 10);
    let mut denied = fixture_request();
    denied.acl = PropertyAcl::deny_property("Shipment", "amount");
    let err = oss.evaluate(&store, &denied).unwrap_err();
    assert!(matches!(
        err,
        ComputeError::Acl(mikura::AclError::Denied { .. })
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

fn page_filling_records(n: usize) -> Vec<ObjectRecord> {
    (0..n)
        .map(|i| {
            rec(
                "Item",
                &format!("k{i:04}"),
                false,
                &[("payload", "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx")],
            )
        })
        .collect()
}

#[test]
fn batch_ingest_group_commits_fewer_fsyncs_than_one_per_record() {
    let (dir, log) = temp_log("group-commit-batch");
    let records = page_filling_records(400);
    let mut batched = Store::create(&log).unwrap();
    let before_batch = batched.log_fsync_count();
    BatchIngest::run(&mut batched, records.clone()).unwrap();
    let batch_fsyncs = batched.log_fsync_count() - before_batch;
    drop(batched);

    let one_dir = dir.join("one");
    std::fs::create_dir_all(&one_dir).unwrap();
    let one_log = one_dir.join("objects.mikura");
    let mut one = Store::create(&one_log).unwrap();
    let before_one = one.log_fsync_count();
    for record in records {
        one.append(record).unwrap();
    }
    let one_fsyncs = one.log_fsync_count() - before_one;
    assert!(
        batch_fsyncs < one_fsyncs,
        "batch fsyncs {batch_fsyncs} should be fewer than one-per-record {one_fsyncs}"
    );
    assert!(
        batch_fsyncs < 16,
        "400 records under Group(32) should not fsync per record, got {batch_fsyncs}"
    );

    let oss = ObjectSet::new(LocalCompute);
    let req = EvaluateRequest {
        root_kind: "Item".into(),
        hops: vec![],
        sum_kind: "Item".into(),
        sum_property: "n".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
    };
    let live = oss.evaluate(&one, &req).unwrap();
    assert_eq!(live.two_hop_count, 400);
    let reopened = Store::open(&log).unwrap();
    let from_batch = oss.evaluate(&reopened, &req).unwrap();
    assert_eq!(from_batch.two_hop_count, 400);
    let _ = std::fs::remove_dir_all(&dir);
}

fn shipment_request() -> EvaluateRequest {
    EvaluateRequest {
        root_kind: "Shipment".into(),
        hops: vec![],
        sum_kind: "Shipment".into(),
        sum_property: "amount".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
    }
}

fn assert_same_identity(live: &Store, reopened: &Store, records: &[ObjectRecord]) {
    for record in records {
        assert_eq!(
            live.joins().is_visible(&record.kind, &record.key),
            !record.hidden
        );
        assert_eq!(
            reopened.joins().is_visible(&record.kind, &record.key),
            !record.hidden
        );
        if !record.hidden {
            for (name, value) in &record.props {
                assert_eq!(
                    live.joins().prop(&record.kind, &record.key, name),
                    Some(value.as_str())
                );
                assert_eq!(
                    reopened.joins().prop(&record.kind, &record.key, name),
                    Some(value.as_str())
                );
            }
        }
    }
}

#[test]
fn merge_source_only_keeps_source_records() {
    let source = vec![
        rec("Shipment", "s1", false, &[("amount", "10")]),
        rec("Shipment", "s2", true, &[("amount", "99")]),
    ];
    let merged = merge_source_and_edits(source.clone(), Vec::new());
    assert_eq!(merged, source);
}

#[test]
fn merge_edits_only_keeps_edit_records() {
    let edits = vec![rec("Shipment", "s1", false, &[("amount", "5")])];
    let merged = merge_source_and_edits(Vec::new(), edits.clone());
    assert_eq!(merged, edits);
}

#[test]
fn merge_same_key_edit_wins() {
    let source = vec![rec(
        "Shipment",
        "s1",
        false,
        &[("order_id", "o1"), ("amount", "10")],
    )];
    let edits = vec![rec(
        "Shipment",
        "s1",
        false,
        &[("order_id", "o1"), ("amount", "5")],
    )];
    let merged = merge_source_and_edits(source, edits);
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].props.get("amount").map(String::as_str), Some("5"));
    assert!(!merged[0].hidden);
}

#[test]
fn merge_hidden_source_visible_edit_unhides() {
    let source = vec![rec("Shipment", "s1", true, &[("amount", "10")])];
    let edits = vec![rec("Shipment", "s1", false, &[("amount", "7")])];
    let merged = merge_source_and_edits(source, edits);
    assert_eq!(merged.len(), 1);
    assert!(!merged[0].hidden);
    assert_eq!(merged[0].props.get("amount").map(String::as_str), Some("7"));
}

#[test]
fn merge_visible_source_hidden_edit_hides() {
    let source = vec![rec("Shipment", "s1", false, &[("amount", "10")])];
    let edits = vec![rec("Shipment", "s1", true, &[("amount", "10")])];
    let merged = merge_source_and_edits(source, edits);
    assert_eq!(merged.len(), 1);
    assert!(merged[0].hidden);
}

#[test]
fn merge_then_append_matches_sequential_merged_set() {
    let source = vec![
        rec(
            "Shipment",
            "s1",
            false,
            &[("order_id", "o1"), ("amount", "10")],
        ),
        rec(
            "Shipment",
            "s2",
            false,
            &[("order_id", "o1"), ("amount", "3")],
        ),
        rec(
            "Shipment",
            "s3",
            true,
            &[("order_id", "o1"), ("amount", "99")],
        ),
    ];
    let edits = vec![
        rec(
            "Shipment",
            "s1",
            false,
            &[("order_id", "o1"), ("amount", "5")],
        ),
        rec(
            "Shipment",
            "s3",
            false,
            &[("order_id", "o1"), ("amount", "4")],
        ),
        rec(
            "Shipment",
            "s4",
            true,
            &[("order_id", "o1"), ("amount", "8")],
        ),
    ];
    let merged = merge_source_and_edits(source.clone(), edits.clone());

    let (dir, log_merge) = temp_log("merge-then-append");
    let mut merged_store = Store::create(&log_merge).unwrap();
    MergeIngest::run(&mut merged_store, source, edits).unwrap();

    let log_seq = dir.join("sequential.mikura");
    let mut sequential = Store::create(&log_seq).unwrap();
    for record in merged {
        sequential.append(record).unwrap();
    }

    let oss = ObjectSet::new(LocalCompute);
    let req = shipment_request();
    let from_merge = oss.evaluate(&merged_store, &req).unwrap();
    let from_seq = oss.evaluate(&sequential, &req).unwrap();
    assert_eq!(from_merge.two_hop_count, from_seq.two_hop_count);
    assert_eq!(from_merge.sum_amount, from_seq.sum_amount);
    assert_eq!(from_merge.two_hop_count, 3);
    assert_eq!(from_merge.sum_amount, 12);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn merge_append_reopen_matches_live_identity() {
    let source = vec![
        rec("Customer", "c1", false, &[("region", "us")]),
        rec(
            "Shipment",
            "s1",
            false,
            &[("order_id", "o1"), ("amount", "10")],
        ),
        rec(
            "Shipment",
            "s_hidden",
            true,
            &[("order_id", "o1"), ("amount", "99")],
        ),
    ];
    let edits = vec![
        rec(
            "Shipment",
            "s1",
            false,
            &[("order_id", "o1"), ("amount", "5")],
        ),
        rec(
            "Shipment",
            "s_hidden",
            false,
            &[("order_id", "o1"), ("amount", "4")],
        ),
        rec(
            "Shipment",
            "s_edit_only",
            true,
            &[("order_id", "o1"), ("amount", "8")],
        ),
    ];
    let merged = merge_source_and_edits(source.clone(), edits.clone());

    let (dir, log) = temp_log("merge-reopen");
    let mut store = Store::create(&log).unwrap();
    MergeIngest::run(&mut store, source, edits).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let live = oss.evaluate(&store, &shipment_request()).unwrap();
    assert_eq!(live.two_hop_count, 2);
    assert_eq!(live.sum_amount, 9);

    let reopened = Store::open(&log).unwrap();
    let from_log = oss.evaluate(&reopened, &shipment_request()).unwrap();
    assert_eq!(from_log.two_hop_count, live.two_hop_count);
    assert_eq!(from_log.sum_amount, live.sum_amount);
    assert_same_identity(&store, &reopened, &merged);
    assert!(!reopened.joins().is_visible("Shipment", "s_edit_only"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn changelog_identical_snapshots_emit_nothing() {
    let snapshot = vec![
        rec("Shipment", "s1", false, &[("amount", "10")]),
        rec("Shipment", "s2", true, &[("amount", "99")]),
    ];
    assert!(snapshot_changelog(snapshot.clone(), snapshot).is_empty());
}

#[test]
fn changelog_new_key_emits_visible_record() {
    let records = snapshot_changelog(
        Vec::new(),
        vec![rec("Shipment", "s1", false, &[("amount", "10")])],
    );
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].key, "s1");
    assert!(!records[0].hidden);
    assert_eq!(
        records[0].props.get("amount").map(String::as_str),
        Some("10")
    );
}

#[test]
fn changelog_deleted_key_emits_hide() {
    let records = snapshot_changelog(
        vec![rec("Shipment", "s1", false, &[("amount", "10")])],
        Vec::new(),
    );
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].key, "s1");
    assert!(records[0].hidden);
    assert_eq!(
        records[0].props.get("amount").map(String::as_str),
        Some("10")
    );
}

#[test]
fn changelog_changed_prop_emits_current_payload() {
    let records = snapshot_changelog(
        vec![rec("Shipment", "s1", false, &[("amount", "10")])],
        vec![rec("Shipment", "s1", false, &[("amount", "5")])],
    );
    assert_eq!(records.len(), 1);
    assert!(!records[0].hidden);
    assert_eq!(
        records[0].props.get("amount").map(String::as_str),
        Some("5")
    );
}

#[test]
fn changelog_into_merge_matches_new_snapshot_plus_edits() {
    let previous = vec![
        rec(
            "Shipment",
            "s1",
            false,
            &[("order_id", "o1"), ("amount", "10")],
        ),
        rec(
            "Shipment",
            "s2",
            false,
            &[("order_id", "o1"), ("amount", "3")],
        ),
    ];
    let current = vec![
        rec(
            "Shipment",
            "s1",
            false,
            &[("order_id", "o1"), ("amount", "8")],
        ),
        rec(
            "Shipment",
            "s3",
            false,
            &[("order_id", "o1"), ("amount", "4")],
        ),
    ];
    let edits = vec![rec(
        "Shipment",
        "s3",
        false,
        &[("order_id", "o1"), ("amount", "7")],
    )];
    let changelog = snapshot_changelog(previous.clone(), current.clone());
    assert_eq!(changelog.len(), 3);

    let (dir, log) = temp_log("changelog-merge");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, previous).unwrap();
    MergeIngest::run(&mut store, changelog, edits.clone()).unwrap();

    let expected = merge_source_and_edits(current, edits);
    let oss = ObjectSet::new(LocalCompute);
    let live = oss.evaluate(&store, &shipment_request()).unwrap();
    assert_eq!(live.two_hop_count, 2);
    assert_eq!(live.sum_amount, 15);
    let reopened = Store::open(&log).unwrap();
    let from_log = oss.evaluate(&reopened, &shipment_request()).unwrap();
    assert_eq!(from_log.two_hop_count, live.two_hop_count);
    assert_eq!(from_log.sum_amount, live.sum_amount);
    assert_same_identity(&store, &reopened, &expected);
    assert!(!reopened.joins().is_visible("Shipment", "s2"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn changelog_empty_diff_does_not_append() {
    let snapshot = vec![rec("Shipment", "s1", false, &[("amount", "10")])];
    let (dir, log) = temp_log("changelog-empty");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, snapshot.clone()).unwrap();
    ChangelogIngest::run(&mut store, snapshot.clone(), snapshot).unwrap();
    assert!(store.joins().is_visible("Shipment", "s1"));
    let reopened = Store::open(&log).unwrap();
    assert!(reopened.joins().is_visible("Shipment", "s1"));
    let _ = std::fs::remove_dir_all(&dir);
}
