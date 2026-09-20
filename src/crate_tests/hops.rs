use super::super::*;
use super::helpers::*;

#[test]
fn incoming_hop_follows_join_property() {
    let (dir, log) = temp_log("incoming-hop");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    let oss = ObjectSet::new(LocalCompute);
    let outgoing = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(outgoing.two_hop_count, 1);
    assert_eq!(outgoing.sum_amount, 10);
    let from_customer = EvaluateRequest {
        root_kind: "Customer".into(),
        hops: vec![Hop {
            far_kind: "Order".into(),
            join_property: "customer_id".into(),
            incoming: false,
            predicate: None,
        }],
        sum_kind: "Order".into(),
        sum_property: "customer_id".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: None,
        object_bound: 0,
        sort: None,
        page_size: 0,
        cursor: None,
    };
    assert_eq!(
        oss.evaluate(&store, &from_customer).unwrap().two_hop_count,
        1
    );
    let incoming = EvaluateRequest {
        root_kind: "Order".into(),
        hops: vec![Hop {
            far_kind: "Customer".into(),
            join_property: "customer_id".into(),
            incoming: true,
            predicate: None,
        }],
        sum_kind: "Customer".into(),
        sum_property: "region".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: None,
        object_bound: 0,
        sort: None,
        page_size: 0,
        cursor: None,
    };
    let response = oss.evaluate(&store, &incoming).unwrap();
    assert_eq!(response.two_hop_count, 1);
    assert!(!store.joins().is_visible("Order", "o0"));
    assert!(!store.joins().is_visible("Customer", "c0"));
    let denied = EvaluateRequest {
        root_kind: "Order".into(),
        hops: vec![Hop {
            far_kind: "Customer".into(),
            join_property: "customer_id".into(),
            incoming: true,
            predicate: None,
        }],
        sum_kind: "Customer".into(),
        sum_property: "region".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::deny_property("Customer", "region"),
        filter: None,
        predicate: None,
        object_bound: 0,
        sort: None,
        page_size: 0,
        cursor: None,
    };
    assert!(matches!(
        oss.evaluate(&store, &denied),
        Err(ComputeError::Acl(AclError::Denied { .. }))
    ));
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert_eq!(oss.evaluate(&reopened, &incoming).unwrap().two_hop_count, 1);
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    assert_eq!(oss.evaluate(&replayed, &incoming).unwrap().two_hop_count, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn exact_match_filter_restricts_roots() {
    let (dir, log) = temp_log("filter-roots");
    let mut store = Store::create(&log).unwrap();
    append_all(
        &mut store,
        vec![
            rec("Customer", "c0", true, &[("region", "eu")]),
            rec("Customer", "c1", false, &[("region", "us")]),
            rec("Customer", "c2", false, &[("region", "eu")]),
            rec("Order", "o1", false, &[("customer_id", "c1")]),
            rec("Order", "o2", false, &[("customer_id", "c2")]),
            rec(
                "Shipment",
                "s1",
                false,
                &[("order_id", "o1"), ("amount", "10")],
            ),
            rec(
                "Shipment",
                "s2",
                false,
                &[("order_id", "o2"), ("amount", "7")],
            ),
        ],
    );
    let oss = ObjectSet::new(LocalCompute);
    let unfiltered = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(unfiltered.two_hop_count, 2);
    assert_eq!(unfiltered.sum_amount, 17);

    let mut us = fixture_request();
    us.filter = Some(ExactMatch {
        property: "region".into(),
        value: "us".into(),
    });
    let us_resp = oss.evaluate(&store, &us).unwrap();
    assert_eq!(us_resp.two_hop_count, 1);
    assert_eq!(us_resp.sum_amount, 10);

    let mut eu = fixture_request();
    eu.filter = Some(ExactMatch {
        property: "region".into(),
        value: "eu".into(),
    });
    let eu_resp = oss.evaluate(&store, &eu).unwrap();
    assert_eq!(eu_resp.two_hop_count, 1);
    assert_eq!(eu_resp.sum_amount, 7);
    assert!(!store.joins().is_visible("Customer", "c0"));

    let mut miss = fixture_request();
    miss.filter = Some(ExactMatch {
        property: "region".into(),
        value: "ap".into(),
    });
    let empty = oss.evaluate(&store, &miss).unwrap();
    assert_eq!(empty.two_hop_count, 0);
    assert_eq!(empty.sum_amount, 0);

    let mut denied = us.clone();
    denied.acl = PropertyAcl::deny_property("Customer", "region");
    assert!(matches!(
        oss.evaluate(&store, &denied),
        Err(ComputeError::Acl(AclError::Denied { .. }))
    ));

    drop(store);
    let reopened = Store::open(&log).unwrap();
    assert_eq!(oss.evaluate(&reopened, &us).unwrap(), us_resp);
    assert!(!reopened.joins().is_visible("Customer", "c0"));
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    assert_eq!(oss.evaluate(&replayed, &us).unwrap(), us_resp);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn exact_match_filter_uses_by_prop_among_many_roots() {
    let (dir, log) = temp_log("filter-by-prop");
    let mut store = Store::create(&log).unwrap();
    let mut records = vec![
        rec("Customer", "keep", false, &[("region", "us")]),
        rec("Order", "o1", false, &[("customer_id", "keep")]),
        rec(
            "Shipment",
            "s1",
            false,
            &[("order_id", "o1"), ("amount", "4")],
        ),
    ];
    for i in 0..64 {
        records.push(rec(
            "Customer",
            &format!("other-{i}"),
            false,
            &[("region", "eu")],
        ));
    }
    append_all(&mut store, records);
    let mut request = fixture_request();
    request.filter = Some(ExactMatch {
        property: "region".into(),
        value: "us".into(),
    });
    let response = ObjectSet::new(LocalCompute)
        .evaluate(&store, &request)
        .unwrap();
    assert_eq!(response.two_hop_count, 1);
    assert_eq!(response.sum_amount, 4);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hop_fan_out_sums_every_leaf_path() {
    let (dir, log) = temp_log("hop-fan-out");
    let mut store = Store::create(&log).unwrap();
    append_all(
        &mut store,
        vec![
            rec("Customer", "c1", false, &[("region", "us")]),
            rec("Order", "o1", false, &[("customer_id", "c1")]),
            rec(
                "Shipment",
                "s1",
                false,
                &[("order_id", "o1"), ("amount", "10")],
            ),
            rec(
                "Shipment",
                "s2",
                false,
                &[("order_id", "o1"), ("amount", "7")],
            ),
        ],
    );
    let oss = ObjectSet::new(LocalCompute);
    let response = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(response.two_hop_count, 1);
    assert_eq!(response.sum_amount, 17);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hop_diamond_counts_distinct_roots_once() {
    let (dir, log) = temp_log("hop-diamond");
    let mut store = Store::create(&log).unwrap();
    append_all(
        &mut store,
        vec![
            rec("Customer", "c1", false, &[("region", "us")]),
            rec("Order", "o1", false, &[("customer_id", "c1")]),
            rec("Order", "o2", false, &[("customer_id", "c1")]),
            rec(
                "Shipment",
                "s1",
                false,
                &[("order_id", "o1"), ("amount", "10")],
            ),
            rec(
                "Shipment",
                "s2",
                false,
                &[("order_id", "o2"), ("amount", "3")],
            ),
        ],
    );
    let oss = ObjectSet::new(LocalCompute);
    let response = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(response.two_hop_count, 1);
    assert_eq!(response.sum_amount, 13);
    let _ = std::fs::remove_dir_all(&dir);
}
