//! Replacement projections are published beside the serving sidecar (#359).
//!
//! A compact or budgeted republish writes `{log}.joins.tmp` then renames.
//! A failed write leaves the previous file serving. After a successful
//! switch the temp file is gone. Concurrent readers see only a complete
//! old or new answer, never a mix.

use super::super::*;
use super::helpers::*;
use crate::fault::{arm_next, install, FaultKind, Op};
use crate::log::CommitStage;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn unique_log(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mikura-proj-switch-{tag}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("objects.mikura");
    (dir, log)
}

fn sidecar_magic(log: &std::path::Path) -> [u8; 8] {
    let mut magic = [0u8; 8];
    std::fs::File::open(Store::join_map_path(log))
        .unwrap()
        .read_exact(&mut magic)
        .unwrap();
    magic
}

fn shipment_amount(store: &Store) -> i64 {
    ObjectSet::new(LocalCompute)
        .evaluate(store, &fixture_request())
        .unwrap()
        .sum_amount
}

fn load_amount(store: &Store, key: &str) -> Option<String> {
    store
        .load(NS, "Shipment", key, &PropertyAcl::allow_all())
        .ok()
        .and_then(|row| row.props.get("amount").cloned())
}

#[test]
fn failed_replacement_write_leaves_serving_sidecar_untouched() {
    let (dir, log) = unique_log("fail-before");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    let before = std::fs::read(Store::join_map_path(&log)).unwrap();
    assert_eq!(&before[..8], JOIN_FORMAT.as_slice());
    let live = shipment_amount(&store);
    store.set_sidecar_persist(1, Duration::from_secs(0));
    store.set_join_delta_compact_bytes(0);
    let _guard = install(None, false);
    store.set_stage_hook(Some(Arc::new(|at| {
        if at == CommitStage::Durable {
            arm_next(Op::Rename, FaultKind::FailBefore);
        }
    })));
    let err = store
        .append(rec(
            "Shipment",
            "s2",
            false,
            &[("order_id", "o1"), ("amount", "5")],
        ))
        .unwrap_err();
    assert!(
        err.contains("projection sidecar not persisted") || err.contains(crate::fault::FAILURE),
        "{err}"
    );
    let after = std::fs::read(Store::join_map_path(&log)).unwrap();
    assert_eq!(after, before, "serving sidecar must stay the previous file");
    assert!(!Store::join_map_path(&log).with_extension("tmp").exists());
    assert_eq!(shipment_amount(&store), live + 5);
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert_eq!(load_amount(&reopened, "s1").as_deref(), Some("10"));
    assert_eq!(load_amount(&reopened, "s2").as_deref(), Some("5"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn kill_during_replacement_write_reopens_old_or_new() {
    let (dir, log) = unique_log("kill");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    store.set_sidecar_persist(1, Duration::from_secs(0));
    store.set_join_delta_compact_bytes(0);
    let _guard = install(None, false);
    store.set_stage_hook(Some(Arc::new(|at| {
        if at == CommitStage::Durable {
            arm_next(Op::Write, FaultKind::Crash);
        }
    })));
    let err = store
        .append(rec(
            "Shipment",
            "s2",
            false,
            &[("order_id", "o1"), ("amount", "5")],
        ))
        .unwrap_err();
    assert!(err.contains(crate::fault::CRASH), "{err}");
    crate::fault::allow_recovery();
    store.crash();

    let sidecar = Store::join_map_path(&log);
    if sidecar.is_file() {
        let bytes = std::fs::read(&sidecar).unwrap();
        assert!(
            bytes.len() >= 12 && (&bytes[..8] == JOIN_FORMAT || &bytes[..8] == JOIN_FORMAT_PAGED),
            "serving sidecar must be a complete file, got {} bytes",
            bytes.len()
        );
    }
    assert!(!sidecar.with_extension("tmp").exists());
    let reopened = Store::open(&log).unwrap();
    assert_eq!(load_amount(&reopened, "s1").as_deref(), Some("10"));
    assert_eq!(load_amount(&reopened, "s2").as_deref(), Some("5"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn budgeted_republish_switches_and_releases_the_temp_file() {
    let (dir, log) = unique_log("paged");
    let budget = ServingBudget::from_bytes(1024 * 1024).unwrap();
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    drop(store);
    let mut store = Store::open_with_serving_budget(&log, budget).unwrap();
    assert_eq!(sidecar_magic(&log), *JOIN_FORMAT_PAGED);
    store.set_sidecar_persist(1, Duration::from_secs(0));
    store
        .append(rec(
            "Shipment",
            "s2",
            false,
            &[("order_id", "o1"), ("amount", "5")],
        ))
        .unwrap();
    assert_eq!(sidecar_magic(&log), *JOIN_FORMAT_PAGED);
    assert!(!Store::join_map_path(&log).with_extension("tmp").exists());
    assert_eq!(load_amount(&store, "s2").as_deref(), Some("5"));
    drop(store);

    let reopened = Store::open_with_serving_budget(&log, budget).unwrap();
    assert_eq!(shipment_amount(&reopened), 15);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn readers_during_checkpoint_rewrite_see_only_complete_answers() {
    let (dir, log) = unique_log("readers");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    store.set_sidecar_persist(1, Duration::from_secs(0));
    store.set_join_delta_compact_bytes(0);
    let shared = SharedStore::new(store);
    let stop = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let bound = Duration::from_secs(10);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                let started = Instant::now();
                while !stop.load(Ordering::SeqCst) && started.elapsed() < bound {
                    let store = shared.read().unwrap();
                    let amount = shipment_amount(&store);
                    seen.lock().unwrap().push(amount);
                }
            });
        }
        match shared.append_batch(vec![
            rec(
                "Shipment",
                "s2",
                false,
                &[("order_id", "o1"), ("amount", "5")],
            ),
            rec(
                "Shipment",
                "s3",
                false,
                &[("order_id", "o1"), ("amount", "1")],
            ),
        ]) {
            BatchWrite::Done(result) => result.unwrap(),
            BatchWrite::Exclusive(_) => panic!("batch took the exclusive path"),
        }
        stop.store(true, Ordering::SeqCst);
    });
    let amounts = seen.lock().unwrap().clone();
    assert!(!amounts.is_empty());
    assert!(
        amounts.iter().all(|amount| *amount == 10 || *amount == 16),
        "torn projection sums: {amounts:?}"
    );
    assert!(
        amounts.contains(&16),
        "readers never observed the replacement: {amounts:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
