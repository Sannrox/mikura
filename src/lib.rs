//! In-process object database: ingest → object log → object-set evaluate.
//!
//! The log is the store of record ([`Store`]). Join maps persist as a
//! checksummed sidecar and rebuild from the log if absent. [`LocalCompute`]
//! answers hop / count / sum from those maps.
//! [`SparkCompute`] returns [`ComputeError::UnsupportedBackend`] until a
//! published envelope says otherwise.
//!
//! See `docs/architecture.md` in the repository for the v1 contract.

mod acl;
mod actions;
mod compute;
mod ingest;
mod log;
mod objectset;
mod store;

pub use acl::{AclError, PropertyAcl};
pub use actions::Action;
pub use compute::{ComputeBackend, ComputeError, LocalCompute, SparkCompute};
pub use ingest::{BatchIngest, StreamIngest};
pub use objectset::{Aggregate, EvaluateRequest, EvaluateResponse, Hop, ObjectSet};
pub use store::{JoinMaps, ObjectRecord, Store};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn rec(kind: &str, key: &str, hidden: bool, props: &[(&str, &str)]) -> ObjectRecord {
        ObjectRecord {
            gen: 1,
            kind: kind.into(),
            key: key.into(),
            hidden,
            props: props
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
        }
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

    fn generic_records() -> Vec<ObjectRecord> {
        let mut records = fixture();
        records.push(rec(
            "Asset",
            "a1",
            false,
            &[("owner_id", "c1"), ("mass", "4")],
        ));
        records.push(rec(
            "Asset",
            "a0",
            true,
            &[("owner_id", "c0"), ("mass", "99")],
        ));
        records
    }

    fn asset_request() -> EvaluateRequest {
        EvaluateRequest {
            root_kind: "Customer".into(),
            hops: vec![Hop {
                far_kind: "Asset".into(),
                join_property: "owner_id".into(),
            }],
            sum_kind: "Asset".into(),
            sum_property: "mass".into(),
            aggregate: Aggregate::CountAndSum,
            acl: PropertyAcl::allow_all(),
        }
    }

    fn temp_log(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("mikura-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("objects.mikura");
        (dir, log)
    }

    #[test]
    fn ingest_evaluate_rebuild_acl_action_and_spark_fail_closed() {
        let dir = std::env::temp_dir().join("mikura-v1-lib-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("objects.mikura");
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

        let mut denied_req = fixture_request();
        denied_req.acl = PropertyAcl::deny_property("Shipment", "amount");
        let denied = oss.evaluate(&store, &denied_req);
        assert!(matches!(
            denied,
            Err(ComputeError::Acl(AclError::Denied { .. }))
        ));

        let rebuilt = Store::open(&log).unwrap();
        let from_log = oss.evaluate(&rebuilt, &fixture_request()).unwrap();
        assert_eq!(from_log.two_hop_count, after.two_hop_count);
        assert_eq!(from_log.sum_amount, after.sum_amount);

        let spark = ObjectSet::new(SparkCompute);
        let err = spark.evaluate(&store, &fixture_request()).unwrap_err();
        assert!(matches!(err, ComputeError::UnsupportedBackend { .. }));

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
        let streamed = oss.evaluate(&store, &fixture_request()).unwrap();
        assert_eq!(streamed.sum_amount, 22);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_reopen_hop_count_and_sum_match_live_evaluate() {
        let (dir, log) = temp_log("join-reopen");
        let mut store = Store::create(&log).unwrap();
        BatchIngest::run(&mut store, generic_records()).unwrap();
        let oss = ObjectSet::new(LocalCompute);
        let live = oss.evaluate(&store, &fixture_request()).unwrap();
        assert_eq!(live.two_hop_count, 1);
        assert_eq!(live.sum_amount, 10);
        let live_asset = oss.evaluate(&store, &asset_request()).unwrap();
        assert_eq!(live_asset.two_hop_count, 1);
        assert_eq!(live_asset.sum_amount, 4);
        assert!(Store::join_map_path(&log).is_file());
        drop(store);

        let reopened = Store::open(&log).unwrap();
        let from_maps = oss.evaluate(&reopened, &fixture_request()).unwrap();
        assert_eq!(from_maps.two_hop_count, live.two_hop_count);
        assert_eq!(from_maps.sum_amount, live.sum_amount);
        let from_maps_asset = oss.evaluate(&reopened, &asset_request()).unwrap();
        assert_eq!(from_maps_asset.two_hop_count, live_asset.two_hop_count);
        assert_eq!(from_maps_asset.sum_amount, live_asset.sum_amount);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dual_read_projection_equals_log_replay() {
        let (dir, log) = temp_log("join-dual-read");
        let sidecar = Store::join_map_path(&log);
        let mut store = Store::create(&log).unwrap();
        BatchIngest::run(&mut store, generic_records()).unwrap();
        let oss = ObjectSet::new(LocalCompute);
        let from_projection = oss.evaluate(&store, &fixture_request()).unwrap();
        let from_projection_asset = oss.evaluate(&store, &asset_request()).unwrap();
        drop(store);

        std::fs::remove_file(&sidecar).unwrap();
        let replayed = Store::open(&log).unwrap();
        let from_log = oss.evaluate(&replayed, &fixture_request()).unwrap();
        let from_log_asset = oss.evaluate(&replayed, &asset_request()).unwrap();
        assert_eq!(from_log.two_hop_count, from_projection.two_hop_count);
        assert_eq!(from_log.sum_amount, from_projection.sum_amount);
        assert_eq!(
            from_log_asset.two_hop_count,
            from_projection_asset.two_hop_count
        );
        assert_eq!(from_log_asset.sum_amount, from_projection_asset.sum_amount);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generic_kind_join_property_survives_reopen() {
        let (dir, log) = temp_log("join-generic");
        let mut store = Store::create(&log).unwrap();
        BatchIngest::run(&mut store, generic_records()).unwrap();
        assert!(store.joins().is_visible("Asset", "a1"));
        assert_eq!(store.joins().prop("Asset", "a1", "owner_id"), Some("c1"));
        drop(store);

        let reopened = Store::open(&log).unwrap();
        assert!(reopened.joins().is_visible("Asset", "a1"));
        assert_eq!(reopened.joins().prop("Asset", "a1", "owner_id"), Some("c1"));
        let oss = ObjectSet::new(LocalCompute);
        let response = oss.evaluate(&reopened, &asset_request()).unwrap();
        assert_eq!(response.two_hop_count, 1);
        assert_eq!(response.sum_amount, 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hidden_keys_absent_from_join_maps_after_reopen() {
        let (dir, log) = temp_log("join-hidden");
        let mut store = Store::create(&log).unwrap();
        BatchIngest::run(&mut store, generic_records()).unwrap();
        drop(store);

        let reopened = Store::open(&log).unwrap();
        assert!(!reopened.joins().is_visible("Customer", "c0"));
        assert!(!reopened.joins().is_visible("Order", "o0"));
        assert!(!reopened.joins().is_visible("Shipment", "s0"));
        assert!(!reopened.joins().is_visible("Asset", "a0"));
        assert!(reopened.joins().is_visible("Customer", "c1"));
        assert!(reopened.joins().is_visible("Asset", "a1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn join_sidecar_checksum_mismatch_and_truncate_fail_closed() {
        let (dir, log) = temp_log("join-checksum");
        let sidecar = Store::join_map_path(&log);
        let mut store = Store::create(&log).unwrap();
        BatchIngest::run(&mut store, generic_records()).unwrap();
        let oss = ObjectSet::new(LocalCompute);
        let live = oss.evaluate(&store, &fixture_request()).unwrap();
        drop(store);

        let good = std::fs::read(&sidecar).unwrap();
        let mut flipped = good.clone();
        let last = flipped.len() - 5;
        flipped[last] ^= 0x01;
        std::fs::write(&sidecar, &flipped).unwrap();
        let flipped_err = match Store::open(&log) {
            Err(err) => err,
            Ok(_) => panic!("bit-flip should fail closed"),
        };
        assert!(
            flipped_err.contains("checksum"),
            "bit-flip should fail closed: {flipped_err}"
        );

        std::fs::write(&sidecar, &good[..3]).unwrap();
        let truncated_err = match Store::open(&log) {
            Err(err) => err,
            Ok(_) => panic!("truncate should fail closed"),
        };
        assert!(
            truncated_err.contains("short") || truncated_err.contains("checksum"),
            "truncate should fail closed: {truncated_err}"
        );

        std::fs::remove_file(&sidecar).unwrap();
        let recovered = Store::open(&log).unwrap();
        let from_log = oss.evaluate(&recovered, &fixture_request()).unwrap();
        assert_eq!(from_log.two_hop_count, live.two_hop_count);
        assert_eq!(from_log.sum_amount, live.sum_amount);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_join_sidecar_rebuilds_from_log() {
        let (dir, log) = temp_log("join-stale");
        let sidecar = Store::join_map_path(&log);
        let mut store = Store::create(&log).unwrap();
        BatchIngest::run(&mut store, fixture()).unwrap();
        drop(store);
        let stale = std::fs::read(&sidecar).unwrap();

        let mut store = Store::open(&log).unwrap();
        store
            .append(rec(
                "Shipment",
                "s2",
                false,
                &[("order_id", "o1"), ("amount", "5")],
            ))
            .unwrap();
        drop(store);
        std::fs::write(&sidecar, stale).unwrap();

        let reopened = Store::open(&log).unwrap();
        let oss = ObjectSet::new(LocalCompute);
        let response = oss.evaluate(&reopened, &fixture_request()).unwrap();
        assert_eq!(response.two_hop_count, 1);
        assert_eq!(response.sum_amount, 15);
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

    #[test]
    fn single_record_append_is_durable_across_reopen() {
        let (dir, log) = temp_log("group-commit-single");
        let mut store = Store::create(&log).unwrap();
        store
            .append(rec("Customer", "c1", false, &[("region", "us")]))
            .unwrap();
        drop(store);
        let reopened = Store::open(&log).unwrap();
        assert!(reopened.joins().is_visible("Customer", "c1"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
