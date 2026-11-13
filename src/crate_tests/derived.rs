//! Derived checkpoint and projection objects for takeover (ADR 0047 §9, #361).

use super::super::*;
use super::helpers::*;
use crate::log::objects_dir;
use std::sync::atomic::{AtomicU64, Ordering};

fn unique_log(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mikura-derived-{tag}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("objects.mikura");
    (dir, log)
}

fn item(key: &str, n: &str) -> ObjectRecord {
    rec("Item", key, false, &[("n", n)])
}

fn evaluate_sum(store: &Store) -> i64 {
    ObjectSet::new(LocalCompute)
        .evaluate(store, &fixture_request())
        .unwrap()
        .sum_amount
}

fn remove_local_derived(log: &std::path::Path) {
    let _ = std::fs::remove_file(Store::join_map_path(log));
    let _ = std::fs::remove_file(Store::join_delta_path(log));
    let _ = std::fs::remove_file(Store::checkpoint_path(log));
    let _ = std::fs::remove_file(Store::checkpoint_prev_path(log));
}

fn current_snapshot(
    log: &std::path::Path,
) -> (crate::objects::FileStore, crate::objects::Manifest) {
    let objects = crate::objects::FileStore::open(&objects_dir(log), false).unwrap();
    let (version, bytes) = objects.current().unwrap().expect("manifest");
    let mut manifest = crate::objects::decode_manifest(&bytes).unwrap();
    manifest.version = version;
    (objects, manifest)
}

fn delete_named_derived(log: &std::path::Path) {
    let (objects, snapshot) = current_snapshot(log);
    for key in [&snapshot.checkpoint_key, &snapshot.projection_key] {
        if key.is_empty() {
            continue;
        }
        let path = objects_dir(log).join(key);
        let _ = std::fs::remove_file(&path);
        assert!(
            objects.get(key).is_err(),
            "derived object {key} must be gone"
        );
    }
}

#[test]
fn checkpoint_names_derived_objects_in_the_snapshot() {
    let (dir, log) = unique_log("named");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    store.checkpoint().unwrap();
    drop(store);

    let (_, snapshot) = current_snapshot(&log);
    assert!(
        snapshot.checkpoint_key.starts_with("ckpt."),
        "checkpoint key: {}",
        snapshot.checkpoint_key
    );
    assert!(
        snapshot.projection_key.starts_with("proj."),
        "projection key: {}",
        snapshot.projection_key
    );
    assert!(crate::objects::derived_covers(
        &snapshot.checkpoint_key,
        snapshot.version,
        snapshot.checkpoint_pages
    ));
    assert!(crate::objects::derived_covers(
        &snapshot.projection_key,
        snapshot.version,
        snapshot.checkpoint_pages
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn empty_machine_open_loads_derived_objects_and_replays_the_tail() {
    let (dir, log) = unique_log("takeover");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    store.checkpoint().unwrap();
    store
        .append(rec("Item", "tail", false, &[("n", "1")]))
        .unwrap();
    let live = evaluate_sum(&store);
    drop(store);

    remove_local_derived(&log);
    crate::log::take_pages_decoded();
    let reopened = Store::open(&log).unwrap();
    let decoded = crate::log::take_pages_decoded();
    assert_eq!(
        decoded, 1,
        "derived objects must replay only the tail, decoded={decoded}"
    );
    assert_eq!(evaluate_sum(&reopened), live);
    assert_eq!(
        reopened
            .load(NS, "Item", "tail", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("n")
            .map(String::as_str),
        Some("1")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deleting_derived_objects_rebuilds_identical_answers() {
    let (dir, log) = unique_log("delete");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    store.checkpoint().unwrap();
    let live = evaluate_sum(&store);
    drop(store);

    remove_local_derived(&log);
    delete_named_derived(&log);
    crate::log::take_pages_decoded();
    let reopened = Store::open(&log).unwrap();
    assert!(
        crate::log::take_pages_decoded() > 0,
        "missing derived objects must replay the log"
    );
    assert_eq!(evaluate_sum(&reopened), live);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn corrupt_derived_projection_is_skipped() {
    let (dir, log) = unique_log("corrupt");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    store.checkpoint().unwrap();
    let live = evaluate_sum(&store);
    drop(store);

    let (_, snapshot) = current_snapshot(&log);
    let path = objects_dir(&log).join(&snapshot.projection_key);
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 5;
    bytes[last] ^= 0x01;
    std::fs::write(&path, bytes).unwrap();
    remove_local_derived(&log);

    crate::log::take_pages_decoded();
    let reopened = Store::open(&log).unwrap();
    assert_eq!(evaluate_sum(&reopened), live);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stale_derived_projection_is_skipped() {
    let (dir, log) = unique_log("stale");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    store.checkpoint().unwrap();
    let live = evaluate_sum(&store);
    drop(store);

    let (dir2, log2) = unique_log("stale-other");
    let mut other = Store::create(&log2).unwrap();
    other.append(item("only", "1")).unwrap();
    other.checkpoint().unwrap();
    drop(other);
    let (_, other_snap) = current_snapshot(&log2);
    let foreign = std::fs::read(objects_dir(&log2).join(&other_snap.projection_key)).unwrap();

    let (_, snapshot) = current_snapshot(&log);
    std::fs::write(objects_dir(&log).join(&snapshot.projection_key), foreign).unwrap();
    remove_local_derived(&log);

    let reopened = Store::open(&log).unwrap();
    assert_eq!(evaluate_sum(&reopened), live);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

#[test]
fn http_empty_machine_open_loads_derived_objects() {
    let server = LocalObjectServer::start().unwrap();
    let backend = ObjectBackend::http(server.endpoint(), None).unwrap();
    let (dir, log) = unique_log("http");
    let mut store = Store::create_with(&log, backend.clone()).unwrap();
    store.append(item("a", "1")).unwrap();
    store.append(item("b", "2")).unwrap();
    store.checkpoint().unwrap();
    drop(store);

    let (dir2, log2) = unique_log("http-empty");
    std::fs::copy(&log, &log2).unwrap();
    std::fs::copy(objects_dir(&log), objects_dir(&log2)).unwrap();
    assert!(!Store::join_map_path(&log2).exists());
    assert!(!Store::checkpoint_path(&log2).exists());

    let reopened = Store::open(&log2).unwrap();
    assert_eq!(
        reopened
            .load(NS, "Item", "a", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("n")
            .map(String::as_str),
        Some("1")
    );
    assert_eq!(
        reopened
            .load(NS, "Item", "b", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("n")
            .map(String::as_str),
        Some("2")
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}
