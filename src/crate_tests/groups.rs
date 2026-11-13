//! One-property breakdown groups kept current at commit (#372, ADR 0049).

use super::super::*;
use super::helpers::*;
use crate::compute::ComputeError;

fn shipment_schema() -> SchemaDescriptor {
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
}

fn ship(key: &str, weight: &str, amount: &str, hidden: bool) -> ObjectRecord {
    rec(
        "Shipment",
        key,
        hidden,
        &[("order_id", "o1"), ("weight", weight), ("amount", amount)],
    )
}

fn seed(name: &str) -> (std::path::PathBuf, std::path::PathBuf, Store) {
    let (dir, log) = temp_log(name);
    let mut store = Store::create(&log).unwrap();
    store
        .append(shipment_schema().to_record(NS).unwrap())
        .unwrap();
    store.append(rec("Order", "o1", false, &[])).unwrap();
    for record in [
        ship("s1", "1", "10", false),
        ship("s2", "1", "5", false),
        ship("s3", "2", "7", false),
        ship("s4", "", "3", false),
        rec(
            "Shipment",
            "s5",
            false,
            &[("order_id", "o1"), ("amount", "4")],
        ),
        ship("s0", "9", "99", true),
    ] {
        store.append(record).unwrap();
    }
    (dir, log, store)
}

fn by_weight() -> EvaluateRequest {
    EvaluateRequest {
        namespace: NS.into(),
        root_kind: "Shipment".into(),
        sum_kind: "Shipment".into(),
        sum_property: "amount".into(),
        breakdown: Some(Breakdown {
            group_by: vec!["weight".into()],
            max_groups: 0,
            distinct: None,
        }),
        ..Default::default()
    }
}

fn walked() -> EvaluateRequest {
    let mut request = by_weight();
    request.predicate = Some(!Predicate::eq("weight", "__none__"));
    request
}

fn count_sum(groups: &[Group]) -> Vec<(Option<String>, usize, i64)> {
    groups
        .iter()
        .map(|g| (g.key.first().cloned().flatten(), g.count, g.sum))
        .collect()
}

fn evaluate(store: &Store, request: &EvaluateRequest) -> Vec<Group> {
    ObjectSet::new(LocalCompute)
        .evaluate(store, request)
        .unwrap()
        .groups
}

