//! The evaluate aggregate fixture (`docs/plans/evaluate-aggregate-sort-fixture.md`,
//! ADR 0038): every answer below is the fixture's.

use super::super::*;
use super::helpers::*;
use crate::compute::ComputeError;

const SHOP: &str = "shop";

fn order(
    key: &str,
    customer: &str,
    status: &str,
    priority: &str,
    owner: &str,
    amount: &str,
) -> ObjectRecord {
    let mut props = vec![
        ("customer", customer),
        ("status", status),
        ("amount", amount),
    ];
    if !priority.is_empty() {
        props.push(("priority", priority));
    }
    if !owner.is_empty() {
        props.push(("owner", owner));
    }
    rec_in(SHOP, "order", key, false, &props)
}

fn seed(name: &str) -> (std::path::PathBuf, Store) {
    let (dir, log) = temp_log(name);
    let mut store = Store::create(&log).unwrap();
    for record in [
        order("o1", "c1", "open", "2", "ann", "10"),
        order("o2", "c1", "closed", "1", "ann", "7"),
        order("o3", "c2", "open", "3", "bob", "4"),
        order("o4", "c3", "open", "", "bob", ""),
        order("o5", "c3", "closed", "2", "cid", "x12"),
        order("o6", "c4", "open", "1", "ann", "-3"),
        order("o7", "c2", "open", "2", "", "10"),
        order("o8", "c1", "archived", "1", "bob", "1000"),
    ] {
        store.append(record).unwrap();
    }
    for (key, region) in [
        ("c1", "east"),
        ("c2", "west"),
        ("c3", "east"),
        ("c4", ""),
        ("c5", "west"),
    ] {
        let props: Vec<(&str, &str)> = if region.is_empty() {
            Vec::new()
        } else {
            vec![("region", region)]
        };
        store
            .append(rec_in(SHOP, "customer", key, false, &props))
            .unwrap();
    }
    (dir, store)
}

/// The fixture restriction: archived orders do not exist.
fn view() -> PropertyAcl {
    let mut acl = PropertyAcl::allow_all();
    acl.insert_row_filter("order", "status", ["open", "closed"])
        .unwrap();
    acl
}

fn orders(acl: PropertyAcl, breakdown: Breakdown) -> EvaluateRequest {
    EvaluateRequest {
        namespace: SHOP.into(),
        root_kind: "order".into(),
        sum_kind: "order".into(),
        sum_property: "amount".into(),
        acl,
        breakdown: Some(breakdown),
        ..Default::default()
    }
}

fn by(group_by: &[&str], max_groups: usize) -> Breakdown {
    Breakdown {
        group_by: group_by.iter().map(|name| (*name).to_string()).collect(),
        max_groups,
        distinct: None,
    }
}

fn run(store: &Store, request: &EvaluateRequest) -> Result<Vec<Group>, ComputeError> {
    ObjectSet::new(LocalCompute)
        .evaluate(store, request)
        .map(|response| response.groups)
}

/// `(key, count, sum, n, min, max)` per group.
type Row<K> = (Vec<Option<K>>, usize, i64, u64, Option<i64>, Option<i64>);

fn rows(groups: &[Group]) -> Vec<Row<String>> {
    groups
        .iter()
        .map(|g| (g.key.clone(), g.count, g.sum, g.n, g.min, g.max))
        .collect()
}

fn expect(rows_expected: &[Row<&str>]) -> Vec<Row<String>> {
    rows_expected
        .iter()
        .map(|(key, count, sum, n, min, max)| {
            (
                key.iter().map(|part| part.map(str::to_string)).collect(),
                *count,
                *sum,
                *n,
                *min,
                *max,
            )
        })
        .collect()
}

