use super::super::*;
pub(super) use std::collections::HashMap;

pub(super) fn rec(kind: &str, key: &str, hidden: bool, props: &[(&str, &str)]) -> ObjectRecord {
    ObjectRecord {
        gen: 1,
        kind: kind.into(),
        key: key.into(),
        hidden,
        action_id: None,
        props: props
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect(),
    }
}

pub(super) fn fixture_request() -> EvaluateRequest {
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

pub(super) fn fixture() -> Vec<ObjectRecord> {
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

pub(super) fn generic_records() -> Vec<ObjectRecord> {
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

pub(super) fn asset_request() -> EvaluateRequest {
    EvaluateRequest {
        root_kind: "Customer".into(),
        hops: vec![Hop {
            far_kind: "Asset".into(),
            join_property: "owner_id".into(),
            incoming: false,
        }],
        sum_kind: "Asset".into(),
        sum_property: "mass".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        object_bound: 0,
    }
}

pub(super) fn temp_log(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("mikura-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("objects.mikura");
    (dir, log)
}

pub(super) fn append_all(store: &mut Store, records: Vec<ObjectRecord>) {
    for record in records {
        store.append(record).unwrap();
    }
}

pub(super) fn product_loop_schemas() -> (SchemaDescriptor, SchemaDescriptor) {
    (
        SchemaDescriptor {
            kind: "component".into(),
            properties: vec!["name".into(), "tier".into()],
            required: vec!["name".into(), "tier".into()],
            links: vec![SchemaLink {
                name: "affects".into(),
                far_kind: "incident".into(),
                outgoing: false,
            }],
        },
        SchemaDescriptor {
            kind: "incident".into(),
            properties: vec!["affects".into(), "name".into()],
            required: vec!["name".into()],
            links: vec![SchemaLink {
                name: "affects".into(),
                far_kind: "component".into(),
                outgoing: true,
            }],
        },
    )
}

pub(super) fn product_loop_seed() -> Vec<ObjectRecord> {
    vec![
        rec(
            "component",
            "svc-api",
            false,
            &[("name", "billing-api"), ("tier", "prod")],
        ),
        rec(
            "incident",
            "inc-1",
            false,
            &[("name", "elevated latency"), ("affects", "svc-api")],
        ),
    ]
}

pub(super) fn list_prod_components(bound: usize) -> EvaluateRequest {
    EvaluateRequest {
        root_kind: "component".into(),
        hops: vec![],
        sum_kind: "component".into(),
        sum_property: "tier".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: Some(ExactMatch {
            property: "tier".into(),
            value: "prod".into(),
        }),
        object_bound: bound,
    }
}

pub(super) fn hop_incident_to_component(bound: usize) -> EvaluateRequest {
    EvaluateRequest {
        root_kind: "incident".into(),
        hops: vec![Hop {
            far_kind: "component".into(),
            join_property: "affects".into(),
            incoming: true,
        }],
        sum_kind: "component".into(),
        sum_property: "tier".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        object_bound: bound,
    }
}
