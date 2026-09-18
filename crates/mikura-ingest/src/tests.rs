use mikura::{Aggregate, EvaluateRequest, Hop, ObjectRecord, ObjectSet, PropertyAcl, Store};

mod batch;
mod changelog;
mod merge;
mod stream;

pub(crate) fn rec(kind: &str, key: &str, hidden: bool, props: &[(&str, &str)]) -> ObjectRecord {
    ObjectRecord {
        gen: 1,
        kind: kind.into(),
        key: key.into(),
        hidden,
        action_id: None,
        props: props
            .iter()
            .map(|(name, value)| ((*name).into(), (*value).into()))
            .collect(),
    }
}

pub(crate) fn fixture() -> Vec<ObjectRecord> {
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

pub(crate) fn fixture_request() -> EvaluateRequest {
    EvaluateRequest {
        root_kind: "Customer".into(),
        hops: vec![
            Hop {
                far_kind: "Order".into(),
                join_property: "customer_id".into(),
                incoming: false,
            },
            Hop {
                far_kind: "Shipment".into(),
                join_property: "order_id".into(),
                incoming: false,
            },
        ],
        sum_kind: "Shipment".into(),
        sum_property: "amount".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        object_bound: 0,
    }
}

pub(crate) fn temp_log(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("mikura-ingest-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("objects.mikura");
    (dir, log)
}

pub(crate) fn shipment_request() -> EvaluateRequest {
    EvaluateRequest {
        root_kind: "Shipment".into(),
        hops: vec![],
        sum_kind: "Shipment".into(),
        sum_property: "amount".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        object_bound: 0,
    }
}

pub(crate) fn assert_same_identity(live: &Store, reopened: &Store, records: &[ObjectRecord]) {
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
