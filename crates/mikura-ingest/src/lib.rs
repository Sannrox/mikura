//! Write orchestrator for [`mikura::Store`].
//!
//! A control plane maps datasets and admitted edits to [`ObjectRecord`]s.
//! This crate appends them. It does not know tenants, policy, or datasets.

use mikura::{ObjectRecord, Store};

pub struct BatchIngest;

impl BatchIngest {
    pub fn run(store: &mut Store, records: Vec<ObjectRecord>) -> Result<(), String> {
        for record in records {
            store.append_uncommitted(record)?;
        }
        store.commit()
    }
}

pub struct StreamIngest {
    pending: Vec<ObjectRecord>,
}

impl StreamIngest {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    pub fn push(&mut self, record: ObjectRecord) -> Result<(), String> {
        self.pending.push(record);
        Ok(())
    }

    pub fn flush_into(&mut self, store: &mut Store) -> Result<(), String> {
        let records = std::mem::take(&mut self.pending);
        BatchIngest::run(store, records)
    }
}

impl Default for StreamIngest {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikura::{
        Action, Aggregate, EvaluateRequest, Hop, LocalCompute, ObjectSet, PropertyAcl, Store,
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
        let mut stream = StreamIngest::new();
        stream
            .push(rec(
                "Shipment",
                "s3",
                false,
                &[("order_id", "o1"), ("amount", "7")],
            ))
            .unwrap();
        stream.flush_into(&mut store).unwrap();
        let oss = ObjectSet::new(LocalCompute);
        let streamed = oss.evaluate(&store, &fixture_request()).unwrap();
        assert_eq!(streamed.sum_amount, 17);
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
}
