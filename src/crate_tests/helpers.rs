use super::super::*;
pub(super) use std::collections::HashMap;

pub(super) const NS: &str = "ns";

pub(super) fn rec(kind: &str, key: &str, hidden: bool, props: &[(&str, &str)]) -> ObjectRecord {
    rec_in(NS, kind, key, hidden, props)
}

pub(super) fn rec_in(
    namespace: &str,
    kind: &str,
    key: &str,
    hidden: bool,
    props: &[(&str, &str)],
) -> ObjectRecord {
    ObjectRecord {
        gen: 1,
        namespace: namespace.into(),
        kind: kind.into(),
        key: key.into(),
        hidden,
        action_id: None,
        schema_version: 1,
        props: props
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect(),
    }
}

pub(super) fn fixture_request() -> EvaluateRequest {
    EvaluateRequest {
        namespace: NS.into(),
        root_kind: "Customer".into(),
        hops: vec![
            Hop {
                far_kind: "Order".into(),
                join_property: "customer_id".into(),
                incoming: false,
                predicate: None,
            },
            Hop {
                far_kind: "Shipment".into(),
                join_property: "order_id".into(),
                incoming: false,
                predicate: None,
            },
        ],
        sum_kind: "Shipment".into(),
        sum_property: "amount".into(),
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
        namespace: NS.into(),
        root_kind: "Customer".into(),
        hops: vec![Hop {
            far_kind: "Asset".into(),
            join_property: "owner_id".into(),
            incoming: false,
            predicate: None,
        }],
        sum_kind: "Asset".into(),
        sum_property: "mass".into(),
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

pub(super) fn temp_log(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("mikura-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("objects.mikura");
    (dir, log)
}

pub(super) fn copy_dir_all(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        if path == to {
            continue;
        }
        let dest = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_dir_all(&path, &dest);
        } else if path.is_file() {
            std::fs::copy(&path, dest).unwrap();
        }
    }
}

pub(super) fn copy_log_tree(from: &std::path::Path, to: &std::path::Path) {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::copy(from, to).unwrap();
    let src_obj = crate::log::objects_dir(from);
    let dst_obj = crate::log::objects_dir(to);
    let _ = std::fs::remove_dir_all(&dst_obj);
    if src_obj.is_dir() {
        copy_dir_all(&src_obj, &dst_obj);
    }
}

pub(super) fn log_tree_contains(path: &std::path::Path, needle: &[u8]) -> bool {
    let in_bytes = |bytes: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    if std::fs::read(path)
        .ok()
        .is_some_and(|bytes| in_bytes(&bytes))
    {
        return true;
    }
    let obj = crate::log::objects_dir(path);
    let Ok(entries) = std::fs::read_dir(&obj) else {
        return false;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_file()
            && std::fs::read(&path)
                .ok()
                .is_some_and(|bytes| in_bytes(&bytes))
        {
            return true;
        }
    }
    false
}

pub(super) fn shipment_sum_schema() -> SchemaDescriptor {
    SchemaDescriptor {
        kind: "Shipment".into(),
        properties: vec!["amount".into(), "order_id".into()],
        required: Vec::new(),
        links: vec![SchemaLink {
            name: "order_id".into(),
            far_kind: "Order".into(),
            outgoing: true,
        }],
        sums: vec!["amount".into()],
        types: Vec::new(),
        ..Default::default()
    }
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
            sums: Vec::new(),
            types: Vec::new(),
            ..Default::default()
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
            sums: Vec::new(),
            types: Vec::new(),
            ..Default::default()
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
        namespace: NS.into(),
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
        predicate: None,
        object_bound: bound,
        sort: None,
        page_size: 0,
        cursor: None,
        breakdown: None,
        then_by: Vec::new(),
    }
}

pub(super) fn hop_incident_to_component(bound: usize) -> EvaluateRequest {
    EvaluateRequest {
        namespace: NS.into(),
        root_kind: "incident".into(),
        hops: vec![Hop {
            far_kind: "component".into(),
            join_property: "affects".into(),
            incoming: true,
            predicate: None,
        }],
        sum_kind: "component".into(),
        sum_property: "tier".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: None,
        object_bound: bound,
        sort: None,
        page_size: 0,
        cursor: None,
        breakdown: None,
        then_by: Vec::new(),
    }
}