#[test]
fn whole_set_aggregates_and_distinct_counts() {
    let (dir, store) = seed("aggregate-whole");
    // A1
    let groups = run(&store, &orders(view(), by(&[], 0))).unwrap();
    assert_eq!(
        rows(&groups),
        expect(&[(vec![], 7, 28, 5, Some(-3), Some(10))])
    );
    assert_eq!(groups[0].avg, (28, 5));
    // A2, A3
    for (property, distinct) in [("owner", 3), ("status", 2)] {
        let mut breakdown = by(&[], 0);
        breakdown.distinct = Some(property.into());
        let groups = run(&store, &orders(view(), breakdown)).unwrap();
        assert_eq!(groups[0].distinct, Some(distinct), "{property}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn groups_by_one_and_two_root_properties() {
    let (dir, store) = seed("aggregate-groups");
    // G1
    let groups = run(&store, &orders(view(), by(&["status"], 0))).unwrap();
    assert_eq!(
        rows(&groups),
        expect(&[
            (vec![Some("closed")], 2, 7, 1, Some(7), Some(7)),
            (vec![Some("open")], 5, 21, 4, Some(-3), Some(10)),
        ])
    );
    // G2
    let groups = run(&store, &orders(view(), by(&["status", "owner"], 0))).unwrap();
    assert_eq!(
        rows(&groups),
        expect(&[
            (vec![Some("closed"), Some("ann")], 1, 7, 1, Some(7), Some(7)),
            (vec![Some("closed"), Some("cid")], 1, 0, 0, None, None),
            (vec![Some("open"), Some("ann")], 2, 7, 2, Some(-3), Some(10)),
            (vec![Some("open"), Some("bob")], 2, 4, 1, Some(4), Some(4)),
            (vec![Some("open"), None], 1, 10, 1, Some(10), Some(10)),
        ])
    );
    assert_eq!(groups[1].avg, (0, 0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_group_bound_fails_closed() {
    let (dir, store) = seed("aggregate-bound");
    // G3
    assert!(matches!(
        run(&store, &orders(view(), by(&["owner"], 2))),
        Err(ComputeError::TooManyGroups { max: 2 })
    ));
    // G4
    let keys: Vec<_> = run(&store, &orders(view(), by(&["owner"], 4)))
        .unwrap()
        .into_iter()
        .map(|g| g.key)
        .collect();
    assert_eq!(
        keys,
        [
            vec![Some("ann".to_string())],
            vec![Some("bob".to_string())],
            vec![Some("cid".to_string())],
            vec![None],
        ]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_hop_measures_each_path_and_groups_by_root() {
    let (dir, store) = seed("aggregate-hop");
    // H1
    let request = EvaluateRequest {
        namespace: SHOP.into(),
        root_kind: "customer".into(),
        hops: vec![Hop {
            far_kind: "order".into(),
            join_property: "customer".into(),
            incoming: false,
            predicate: None,
        }],
        sum_kind: "order".into(),
        sum_property: "amount".into(),
        acl: view(),
        breakdown: Some(by(&["region"], 0)),
        ..Default::default()
    };
    assert_eq!(
        rows(&run(&store, &request).unwrap()),
        expect(&[
            (vec![Some("east")], 2, 17, 2, Some(7), Some(10)),
            (vec![Some("west")], 1, 14, 2, Some(4), Some(10)),
            (vec![None], 1, -3, 1, Some(-3), Some(-3)),
        ])
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn denied_properties_fail_closed() {
    let (dir, store) = seed("aggregate-denied");
    // D1 (the sort case is multi-key sort's)
    let mut denied = PropertyAcl::deny_property("order", "owner");
    denied
        .insert_row_filter("order", "status", ["open", "closed"])
        .unwrap();
    assert!(matches!(
        run(&store, &orders(denied.clone(), by(&["owner"], 0))),
        Err(ComputeError::Acl(_))
    ));
    let mut breakdown = by(&[], 0);
    breakdown.distinct = Some("owner".into());
    assert!(matches!(
        run(&store, &orders(denied, breakdown)),
        Err(ComputeError::Acl(_))
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_average_sum_is_checked_and_the_plain_sum_still_wraps() {
    let (dir, log) = temp_log("aggregate-overflow");
    let mut store = Store::create(&log).unwrap();
    store
        .append(rec_in(
            "ovf",
            "order",
            "o1",
            false,
            &[("amount", "9223372036854775807")],
        ))
        .unwrap();
    store
        .append(rec_in("ovf", "order", "o2", false, &[("amount", "1")]))
        .unwrap();
    let mut request = orders(PropertyAcl::allow_all(), by(&[], 0));
    request.namespace = "ovf".into();
    // O1
    assert!(matches!(
        run(&store, &request),
        Err(ComputeError::Overflow(_))
    ));
    // A total back in range after an intermediate overflow is exact,
    // whatever order the paths are visited in.
    store
        .append(rec_in("ovf", "order", "o3", false, &[("amount", "-1")]))
        .unwrap();
    assert_eq!(run(&store, &request).unwrap()[0].avg, (i64::MAX, 3));
    store
        .append(rec_in("ovf", "order", "o3", false, &[("amount", "0")]))
        .unwrap();
    // O2
    request.breakdown = None;
    let response = ObjectSet::new(LocalCompute)
        .evaluate(&store, &request)
        .unwrap();
    assert_eq!(response.sum_amount, i64::MIN);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn malformed_breakdowns_fail_closed() {
    let (dir, store) = seed("aggregate-malformed");
    for breakdown in [
        by(&["status", "owner", "priority"], 0),
        by(&["status", "status"], 0),
        by(&[""], 0),
    ] {
        assert!(matches!(
            run(&store, &orders(view(), breakdown)),
            Err(ComputeError::Predicate(_))
        ));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Group-by cost on a synthetic set; run with
/// `cargo test --release -p mikura --lib -- --ignored group_by_cost --nocapture`.
#[test]
#[ignore = "measurement, not a check"]
fn group_by_cost() {
    let (dir, log) = temp_log("aggregate-cost");
    let mut store = Store::create(&log).unwrap();
    let records: Vec<_> = (0..200_000)
        .map(|i| {
            let owner = format!("u{}", i % 50);
            let status = ["open", "closed", "held"][i % 3];
            let amount = (i % 1000).to_string();
            rec_in(
                SHOP,
                "order",
                &format!("o{i}"),
                false,
                &[("owner", &owner), ("status", status), ("amount", &amount)],
            )
        })
        .collect();
    store.append_batch(records).unwrap();
    for group_by in [&[][..], &["status"][..], &["status", "owner"][..]] {
        let request = orders(PropertyAcl::allow_all(), by(group_by, 0));
        let started = std::time::Instant::now();
        let groups = run(&store, &request).unwrap();
        println!(
            "200000 roots, group_by {group_by:?}: {} groups in {:?}",
            groups.len(),
            started.elapsed()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
