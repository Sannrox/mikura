//! Maintenance ownership and fenced-writer collection (ADR 0047 §11, #363).

use super::super::*;
use super::helpers::*;
use crate::log::{objects_dir, CommitStage};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

fn unique_log(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mikura-collect-{tag}-{}-{}",
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

fn open_objects(log: &std::path::Path) -> crate::objects::ObjectStore {
    crate::objects::ObjectStore::open(&objects_dir(log), false, &ObjectBackend::File).unwrap()
}

fn durable_names(log: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = open_objects(log)
        .list()
        .unwrap()
        .into_iter()
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    names
}

fn live_object_names(log: &std::path::Path) -> Vec<String> {
    let objects = open_objects(log);
    let (version, bytes) = objects.current().unwrap().expect("manifest");
    let mut manifest = crate::objects::decode_manifest(&bytes).unwrap();
    manifest.version = version;
    if !manifest.prefix_key.is_empty() {
        let prefix = objects.get(&manifest.prefix_key).unwrap();
        manifest.prefix_segments = crate::objects::decode_manifest(&prefix).unwrap().segments;
    }
    let mut live: Vec<String> = manifest.live_keys().into_iter().collect();
    live.sort();
    live
}

#[test]
fn collector_mid_commit_keeps_the_acked_segment() {
    let (dir, log) = unique_log("live");
    let mut store = Store::create(&log).unwrap();
    store.append(item("a", "1")).unwrap();
    let start = Arc::new(Barrier::new(2));
    let done = Arc::new(Barrier::new(2));
    let start_w = Arc::clone(&start);
    let done_w = Arc::clone(&done);
    let objects = objects_dir(&log);
    store.set_stage_hook(Some(Arc::new(move |stage| {
        if stage == CommitStage::PagesWritten {
            start_w.wait();
            done_w.wait();
        }
    })));
    let collector = thread::spawn(move || {
        start.wait();
        let store = crate::objects::FileStore::open(&objects, false).unwrap();
        let (version, bytes) = store.current().unwrap().expect("manifest");
        let mut manifest = crate::objects::decode_manifest(&bytes).unwrap();
        manifest.version = version;
        store
            .collect(&manifest.live_keys(), version, &|| Ok(true))
            .unwrap();
        done.wait();
    });
    store.append(item("b", "2")).unwrap();
    collector.join().unwrap();
    assert!(store
        .load(NS, "Item", "b", &PropertyAcl::allow_all())
        .is_ok());
    drop(store);
    let reopened = Store::open(&log).unwrap();
    assert!(reopened
        .load(NS, "Item", "b", &PropertyAcl::allow_all())
        .is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fenced_open_does_not_delete_the_winner_manifest() {
    let (dir, log) = unique_log("fence-open");
    let mut store = Store::create(&log).unwrap();
    store.append(item("a", "1")).unwrap();
    store.release_writer_lock();
    let objects = crate::objects::FileStore::open(&objects_dir(&log), false).unwrap();
    let (version, bytes) = objects.current().unwrap().expect("manifest");
    let mut snapshot = crate::objects::decode_manifest(&bytes).unwrap();
    snapshot.version = version;
    let encoded = crate::objects::encode_manifest(&snapshot).unwrap();
    let stale_live = snapshot.live_keys();
    let (won, _) = objects.cas(version, &encoded).unwrap();
    let before = objects.get(&format!("manifest.{won}")).unwrap();
    objects.collect(&stale_live, version, &|| Ok(true)).unwrap();
    assert_eq!(
        objects.get(&format!("manifest.{won}")).unwrap(),
        before,
        "a fenced collector must not drop the winner snapshot"
    );
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn collect_killed_leaves_a_store_that_opens() {
    let (dir, log) = unique_log("kill");
    let mut store = Store::create(&log).unwrap();
    store.append(item("a", "1")).unwrap();
    store.set_crash_before_swap(true);
    let _ = store.append(item("b", "2"));
    store.crash();
    let mut store = Store::open(&log).unwrap();
    let _guard = crate::fault::install(None, false);
    crate::fault::arm_next(
        crate::fault::Op::Unlink,
        crate::fault::FaultKind::FailBefore,
    );
    let err = store.collect().unwrap_err();
    assert!(
        err.contains(crate::fault::FAILURE) || err.contains(LOG_FENCED) || err.contains("unlink"),
        "{err}"
    );
    drop(_guard);
    store.append(item("c", "3")).unwrap();
    drop(store);
    let mut reopened = Store::open(&log).unwrap();
    assert!(reopened
        .load(NS, "Item", "a", &PropertyAcl::allow_all())
        .is_ok());
    assert!(reopened
        .load(NS, "Item", "b", &PropertyAcl::allow_all())
        .is_err());
    reopened.collect().unwrap();
    let live = live_object_names(&log);
    let after = durable_names(&log);
    for name in &after {
        assert!(
            live.iter().any(|live| live == name),
            "leftover {name} after collect; live={live:?} after={after:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn two_collectors_one_refuses() {
    let (dir, log) = unique_log("two");
    let mut store = Store::create(&log).unwrap();
    store.append(item("a", "1")).unwrap();
    store.release_writer_lock();
    let objects = objects_dir(&log);
    let barrier = Arc::new(Barrier::new(2));
    let run = |objects: std::path::PathBuf, barrier: Arc<Barrier>| {
        let store = crate::objects::FileStore::open(&objects, false).unwrap();
        let (version, bytes) = store.current().unwrap().expect("manifest");
        let mut manifest = crate::objects::decode_manifest(&bytes).unwrap();
        manifest.version = version;
        let encoded = crate::objects::encode_manifest(&manifest).unwrap();
        barrier.wait();
        store.cas(version, &encoded)
    };
    let a_objects = objects.clone();
    let a_barrier = Arc::clone(&barrier);
    let b_objects = objects;
    let b_barrier = barrier;
    let a = thread::spawn(move || run(a_objects, a_barrier));
    let b = thread::spawn(move || run(b_objects, b_barrier));
    let a = a.join().unwrap();
    let b = b.join().unwrap();
    let wins = u8::from(a.is_ok()) + u8::from(b.is_ok());
    let losses = u8::from(a.is_err()) + u8::from(b.is_err());
    assert_eq!((wins, losses), (1, 1), "a={a:?} b={b:?}");
    let err = a.err().or(b.err()).unwrap();
    assert!(err.contains(LOG_FENCED), "{err}");
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn collect_returns_unreferenced_storage_to_zero() {
    let (dir, log) = unique_log("empty");
    let mut store = Store::create(&log).unwrap();
    store.append(item("a", "1")).unwrap();
    store.set_crash_before_swap(true);
    let _ = store.append(item("b", "2"));
    store.crash();
    let before = durable_names(&log).len();
    let mut store = Store::open(&log).unwrap();
    store.collect().unwrap();
    let live = live_object_names(&log);
    let after = durable_names(&log);
    assert!(after.len() < before, "before={before} after={after:?}");
    assert_eq!(after, live);
    assert!(store
        .load(NS, "Item", "a", &PropertyAcl::allow_all())
        .is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn checkpoint_then_collect_keeps_committed_pages() {
    let (dir, log) = unique_log("ckpt");
    let mut store = Store::create(&log).unwrap();
    store.append(item("a", "1")).unwrap();
    store.append(item("b", "2")).unwrap();
    store.checkpoint().unwrap();
    let live = live_object_names(&log);
    assert!(
        live.iter().any(|name| name.starts_with("prefix.")),
        "checkpoint must freeze coverage into a prefix object: {live:?}"
    );
    store.collect().unwrap();
    assert!(store
        .load(NS, "Item", "a", &PropertyAcl::allow_all())
        .is_ok());
    assert!(store
        .load(NS, "Item", "b", &PropertyAcl::allow_all())
        .is_ok());
    drop(store);
    let reopened = Store::open(&log).unwrap();
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
    let after = durable_names(&log);
    let live = live_object_names(&log);
    for name in &live {
        assert!(
            after.iter().any(|have| have == name),
            "collect dropped live {name}; live={live:?} after={after:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn second_writer_open_refuses_while_collect_runs() {
    let (dir, log) = unique_log("lock");
    let mut store = Store::create(&log).unwrap();
    store.append(item("a", "1")).unwrap();
    store.collect().unwrap();
    let err = match Store::open(&log) {
        Ok(_) => panic!("second open must refuse"),
        Err(err) => err,
    };
    assert!(err.contains(LOG_IN_USE), "{err}");
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}
