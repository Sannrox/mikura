//! kura v1: ingest → object log → object-set evaluate.
//!
//! Compute backends, streaming ingest, property ACLs, and Action writeback
//! are first-class seams. Spark is a named backend that fails closed until
//! an envelope exists.

mod acl;
mod actions;
mod compute;
mod ingest;
mod objectset;
mod store;

pub use acl::{AclError, PropertyAcl};
pub use actions::Action;
pub use compute::{ComputeBackend, ComputeError, LocalCompute, SparkCompute};
pub use ingest::{BatchIngest, StreamIngest};
pub use objectset::{Aggregate, EvaluateRequest, EvaluateResponse, ObjectSet};
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

    #[test]
    fn ingest_evaluate_rebuild_acl_action_and_spark_fail_closed() {
        let dir = std::env::temp_dir().join("kura-v1-lib-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("objects.jsonl");
        let mut store = Store::create(&log).unwrap();
        BatchIngest::run(&mut store, fixture()).unwrap();

        let oss = ObjectSet::new(LocalCompute);
        let visible = oss
            .evaluate(
                &store,
                &EvaluateRequest {
                    aggregate: Aggregate::CountAndSum,
                    acl: PropertyAcl::allow_all(),
                },
            )
            .unwrap();
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
        let after = oss
            .evaluate(
                &store,
                &EvaluateRequest {
                    aggregate: Aggregate::CountAndSum,
                    acl: PropertyAcl::allow_all(),
                },
            )
            .unwrap();
        assert_eq!(after.sum_amount, 15);

        let denied = oss.evaluate(
            &store,
            &EvaluateRequest {
                aggregate: Aggregate::CountAndSum,
                acl: PropertyAcl::deny_property("Shipment", "amount"),
            },
        );
        assert!(matches!(denied, Err(ComputeError::Acl(AclError::Denied { .. }))));

        let rebuilt = Store::open(&log).unwrap();
        let from_log = oss
            .evaluate(
                &rebuilt,
                &EvaluateRequest {
                    aggregate: Aggregate::CountAndSum,
                    acl: PropertyAcl::allow_all(),
                },
            )
            .unwrap();
        assert_eq!(from_log.two_hop_count, after.two_hop_count);
        assert_eq!(from_log.sum_amount, after.sum_amount);

        let spark = ObjectSet::new(SparkCompute);
        let err = spark
            .evaluate(
                &store,
                &EvaluateRequest {
                    aggregate: Aggregate::CountAndSum,
                    acl: PropertyAcl::allow_all(),
                },
            )
            .unwrap_err();
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
        let streamed = oss
            .evaluate(
                &store,
                &EvaluateRequest {
                    aggregate: Aggregate::CountAndSum,
                    acl: PropertyAcl::allow_all(),
                },
            )
            .unwrap();
        assert_eq!(streamed.sum_amount, 22);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
