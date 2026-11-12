//! Small-fixture serving bar (#355): evaluate shapes match log replay, and
//! a non-empty deny list plus hide_identities are enforced per request.

use super::super::*;
use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

const BAR_NS: &str = "bench";
const BAR_OBJECTS: u64 = 400;

struct Scale {
    customers: u64,
    orders: u64,
    shipments: u64,
}

impl Scale {
    fn new(objects: u64) -> Self {
        let customers = (objects / 100).max(1);
        let orders = (objects / 10).max(1);
        Self {
            customers,
            orders,
            shipments: objects.saturating_sub(customers + orders).max(1),
        }
    }
}

fn bar_log(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "mikura-serving-bar-{name}-{}-{nanos}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("objects.mikura");
    (dir, log)
}

fn rec_bar(kind: &str, prefix: char, id: u64, props: Vec<(&str, String)>) -> ObjectRecord {
    ObjectRecord {
        gen: 1,
        namespace: BAR_NS.into(),
        kind: kind.into(),
        key: format!("{prefix}{id}"),
        hidden: id.is_multiple_of(100),
        action_id: None,
        props: props
            .into_iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect(),
        schema_version: 1,
    }
}

fn shipment_schema() -> ObjectRecord {
    SchemaDescriptor {
        kind: "Shipment".into(),
        properties: vec!["order_id".into(), "amount".into(), "weight".into()],
        links: vec![SchemaLink {
            name: "order_id".into(),
            far_kind: "Order".into(),
            outgoing: true,
        }],
        sums: vec!["amount".into()],
        ..SchemaDescriptor::default()
    }
    .to_record(BAR_NS)
    .expect("shipment schema")
}

fn seed(store: &mut Store, scale: &Scale) {
    store.append(shipment_schema()).unwrap();
    let mut batch = Vec::new();
    for id in 0..scale.customers {
        let region = ["eu", "us", "ap"][(id % 3) as usize];
        batch.push(rec_bar(
            "Customer",
            'c',
            id,
            vec![("region", region.into())],
        ));
    }
    for id in 0..scale.orders {
        batch.push(rec_bar(
            "Order",
            'o',
            id,
            vec![("customer_id", format!("c{}", id % scale.customers))],
        ));
    }
    for id in 0..scale.shipments {
        batch.push(rec_bar(
            "Shipment",
            's',
            id,
            vec![
                ("order_id", format!("o{}", id % scale.orders)),
                ("amount", format!("{}", id % 50 + 1)),
                ("weight", format!("{}", id % 7 + 1)),
            ],
        ));
    }
    for record in batch {
        store.append_uncommitted(record).unwrap();
    }
    store.commit().unwrap();
}

fn hop(far_kind: &str, join_property: &str, incoming: bool) -> Hop {
    Hop {
        far_kind: far_kind.into(),
        join_property: join_property.into(),
        incoming,
        predicate: None,
    }
}

fn base(root: &str, hops: Vec<Hop>, sum_kind: &str, sum_property: &str) -> EvaluateRequest {
    EvaluateRequest {
        namespace: BAR_NS.into(),
        root_kind: root.into(),
        hops,
        sum_kind: sum_kind.into(),
        sum_property: sum_property.into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        ..EvaluateRequest::default()
    }
}