#[test]
fn one_property_groups_match_a_walk_and_reopen() {
    let (dir, log, mut store) = seed("groups-dual");
    let live = evaluate(&store, &by_weight());
    assert_eq!(count_sum(&live), count_sum(&evaluate(&store, &walked())));
    assert_eq!(
        count_sum(&live),
        [
            (Some("1".into()), 2, 15),
            (Some("2".into()), 1, 7),
            (None, 2, 7),
        ]
    );

    store.append(ship("s6", "2", "1", false)).unwrap();
    let after_append = evaluate(&store, &by_weight());
    assert_eq!(
        count_sum(&after_append),
        [
            (Some("1".into()), 2, 15),
            (Some("2".into()), 2, 8),
            (None, 2, 7),
        ]
    );

    store
        .hide(NS, "Shipment", "s3", &PropertyAcl::allow_all())
        .unwrap();
    let after_hide = evaluate(&store, &by_weight());
    assert_eq!(
        count_sum(&after_hide),
        [
            (Some("1".into()), 2, 15),
            (Some("2".into()), 1, 1),
            (None, 2, 7),
        ]
    );

    drop(store);
    let reopened = Store::open(&log).unwrap();
    assert_eq!(
        count_sum(&evaluate(&reopened, &by_weight())),
        count_sum(&after_hide)
    );
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    assert_eq!(
        count_sum(&evaluate(&replayed, &by_weight())),
        count_sum(&after_hide)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn empty_groups_disappear() {
    let (dir, _, mut store) = seed("groups-empty");
    store.append(ship("s7", "8", "2", false)).unwrap();
    assert!(count_sum(&evaluate(&store, &by_weight()))
        .iter()
        .any(|(key, _, _)| key.as_deref() == Some("8")));
    store
        .hide(NS, "Shipment", "s7", &PropertyAcl::allow_all())
        .unwrap();
    assert!(count_sum(&evaluate(&store, &by_weight()))
        .iter()
        .all(|(key, _, _)| key.as_deref() != Some("8")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn earlier_row_without_group_property_stays_absent() {
    let (dir, log) = temp_log("groups-late-prop");
    let mut store = Store::create(&log).unwrap();
    store
        .append(shipment_schema().to_record(NS).unwrap())
        .unwrap();
    store.append(rec("Order", "o1", false, &[])).unwrap();
    store
        .append(rec(
            "Shipment",
            "s5",
            false,
            &[("order_id", "o1"), ("amount", "4")],
        ))
        .unwrap();
    store.append(ship("s1", "1", "10", false)).unwrap();
    assert_eq!(
        count_sum(&evaluate(&store, &by_weight())),
        count_sum(&evaluate(&store, &walked()))
    );
    assert_eq!(
        count_sum(&evaluate(&store, &by_weight())),
        [(Some("1".into()), 1, 10), (None, 1, 4)]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn join_property_groups_keep_absent_after_reopen() {
    let (dir, log, mut store) = seed("groups-join-prop");
    store
        .append(rec("Shipment", "s8", false, &[("amount", "6")]))
        .unwrap();
    let mut request = by_weight();
    request.breakdown = Some(Breakdown {
        group_by: vec!["order_id".into()],
        max_groups: 0,
        distinct: None,
    });
    let mut walked_req = request.clone();
    walked_req.predicate = Some(!Predicate::eq("order_id", "__none__"));
    let live = evaluate(&store, &request);
    assert_eq!(count_sum(&live), count_sum(&evaluate(&store, &walked_req)));
    assert!(count_sum(&live)
        .iter()
        .any(|(key, count, sum)| key.is_none() && *count == 1 && *sum == 6));
    drop(store);
    let reopened = Store::open(&log).unwrap();
    assert_eq!(count_sum(&evaluate(&reopened, &request)), count_sum(&live));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn undeclared_sum_keeps_absent_after_reopen() {
    let (dir, log) = temp_log("groups-undeclared");
    let mut store = Store::create(&log).unwrap();
    store
        .append(
            SchemaDescriptor {
                kind: "Shipment".into(),
                properties: vec!["order_id".into(), "amount".into(), "weight".into()],
                links: vec![SchemaLink {
                    name: "order_id".into(),
                    far_kind: "Order".into(),
                    outgoing: true,
                }],
                ..SchemaDescriptor::default()
            }
            .to_record(NS)
            .unwrap(),
        )
        .unwrap();
    store.append(rec("Order", "o1", false, &[])).unwrap();
    store.append(ship("s1", "1", "10", false)).unwrap();
    store
        .append(rec(
            "Shipment",
            "s5",
            false,
            &[("order_id", "o1"), ("amount", "4")],
        ))
        .unwrap();
    let live = evaluate(&store, &by_weight());
    assert_eq!(count_sum(&live), count_sum(&evaluate(&store, &walked())));
    assert_eq!(count_sum(&live), [(Some("1".into()), 1, 10), (None, 1, 4)]);
    drop(store);
    let reopened = Store::open(&log).unwrap();
    assert_eq!(
        count_sum(&evaluate(&reopened, &by_weight())),
        count_sum(&live)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hide_clears_lazy_absent_group() {
    let (dir, log) = temp_log("groups-hide-absent");
    let mut store = Store::create(&log).unwrap();
    store
        .append(
            SchemaDescriptor {
                kind: "Shipment".into(),
                properties: vec!["order_id".into(), "amount".into(), "weight".into()],
                links: vec![SchemaLink {
                    name: "order_id".into(),
                    far_kind: "Order".into(),
                    outgoing: true,
                }],
                ..SchemaDescriptor::default()
            }
            .to_record(NS)
            .unwrap(),
        )
        .unwrap();
    store.append(rec("Order", "o1", false, &[])).unwrap();
    store
        .append(rec(
            "Shipment",
            "s5",
            false,
            &[("order_id", "o1"), ("amount", "4")],
        ))
        .unwrap();
    assert_eq!(count_sum(&evaluate(&store, &by_weight())), [(None, 1, 4)]);
    store
        .hide(NS, "Shipment", "s5", &PropertyAcl::allow_all())
        .unwrap();
    assert!(evaluate(&store, &by_weight()).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grouping_by_the_sum_property_matches_a_walk() {
    let (dir, _, mut store) = seed("groups-by-sum");
    let mut request = by_weight();
    request.breakdown = Some(Breakdown {
        group_by: vec!["amount".into()],
        max_groups: 0,
        distinct: None,
    });
    let mut walked_req = request.clone();
    walked_req.predicate = Some(!Predicate::eq("amount", "__none__"));
    store.append(ship("s8", "1", "10", false)).unwrap();
    assert_eq!(
        count_sum(&evaluate(&store, &request)),
        count_sum(&evaluate(&store, &walked_req))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn denied_group_property_fails_closed() {
    let (dir, _, store) = seed("groups-denied");
    let mut request = by_weight();
    request.acl = PropertyAcl::deny_property("Shipment", "weight");
    assert!(matches!(
        ObjectSet::new(LocalCompute).evaluate(&store, &request),
        Err(ComputeError::Acl(_))
    ));
    let _ = std::fs::remove_dir_all(&dir);
}
