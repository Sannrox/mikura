use super::super::*;
use super::helpers::*;
use crate::log::SyncPolicy;

fn claimed(key: &str, id: &str, amount: &str) -> ObjectRecord {
    let mut record = rec(
        "Shipment",
        key,
        false,
        &[("order_id", "o1"), ("amount", amount)],
    );
    record.action_id = Some(id.into());
    record
}

fn is_absent(store: &Store, kind: &str, key: &str) -> bool {
    store.load(kind, key, &PropertyAcl::allow_all()).is_err()
}

fn hops(store: &Store) -> (usize, i64) {
    let result = ObjectSet::new(LocalCompute)
        .evaluate(store, &fixture_request())
        .unwrap();
    (result.two_hop_count, result.sum_amount)
}

#[test]
fn append_batch_error_drops_the_whole_batch() {
    let (dir, log) = temp_log("batch-abort");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    store.append(claimed("s2", "act-1", "5")).unwrap();
    let before = hops(&store);
    let committed = store.committed_pages();

    let remap = claimed("s8", "act-1", "2");
    let batch = vec![rec("Shipment", "s7", false, &[("order_id", "o1")]), remap];
    let error = store.append_batch(batch).unwrap_err();
    assert!(error.contains("already committed"), "{error}");
    assert!(is_absent(&store, "Shipment", "s7"));
    assert!(is_absent(&store, "Shipment", "s8"));
    assert_eq!(hops(&store), before);

    store.commit().unwrap();
    assert_eq!(store.committed_pages(), committed);
    store
        .append_batch(vec![claimed("s9", "act-2", "3")])
        .unwrap();
    assert!(!is_absent(&store, "Shipment", "s9"));
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert!(is_absent(&reopened, "Shipment", "s7"));
    assert!(is_absent(&reopened, "Shipment", "s8"));
    assert!(!is_absent(&reopened, "Shipment", "s9"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn append_batch_error_keeps_an_earlier_uncommitted_tail() {
    let (dir, log) = temp_log("batch-abort-tail");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    store.append(claimed("s2", "act-1", "5")).unwrap();

    store
        .append_uncommitted(rec("Shipment", "s6", false, &[("order_id", "o1")]))
        .unwrap();
    let batch = vec![
        rec("Shipment", "s7", false, &[("order_id", "o1")]),
        claimed("s8", "act-1", "2"),
    ];
    store.append_batch(batch).unwrap_err();
    assert!(!is_absent(&store, "Shipment", "s6"));
    assert!(is_absent(&store, "Shipment", "s7"));
    store.commit().unwrap();
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert!(!is_absent(&reopened, "Shipment", "s6"));
    assert!(is_absent(&reopened, "Shipment", "s7"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn append_batch_never_group_commits_a_partial_batch() {
    let (dir, log) = temp_log("batch-abort-large");
    let mut store = Store::create_with_sync(&log, SyncPolicy::Group(2)).unwrap();
    append_all(&mut store, fixture());
    store.append(claimed("s2", "act-1", "5")).unwrap();
    let committed = store.committed_pages();

    let payload = "x".repeat(1500);
    let mut batch: Vec<ObjectRecord> = (0..12)
        .map(|i| rec("Item", &format!("k{i:02}"), false, &[("payload", &payload)]))
        .collect();
    batch.push(claimed("s8", "act-1", "2"));
    store.append_batch(batch).unwrap_err();
    assert_eq!(store.committed_pages(), committed);
    assert!(is_absent(&store, "Item", "k00"));
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert!(is_absent(&reopened, "Item", "k00"));
    assert!(is_absent(&reopened, "Item", "k11"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn append_batch_commits_a_valid_batch_once() {
    let (dir, log) = temp_log("batch-ok");
    let mut store = Store::create_with_sync(&log, SyncPolicy::Group(2)).unwrap();
    let payload = "x".repeat(1500);
    let batch: Vec<ObjectRecord> = (0..12)
        .map(|i| rec("Item", &format!("k{i:02}"), false, &[("payload", &payload)]))
        .collect();
    store.append_batch(batch).unwrap();
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert!(!is_absent(&reopened, "Item", "k00"));
    assert!(!is_absent(&reopened, "Item", "k11"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn append_batch_reports_a_sidecar_failure_after_the_log_commit() {
    let (dir, log) = temp_log("batch-sidecar-fail");
    let mut store = Store::create(&log).unwrap();
    // A directory where the sidecar temp file goes makes persisting fail.
    let blocker = Store::join_map_path(&log).with_extension("tmp");
    std::fs::create_dir(&blocker).unwrap();
    let error = store
        .append_batch(vec![claimed("s2", "act-1", "5")])
        .unwrap_err();
    assert!(error.contains("committed to the log"), "{error}");
    assert!(!is_absent(&store, "Shipment", "s2"));
    drop(store);

    std::fs::remove_dir(&blocker).unwrap();
    let reopened = Store::open(&log).unwrap();
    assert!(!is_absent(&reopened, "Shipment", "s2"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_rollback_poisons_the_store_instead_of_committing_a_broken_projection() {
    let (dir, log) = temp_log("batch-rollback-fail");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    store.append(claimed("s2", "act-1", "5")).unwrap();
    // The rollback reloads the projection from the sidecar; a corrupt one
    // makes that reload fail.
    std::fs::write(Store::join_map_path(&log), b"not a sidecar").unwrap();

    let batch = vec![
        rec("Shipment", "s7", false, &[("order_id", "o1")]),
        claimed("s8", "act-1", "2"),
    ];
    let error = store.append_batch(batch).unwrap_err();
    assert!(error.contains("rollback failed"), "{error}");
    let refused = store
        .append(rec("Shipment", "s9", false, &[("order_id", "o1")]))
        .unwrap_err();
    assert!(refused.contains("uncertain"), "{refused}");
    let refused = store.commit().unwrap_err();
    assert!(refused.contains("uncertain"), "{refused}");
    drop(store);

    let _ = std::fs::remove_file(Store::join_map_path(&log));
    let _ = std::fs::remove_file(Store::join_delta_path(&log));
    let reopened = Store::open(&log).unwrap();
    assert!(!is_absent(&reopened, "Shipment", "s2"));
    assert!(is_absent(&reopened, "Shipment", "s7"));
    assert!(is_absent(&reopened, "Shipment", "s9"));
    assert_eq!(hops(&reopened).0, 1);
    let _ = std::fs::remove_dir_all(&dir);
}