fn shapes(scale: &Scale) -> Vec<(&'static str, EvaluateRequest)> {
    let outgoing = vec![
        hop("Order", "customer_id", false),
        hop("Shipment", "order_id", false),
    ];
    let incoming = vec![hop("Customer", "customer_id", true)];
    let bound = (scale.shipments as usize).saturating_add(1);
    vec![
        (
            "hops_outgoing",
            base("Customer", outgoing, "Shipment", "amount"),
        ),
        (
            "hops_incoming",
            base("Order", incoming, "Order", "customer_id"),
        ),
        (
            "filter",
            EvaluateRequest {
                filter: Some(ExactMatch {
                    property: "weight".into(),
                    value: "3".into(),
                }),
                object_bound: bound,
                sort: Some(Sort::by("amount")),
                page_size: 20,
                ..base("Shipment", Vec::new(), "Shipment", "amount")
            },
        ),
        (
            "predicate",
            EvaluateRequest {
                predicate: Some(Predicate::eq("weight", "3")),
                object_bound: bound,
                sort: Some(Sort::by("amount")),
                page_size: 20,
                ..base("Shipment", Vec::new(), "Shipment", "amount")
            },
        ),
        (
            "grouping",
            EvaluateRequest {
                breakdown: Some(Breakdown {
                    group_by: vec!["region".into()],
                    max_groups: 10,
                    distinct: None,
                }),
                ..base("Customer", Vec::new(), "Customer", "region")
            },
        ),
        (
            "multi_key_sort",
            EvaluateRequest {
                object_bound: bound,
                sort: Some(Sort::by("amount")),
                then_by: vec![Sort::by("weight")],
                page_size: 20,
                ..base("Shipment", Vec::new(), "Shipment", "amount")
            },
        ),
        (
            "distinct_page",
            EvaluateRequest {
                object_bound: (scale.customers as usize).saturating_add(1),
                sort: Some(Sort::by("region")),
                page_size: 2,
                hops: vec![hop("Customer", "customer_id", true)],
                ..base("Order", Vec::new(), "Customer", "region")
            },
        ),
        (
            "breakdown_distinct",
            EvaluateRequest {
                breakdown: Some(Breakdown {
                    group_by: vec!["weight".into()],
                    max_groups: 20,
                    distinct: Some("order_id".into()),
                }),
                ..base("Shipment", Vec::new(), "Shipment", "amount")
            },
        ),
    ]
}

fn object_fingerprint(
    record: &ObjectRecord,
) -> (String, String, String, bool, Vec<(String, String)>) {
    let mut props: Vec<(String, String)> = record
        .props
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    props.sort();
    (
        record.namespace.clone(),
        record.kind.clone(),
        record.key.clone(),
        record.hidden,
        props,
    )
}

fn answers_match(
    name: &str,
    live: &EvaluateResponse,
    replayed: &EvaluateResponse,
) -> Result<(), String> {
    if live.two_hop_count != replayed.two_hop_count {
        return Err(format!(
            "{name}: count {} vs replay {}",
            live.two_hop_count, replayed.two_hop_count
        ));
    }
    if live.sum_amount != replayed.sum_amount {
        return Err(format!(
            "{name}: sum {} vs replay {}",
            live.sum_amount, replayed.sum_amount
        ));
    }
    if live.groups != replayed.groups {
        return Err(format!("{name}: groups differ from log replay"));
    }
    let live_objs: Vec<_> = live.objects.iter().map(object_fingerprint).collect();
    let replayed_objs: Vec<_> = replayed.objects.iter().map(object_fingerprint).collect();
    if live_objs != replayed_objs {
        return Err(format!("{name}: page identities differ from log replay"));
    }
    if live.cursor.is_some() != replayed.cursor.is_some() {
        return Err(format!("{name}: cursor presence differs from log replay"));
    }
    Ok(())
}

fn remove_projections(log: &std::path::Path) {
    for path in [
        Store::join_map_path(log),
        Store::join_delta_path(log),
        Store::checkpoint_path(log),
        Store::checkpoint_prev_path(log),
    ] {
        let _ = std::fs::remove_file(path);
    }
}

fn visible_shipment(scale: &Scale) -> String {
    (0..scale.shipments)
        .find(|id| !id.is_multiple_of(100))
        .map(|id| format!("s{id}"))
        .expect("visible shipment")
}

