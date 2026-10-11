use super::super::*;
use super::helpers::*;

const REGIONS: [&str; 3] = ["eu", "us", "apac"];

/// Customers, orders and shipments across regions. Shipments carry their
/// own `region` so a filter can target only the last hop.
fn regional_fixture() -> Vec<ObjectRecord> {
    let mut records = Vec::new();
    for c in 0..9 {
        let region = REGIONS[c % 3];
        records.push(rec(
            "Customer",
            &format!("c{c}"),
            false,
            &[("region", region)],
        ));
        for o in 0..2 {
            let order = format!("o{c}-{o}");
            let customer = format!("c{c}");
            records.push(rec("Order", &order, false, &[("customer_id", &customer)]));
            let ship_region = REGIONS[(c + o) % 3];
            let amount = (c * 10 + o + 1).to_string();
            records.push(rec(
                "Shipment",
                &format!("s{c}-{o}"),
                false,
                &[
                    ("order_id", &order),
                    ("amount", &amount),
                    ("region", ship_region),
                ],
            ));
        }
    }
    records
}

/// The same view as `region in allowed` on `kind`, enumerated as hides.
fn as_hide_identities(records: &[ObjectRecord], kind: &str, allowed: &[&str]) -> PropertyAcl {
    let mut acl = PropertyAcl::allow_all();
    for record in records.iter().filter(|r| r.kind == kind) {
        let region = record.props.get("region").map(String::as_str);
        if !region.is_some_and(|r| allowed.contains(&r)) {
            acl.insert_hide_identity(kind, &record.key).unwrap();
        }
    }
    acl
}

fn row_filter(kind: &str, allowed: &[&str]) -> PropertyAcl {
    let mut acl = PropertyAcl::allow_all();
    acl.insert_row_filter(kind, "region", allowed).unwrap();
    acl
}

fn with_acl(mut request: EvaluateRequest, acl: &PropertyAcl, bound: usize) -> EvaluateRequest {
    request.acl = acl.clone();
    request.object_bound = bound;
    request
}

fn root_list() -> EvaluateRequest {
    EvaluateRequest {
        hops: Vec::new(),
        sum_kind: "Customer".into(),
        sum_property: "region".into(),
        ..fixture_request()
    }
}

fn answer(store: &Store, request: &EvaluateRequest) -> (usize, i64, Vec<String>) {
    let response = ObjectSet::new(LocalCompute)
        .evaluate(store, request)
        .unwrap();
    let keys = response.objects.into_iter().map(|o| o.key).collect();
    (response.two_hop_count, response.sum_amount, keys)
}

