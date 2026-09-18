use super::super::*;
use super::helpers::*;

#[test]
fn load_current_object_after_reopen() {
    let (dir, log) = temp_log("load-reopen");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    let live_visible = store
        .load("Customer", "c1", &PropertyAcl::allow_all())
        .unwrap();
    let live_hidden = store
        .load("Customer", "c0", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(live_visible.gen, 1);
    assert!(!live_visible.hidden);
    assert_eq!(
        live_visible.props.get("region").map(String::as_str),
        Some("us")
    );
    assert_eq!(live_hidden.gen, 1);
    assert!(live_hidden.hidden);
    assert_eq!(
        live_hidden.props.get("region").map(String::as_str),
        Some("eu")
    );
    assert!(!store.joins().is_visible("Customer", "c0"));
    let missing = store
        .load("Customer", "missing", &PropertyAcl::allow_all())
        .unwrap_err();
    assert!(missing.contains("unknown identity"), "{missing}");
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert_eq!(
        reopened
            .load("Customer", "c1", &PropertyAcl::allow_all())
            .unwrap(),
        live_visible
    );
    assert_eq!(
        reopened
            .load("Customer", "c0", &PropertyAcl::allow_all())
            .unwrap(),
        live_hidden
    );
    assert!(!reopened.joins().is_visible("Customer", "c0"));
    drop(reopened);

    let mut store = Store::open(&log).unwrap();
    store
        .append(rec("Customer", "c1", false, &[("region", "ap")]))
        .unwrap();
    drop(store);
    let updated = Store::open(&log).unwrap();
    let latest = updated
        .load("Customer", "c1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(latest.gen, 2);
    assert_eq!(latest.props.get("region").map(String::as_str), Some("ap"));

    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    assert_eq!(
        replayed
            .load("Customer", "c1", &PropertyAcl::allow_all())
            .unwrap(),
        latest
    );
    assert_eq!(
        replayed
            .load("Customer", "c0", &PropertyAcl::allow_all())
            .unwrap(),
        live_hidden
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn load_omits_denied_properties() {
    let (dir, log) = temp_log("load-acl");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    let allow = PropertyAcl::allow_all();
    let live = store.load("Shipment", "s1", &allow).unwrap();
    assert_eq!(live.props.get("amount").map(String::as_str), Some("10"));
    assert_eq!(live.props.get("order_id").map(String::as_str), Some("o1"));

    let deny_unknown = PropertyAcl::deny_property("Shipment", "not_a_column");
    assert_eq!(
        store.load("Shipment", "s1", &deny_unknown).unwrap().props,
        live.props
    );

    let deny_amount = PropertyAcl::deny_property("Shipment", "amount");
    let redacted = store.load("Shipment", "s1", &deny_amount).unwrap();
    assert!(!redacted.props.contains_key("amount"));
    assert_eq!(
        redacted.props.get("order_id").map(String::as_str),
        Some("o1")
    );
    assert!(!redacted.props.contains_key("fabricated"));

    let hidden = store
        .load(
            "Customer",
            "c0",
            &PropertyAcl::deny_property("Customer", "region"),
        )
        .unwrap();
    assert!(hidden.hidden);
    assert!(!hidden.props.contains_key("region"));
    assert!(!store.joins().is_visible("Customer", "c0"));

    let oss = ObjectSet::new(LocalCompute);
    let mut denied_req = fixture_request();
    denied_req.acl = PropertyAcl::deny_property("Shipment", "amount");
    assert!(matches!(
        oss.evaluate(&store, &denied_req),
        Err(ComputeError::Acl(AclError::Denied { .. }))
    ));
    drop(store);

    let reopened = Store::open(&log).unwrap();
    let after_open = reopened.load("Shipment", "s1", &deny_amount).unwrap();
    assert!(!after_open.props.contains_key("amount"));
    assert_eq!(
        after_open.props.get("order_id").map(String::as_str),
        Some("o1")
    );
    assert_eq!(
        reopened.load("Shipment", "s1", &allow).unwrap().props,
        live.props
    );
    drop(reopened);

    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    let after_replay = replayed.load("Shipment", "s1", &deny_amount).unwrap();
    assert!(!after_replay.props.contains_key("amount"));
    assert_eq!(
        after_replay.props.get("order_id").map(String::as_str),
        Some("o1")
    );
    let _ = std::fs::remove_dir_all(&dir);
}