#[test]
fn serving_bar_shapes_match_log_replay() {
    let (dir, log) = bar_log("parity");
    let scale = Scale::new(BAR_OBJECTS);
    let mut store = Store::create(&log).unwrap();
    seed(&mut store, &scale);
    let oss = ObjectSet::new(LocalCompute);
    let requests = shapes(&scale);
    let live: Vec<(&str, EvaluateResponse)> = requests
        .iter()
        .map(|(name, request)| {
            (
                *name,
                oss.evaluate(&store, request)
                    .unwrap_or_else(|err| panic!("{name}: live evaluate: {err:?}")),
            )
        })
        .collect();
    let mut distinct = requests
        .iter()
        .find(|(name, _)| *name == "distinct_page")
        .expect("distinct_page")
        .1
        .clone();
    let mut seen = HashSet::new();
    for _ in 0..=distinct.object_bound {
        let page = oss.evaluate(&store, &distinct).unwrap();
        for row in &page.objects {
            assert!(
                seen.insert((row.kind.clone(), row.key.clone())),
                "distinct_page duplicate {}/{}",
                row.kind,
                row.key
            );
        }
        match page.cursor {
            Some(cursor) => distinct.cursor = Some(cursor),
            None => break,
        }
    }
    assert!(!seen.is_empty(), "distinct_page returned no identities");
    drop(store);
    remove_projections(&log);
    let replayed = Store::open(&log).unwrap();
    for ((name, request), (_, live_answer)) in requests.iter().zip(live.iter()) {
        let from_log = oss
            .evaluate(&replayed, request)
            .unwrap_or_else(|err| panic!("{name}: replay evaluate: {err:?}"));
        answers_match(name, live_answer, &from_log).unwrap();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn serving_bar_enforces_deny_and_hide_per_request() {
    let (dir, log) = bar_log("policy");
    let scale = Scale::new(BAR_OBJECTS);
    let mut store = Store::create(&log).unwrap();
    seed(&mut store, &scale);
    let oss = ObjectSet::new(LocalCompute);
    let key = visible_shipment(&scale);
    let open = PropertyAcl::allow_all();
    let open_row = store.load(BAR_NS, "Shipment", &key, &open).unwrap();
    assert!(
        open_row.props.contains_key("amount"),
        "open path keeps amount"
    );

    let deny = PropertyAcl::deny_property("Shipment", "amount");
    let denied_row = store.load(BAR_NS, "Shipment", &key, &deny).unwrap();
    assert!(
        !denied_row.props.contains_key("amount"),
        "deny omits amount"
    );
    assert_eq!(denied_row.key, key);

    let mut sum_denied = base("Shipment", Vec::new(), "Shipment", "amount");
    sum_denied.acl = deny;
    let err = oss.evaluate(&store, &sum_denied).unwrap_err();
    match err {
        ComputeError::Acl(AclError::Denied { kind, property }) => {
            assert_eq!(kind, "Shipment");
            assert_eq!(property, "amount");
        }
        other => panic!("expected denied sum, got {other:?}"),
    }

    let listed_open = EvaluateRequest {
        object_bound: (scale.shipments as usize).saturating_add(1),
        ..base("Shipment", Vec::new(), "Shipment", "amount")
    };
    let open_page = oss.evaluate(&store, &listed_open).unwrap();
    assert!(
        open_page.objects.iter().any(|row| row.key == key),
        "{key} visible without hide"
    );
    let open_count = open_page.two_hop_count;
    let open_sum = open_page.sum_amount;

    let mut hide = PropertyAcl::allow_all();
    hide.insert_hide_identity("Shipment", &key).unwrap();
    let mut listed_hidden = listed_open.clone();
    listed_hidden.acl = hide;
    let hidden_page = oss.evaluate(&store, &listed_hidden).unwrap();
    assert!(
        hidden_page.objects.iter().all(|row| row.key != key),
        "{key} must vanish under hide_identities"
    );
    assert!(
        hidden_page.two_hop_count < open_count,
        "hide must drop membership"
    );
    assert!(
        hidden_page.sum_amount < open_sum,
        "hide must drop the row from the sum"
    );

    let still_open = oss
        .evaluate(&store, &listed_open)
        .expect("empty ACL stays the open path");
    assert_eq!(still_open.two_hop_count, open_count);
    assert_eq!(still_open.sum_amount, open_sum);
    let _ = std::fs::remove_dir_all(&dir);
}
