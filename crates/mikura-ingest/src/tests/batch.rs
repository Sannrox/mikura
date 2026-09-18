use super::*;
use crate::BatchIngest;
use mikura::{Action, LocalCompute};
use std::collections::HashMap;

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
            id: "act-s2".into(),
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
        filter: None,
        object_bound: 0,
    };
    let live = oss.evaluate(&one, &req).unwrap();
    assert_eq!(live.two_hop_count, 400);
    let reopened = Store::open(&log).unwrap();
    let from_batch = oss.evaluate(&reopened, &req).unwrap();
    assert_eq!(from_batch.two_hop_count, 400);
    let _ = std::fs::remove_dir_all(&dir);
}
