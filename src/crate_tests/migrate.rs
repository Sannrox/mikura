use super::super::*;
use super::helpers::*;

/// The fixture as a pre-namespace log, plus a record with an Action id.
fn v1_log(dir: &std::path::Path) -> (std::path::PathBuf, Vec<ObjectRecord>) {
    let mut records = fixture();
    let mut action = rec("Customer", "c2", false, &[("region", "eu")]);
    action.action_id = Some("act-1".into());
    records.push(action);
    let src = dir.join("legacy.mikura");
    crate::log::write_v1_log(&src, &records).unwrap();
    (src, records)
}

fn two_hop(namespace: &str) -> EvaluateRequest {
    EvaluateRequest {
        namespace: namespace.into(),
        ..fixture_request()
    }
}

#[test]
fn migrated_v1_log_opens_in_the_named_namespace_and_stays_isolated() {
    let (dir, dst) = temp_log("migrate-v1");
    let (src, records) = v1_log(&dir);
    let src_bytes = std::fs::read(&src).unwrap();
    let refused = Store::open(&src).err().expect("V1 open must fail closed");
    assert!(refused.contains("migrate_unnamespaced"), "{refused}");

    let count = Store::migrate_unnamespaced(&src, &dst, "legacy").unwrap();
    assert_eq!(count, records.len());
    assert_eq!(
        std::fs::read(&src).unwrap(),
        src_bytes,
        "source is read-only"
    );

    let mut store = Store::open(&dst).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let legacy = oss.evaluate(&store, &two_hop("legacy")).unwrap();
    assert_eq!((legacy.two_hop_count, legacy.sum_amount), (1, 10));
    let open = PropertyAcl::allow_all();
    let c2 = store.load("legacy", "Customer", "c2", &open).unwrap();
    assert_eq!(c2.action_id.as_deref(), Some("act-1"));
    assert!(
        store
            .load("legacy", "Customer", "c0", &open)
            .unwrap()
            .hidden
    );
    assert!(store.load(NS, "Customer", "c1", &open).is_err());

    // Same (kind, key) in a second namespace never mixes with the migrated one.
    append_all(&mut store, fixture());
    let fresh = oss.evaluate(&store, &two_hop(NS)).unwrap();
    assert_eq!((fresh.two_hop_count, fresh.sum_amount), (1, 10));
    store.hide(NS, "Shipment", "s1", &open).unwrap();
    assert_eq!(oss.evaluate(&store, &two_hop(NS)).unwrap().sum_amount, 0);
    assert_eq!(
        oss.evaluate(&store, &two_hop("legacy")).unwrap().sum_amount,
        10
    );

    drop(store);
    let reopened = Store::open(&dst).unwrap();
    assert_eq!(
        oss.evaluate(&reopened, &two_hop("legacy"))
            .unwrap()
            .sum_amount,
        10
    );
    assert_eq!(oss.evaluate(&reopened, &two_hop(NS)).unwrap().sum_amount, 0);
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(&dst)).unwrap();
    let _ = std::fs::remove_file(Store::join_delta_path(&dst));
    let rebuilt = Store::open(&dst).unwrap();
    assert_eq!(
        oss.evaluate(&rebuilt, &two_hop("legacy"))
            .unwrap()
            .sum_amount,
        10
    );
    assert_eq!(oss.evaluate(&rebuilt, &two_hop(NS)).unwrap().sum_amount, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn migration_fails_closed_without_reinterpreting() {
    let (dir, dst) = temp_log("migrate-refuse");
    let (src, _) = v1_log(&dir);
    let empty = Store::migrate_unnamespaced(&src, &dst, "").unwrap_err();
    assert!(empty.contains("namespace required"), "{empty}");
    assert!(!dst.exists());

    Store::migrate_unnamespaced(&src, &dst, "legacy").unwrap();
    let again = Store::migrate_unnamespaced(&src, &dst, "legacy").unwrap_err();
    assert!(again.contains("already exists"), "{again}");
    let stale = dir.join("stale.mikura");
    std::fs::write(Store::join_map_path(&stale), b"not ours").unwrap();
    let sidecar = Store::migrate_unnamespaced(&src, &stale, "legacy").unwrap_err();
    assert!(sidecar.contains("already exists"), "{sidecar}");
    assert!(!stale.exists());
    let other = dir.join("other.mikura");
    let v2 = Store::migrate_unnamespaced(&dst, &other, "legacy").unwrap_err();
    assert!(v2.contains("already namespaced"), "{v2}");
    assert!(!other.exists());
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().contains(".migrating"))
        .collect();
    assert!(leftovers.is_empty(), "staging files left: {leftovers:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tenant_namespace_composes_one_to_one() {
    assert_eq!(tenant_namespace("t1", "sales").unwrap(), "t1/sales");
    assert_eq!(tenant_namespace("t1", "a/b").unwrap(), "t1/a/b");
    assert!(tenant_namespace("", "sales").is_err());
    assert!(tenant_namespace("t1", "").is_err());
    assert!(tenant_namespace("t/1", "sales").is_err());
}

#[test]
fn full_page_legacy_record_fails_closed_naming_it() {
    let (dir, dst) = temp_log("migrate-full-page");
    let src = dir.join("legacy.mikura");
    // V1 body: gen 8 + hidden 1 + kind 3 + key 4 + count 2 + prop name 3 +
    // value, framed by 2, filling the 4090-byte page payload.
    let value = "x".repeat(4090 - 2 - (8 + 1 + 3 + 4 + 2 + 3 + 2));
    let record = rec("K", "k1", false, &[("p", &value)]);
    crate::log::write_v1_log(&src, std::slice::from_ref(&record)).unwrap();
    let error = Store::migrate_unnamespaced(&src, &dst, "n").unwrap_err();
    assert!(error.contains("K/k1"), "{error}");
    assert!(!dst.exists());
    let _ = std::fs::remove_dir_all(&dir);
}
