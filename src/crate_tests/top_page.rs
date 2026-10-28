//! Paged single-key sorts answered without ranking every result (#316)
//! against the full ranking. A `then_by` on a property no row holds keeps
//! the same order but takes the full-ranking path, so it is the oracle.

use super::super::*;
use super::helpers::*;

/// Deterministic xorshift; the seed picks the data set.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

fn item_schema() -> SchemaDescriptor {
    SchemaDescriptor {
        kind: "Item".into(),
        properties: ["order_id", "w", "lo", "hi", "label", "secret", "team"]
            .map(String::from)
            .to_vec(),
        links: vec![SchemaLink {
            name: "order_id".into(),
            far_kind: "Order".into(),
            outgoing: true,
        }],
        types: vec![
            ("lo".into(), PropertyType::Integer),
            ("hi".into(), PropertyType::Integer),
        ],
        ..Default::default()
    }
}

/// Items with low- and high-cardinality sort values, missing values, hidden
/// rows, rewrites that hide or move rows, and orders they link to.
fn seed(store: &mut Store, rng: &mut Rng) {
    store.append(item_schema().to_record(NS).unwrap()).unwrap();
    let mut records: Vec<ObjectRecord> = (0..20)
        .map(|id| rec("Order", &format!("o{id}"), id % 9 == 0, &[]))
        .collect();
    for round in 0..2 {
        for id in 0..80 {
            if round == 1 && rng.below(3) != 0 {
                continue;
            }
            let order = format!("o{}", rng.below(20));
            let w = rng.below(3).to_string();
            let lo = rng.below(4).to_string();
            let hi = rng.below(100_000).to_string();
            let label = format!("l{}", rng.below(50));
            let team = ["red", "blue"][rng.below(2) as usize];
            let mut props = vec![
                ("order_id", order.as_str()),
                ("w", w.as_str()),
                ("secret", "x"),
                ("team", team),
            ];
            if rng.below(5) != 0 {
                props.push(("lo", lo.as_str()));
            }
            if rng.below(5) != 0 {
                props.push(("hi", hi.as_str()));
            }
            if rng.below(4) != 0 {
                props.push(("label", label.as_str()));
            }
            records.push(rec("Item", &format!("i{id}"), rng.below(10) == 0, &props));
        }
    }
    append_all(store, records);
}

fn acls() -> Vec<PropertyAcl> {
    let mut row_filter = PropertyAcl::deny_property("Item", "secret");
    row_filter
        .insert_row_filter("Item", "team", ["red"])
        .unwrap();
    let mut hide = PropertyAcl::allow_all();
    for id in (0..80).step_by(7) {
        hide.insert_hide_identity("Item", &format!("i{id}"))
            .unwrap();
    }
    hide.insert_hide_identity("Order", "o3").unwrap();
    vec![
        PropertyAcl::allow_all(),
        PropertyAcl::deny_property("Item", "secret"),
        row_filter,
        hide,
    ]
}

/// Root, filter, predicate, and hop shapes that list `Item`.
fn shapes(acl: &PropertyAcl) -> Vec<EvaluateRequest> {
    let base = EvaluateRequest {
        namespace: NS.into(),
        root_kind: "Item".into(),
        sum_kind: "Item".into(),
        sum_property: "lo".into(),
        acl: acl.clone(),
        object_bound: 1000,
        ..Default::default()
    };
    vec![
        base.clone(),
        EvaluateRequest {
            filter: Some(ExactMatch {
                property: "w".into(),
                value: "1".into(),
            }),
            ..base.clone()
        },
        EvaluateRequest {
            predicate: Some(Predicate::neq("w", "2")),
            ..base.clone()
        },
        EvaluateRequest {
            root_kind: "Order".into(),
            hops: vec![Hop {
                far_kind: "Item".into(),
                join_property: "order_id".into(),
                incoming: false,
                predicate: None,
            }],
            ..base
        },
    ]
}

/// Every page from the first, following cursors, as comparable tuples.
fn pages(store: &Store, request: &EvaluateRequest) -> Vec<(usize, i64, Vec<ObjectRecord>, bool)> {
    let mut out = Vec::new();
    let mut request = request.clone();
    loop {
        let response = LocalCompute.evaluate(store, &request).unwrap();
        let more = response.cursor.is_some();
        out.push((
            response.two_hop_count,
            response.sum_amount,
            response.objects,
            more,
        ));
        match response.cursor {
            Some(cursor) => request.cursor = Some(cursor),
            None => return out,
        }
    }
}

#[test]
fn paged_single_key_sort_matches_full_ranking() {
    for seed_value in [0x9e37_79b9_7f4a_7c15, 0x2545_f491_4f6c_dd1d] {
        let (dir, log) = temp_log(&format!("top-page-{seed_value:x}"));
        let mut store = Store::create(&log).unwrap();
        let mut rng = Rng(seed_value);
        seed(&mut store, &mut rng);
        let mut compared = 0;
        for acl in acls() {
            for shape in shapes(&acl) {
                for property in ["lo", "hi", "label", "absent"] {
                    for descending in [false, true] {
                        for page_size in [6, 64, 2000, usize::MAX] {
                            let fast = EvaluateRequest {
                                sort: Some(Sort {
                                    descending,
                                    ..Sort::by(property)
                                }),
                                page_size,
                                ..shape.clone()
                            };
                            let full = EvaluateRequest {
                                then_by: vec![Sort::by("never_set")],
                                ..fast.clone()
                            };
                            let fast_pages = pages(&store, &fast);
                            assert_eq!(
                                fast_pages,
                                pages(&store, &full),
                                "seed {seed_value:x} {property} desc={descending} page={page_size}"
                            );
                            compared += fast_pages.len();
                        }
                    }
                }
            }
        }
        assert!(compared > 500, "too few pages compared: {compared}");
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn paged_single_key_sort_keeps_full_ranking_errors() {
    let (dir, log) = temp_log("top-page-errors");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store, &mut Rng(11));
    let request = EvaluateRequest {
        namespace: NS.into(),
        root_kind: "Item".into(),
        sum_kind: "Item".into(),
        sum_property: "lo".into(),
        acl: PropertyAcl::allow_all(),
        object_bound: 1000,
        sort: Some(Sort::by("lo")),
        page_size: 5,
        ..Default::default()
    };
    let bad_cursor = EvaluateRequest {
        cursor: Some("not-a-cursor".into()),
        ..request
    };
    let full_bad_cursor = EvaluateRequest {
        then_by: vec![Sort::by("never_set")],
        ..bad_cursor.clone()
    };
    let fast = LocalCompute.evaluate(&store, &bad_cursor).unwrap_err();
    assert!(matches!(fast, ComputeError::Page(_)), "{fast:?}");
    assert_eq!(
        fast,
        LocalCompute.evaluate(&store, &full_bad_cursor).unwrap_err()
    );
    drop(store);
    let _ = std::fs::remove_dir_all(dir);
}