#[test]
fn row_filter_answers_equal_enumerated_hide_identities() {
    let (dir, log) = temp_log("row-filter-equivalence");
    let mut store = Store::create(&log).unwrap();
    let records = regional_fixture();
    append_all(&mut store, records.clone());
    for allowed in [&["eu"][..], &["eu", "apac"], &["none"]] {
        let filtered = row_filter("Customer", allowed);
        let enumerated = as_hide_identities(&records, "Customer", allowed);
        for request in [root_list(), fixture_request()] {
            let by_filter = answer(&store, &with_acl(request.clone(), &filtered, 64));
            let by_hide = answer(&store, &with_acl(request.clone(), &enumerated, 64));
            assert_eq!(by_filter, by_hide, "allowed {allowed:?}");
            let mut by_predicate = with_acl(request, &PropertyAcl::allow_all(), 64);
            by_predicate.predicate = Some(Predicate::or(
                allowed
                    .iter()
                    .map(|v| Predicate::eq("region", *v))
                    .collect(),
            ));
            assert_eq!(
                by_filter,
                answer(&store, &by_predicate),
                "allowed {allowed:?}"
            );
        }
    }
    let eu = answer(
        &store,
        &with_acl(fixture_request(), &row_filter("Customer", &["eu"]), 0),
    );
    assert_eq!((eu.0, eu.1), (3, 1 + 2 + 31 + 32 + 61 + 62));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn row_filter_on_hop_target_only_drops_its_contribution() {
    let (dir, log) = temp_log("row-filter-hop");
    let mut store = Store::create(&log).unwrap();
    let records = regional_fixture();
    append_all(&mut store, records.clone());
    let filtered = row_filter("Shipment", &["us"]);
    let enumerated = as_hide_identities(&records, "Shipment", &["us"]);
    let by_filter = answer(&store, &with_acl(fixture_request(), &filtered, 64));
    let by_hide = answer(&store, &with_acl(fixture_request(), &enumerated, 64));
    assert_eq!(by_filter, by_hide);
    // Roots stay unfiltered; only paths ending in a `us` shipment survive.
    let us: Vec<&ObjectRecord> = records
        .iter()
        .filter(|r| r.kind == "Shipment" && r.props["region"] == "us")
        .collect();
    let mut expected: Vec<String> = us.iter().map(|r| r.key.clone()).collect();
    expected.sort();
    let mut keys = by_filter.2.clone();
    keys.sort();
    assert_eq!(keys, expected);
    let sum: i64 = us
        .iter()
        .map(|r| r.props["amount"].parse::<i64>().unwrap())
        .sum();
    assert_eq!(by_filter.1, sum);
    assert_eq!(by_filter.0, 6, "customers reaching a us shipment");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn row_filter_load_matches_missing_and_mutations_fail_closed() {
    let (dir, log) = temp_log("row-filter-load");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, regional_fixture());
    let eu = row_filter("Customer", &["eu"]);
    assert!(store.load(NS, "Customer", "c0", &eu).is_ok());
    let filtered = store.load(NS, "Customer", "c1", &eu).unwrap_err();
    let missing = store.load(NS, "Customer", "c-none", &eu).unwrap_err();
    assert_eq!(filtered, missing.replace("c-none", "c1"));
    // Other kinds are untouched by a Customer filter.
    assert!(store.load(NS, "Order", "o1-0", &eu).is_ok());

    let overlay = store
        .apply_overlay_in_view(
            OverlayPatch {
                namespace: NS.into(),
                kind: "Customer".into(),
                key: "c1".into(),
                props: HashMap::from([("note".into(), "x".into())]),
                cleared: Vec::new(),
                action_id: None,
                ..Default::default()
            },
            "act-row".into(),
            None,
            &eu,
        )
        .unwrap_err();
    assert!(overlay.contains("not in this view"), "{overlay}");
    let hide = store.hide(NS, "Customer", "c1", &eu).unwrap_err();
    assert!(hide.contains("not in this view"), "{hide}");
    assert!(
        !store
            .load(NS, "Customer", "c1", &PropertyAcl::allow_all())
            .unwrap()
            .hidden
    );

    // Same view from the persisted sidecar and from a log rebuild.
    let before = answer(&store, &with_acl(fixture_request(), &eu, 64));
    drop(store);
    let reopened = Store::open(&log).unwrap();
    assert_eq!(
        answer(&reopened, &with_acl(fixture_request(), &eu, 64)),
        before
    );
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let rebuilt = Store::open(&log).unwrap();
    assert_eq!(
        answer(&rebuilt, &with_acl(fixture_request(), &eu, 64)),
        before
    );
    assert!(rebuilt.load(NS, "Customer", "c1", &eu).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn row_filter_fails_closed_on_denied_or_malformed_terms() {
    let mut acl = PropertyAcl::deny_property("Customer", "region");
    assert!(acl.insert_row_filter("Customer", "region", ["eu"]).is_err());
    let mut acl = row_filter("Customer", &["eu"]);
    assert!(acl.insert_deny("Customer", "region").is_err());
    assert!(acl.insert_row_filter("Customer", "region", ["us"]).is_err());
    let none: [&str; 0] = [];
    assert!(acl.insert_row_filter("Customer", "tier", none).is_err());
    assert!(acl
        .insert_row_filter("Customer", "tier", ["a", "a"])
        .is_err());
    assert!(acl.insert_row_filter("Customer", "tier", [""]).is_err());
    assert!(acl.insert_row_filter("", "tier", ["a"]).is_err());
    // A conjunction: both terms must hold.
    acl.insert_row_filter("Customer", "tier", ["gold"]).unwrap();
    let props = HashMap::from([("region", "eu"), ("tier", "gold")]);
    assert!(acl.object_visible("Customer", "c", |p| props.get(p).copied()));
    let props = HashMap::from([("region", "eu")]);
    assert!(!acl.object_visible("Customer", "c", |p| props.get(p).copied()));
}

#[test]
fn row_filter_cursor_binds_allowed_values() {
    let (dir, log) = temp_log("row-filter-cursor");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, regional_fixture());
    let oss = ObjectSet::new(LocalCompute);
    let mut first = with_acl(root_list(), &row_filter("Customer", &["eu", "us"]), 64);
    first.sort = Some(Sort::by("region"));
    first.page_size = 2;
    let cursor = oss
        .evaluate(&store, &first)
        .unwrap()
        .cursor
        .expect("cursor");
    let mut next = first.clone();
    next.cursor = Some(cursor.clone());
    assert!(oss.evaluate(&store, &next).is_ok());
    let mut widened = with_acl(first, &row_filter("Customer", &["eu", "us", "apac"]), 64);
    widened.cursor = Some(cursor);
    assert!(matches!(
        oss.evaluate(&store, &widened),
        Err(ComputeError::Page(_))
    ));
    let _ = std::fs::remove_dir_all(&dir);
}
