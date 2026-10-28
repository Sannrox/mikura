//! Split commit path of [`SharedStore`] (#261, ADR 0040 amendment).
//!
//! A test hook pauses the writer at a log-write stage while the test reads
//! under shared access. Every wait is bounded, so a writer that wrongly holds
//! readers out fails the test instead of hanging it.

use super::super::*;
use super::helpers::*;
use crate::crypto::{decode_catalog, LogKeys, MemoryKeyProvider, RuntimeProfile, STORE_KEY_TENANT};
use crate::log::{read_records, CommitStage};
use crate::{KeyId, KeyProvider, KeyPurpose, TenantId, UnwrapGrant};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(10);

fn shipment(key: &str, amount: &str) -> ObjectRecord {
    rec(
        "Shipment",
        key,
        false,
        &[("order_id", "o1"), ("amount", amount)],
    )
}

fn claimed(key: &str, id: &str) -> ObjectRecord {
    let mut record = shipment(key, "1");
    record.action_id = Some(id.into());
    record
}

fn seeded(name: &str) -> (std::path::PathBuf, std::path::PathBuf, SharedStore) {
    let (dir, log) = temp_log(name);
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    (dir, log, SharedStore::new(store))
}

fn visible(store: &Store, key: &str) -> bool {
    store
        .load(NS, "Shipment", key, &PropertyAcl::allow_all())
        .is_ok()
}

fn split(shared: &SharedStore, batch: Vec<ObjectRecord>) {
    match shared.append_batch(batch) {
        BatchWrite::Done(result) => result.unwrap(),
        BatchWrite::Exclusive(_) => panic!("batch took the exclusive path"),
    }
}

/// Run `batch` on another thread, pause its log write at `stage`, run
/// `during` under shared access while it is paused, then let it finish.
/// Fails unless `during` finished before the writer resumed.
fn while_paused(
    shared: &SharedStore,
    stage: CommitStage,
    batch: Vec<ObjectRecord>,
    during: impl FnOnce(&Store),
) {
    let (paused_tx, paused_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    let released = Arc::new(AtomicBool::new(false));
    let armed = AtomicBool::new(true);
    let hook_released = Arc::clone(&released);
    let hook: crate::log::StageHook = Arc::new(move |at| {
        if at == stage && armed.swap(false, Ordering::SeqCst) {
            paused_tx.send(()).unwrap();
            let rx = release_rx.lock().unwrap();
            hook_released.store(rx.recv_timeout(BOUND).is_ok(), Ordering::SeqCst);
        }
    });
    shared.write().unwrap().set_stage_hook(Some(hook));
    std::thread::scope(|scope| {
        let writer = scope.spawn(|| split(shared, batch));
        paused_rx
            .recv_timeout(BOUND)
            .expect("writer reached the stage");
        during(&shared.read().unwrap());
        release_tx.send(()).unwrap();
        writer.join().unwrap();
    });
    shared.write().unwrap().set_stage_hook(None);
    assert!(
        released.load(Ordering::SeqCst),
        "the reader waited for the paused writer"
    );
}

#[test]
fn readers_proceed_and_see_no_unsynced_record_while_a_batch_syncs() {
    let (dir, _log, shared) = seeded("split-unsynced");
    let committed = shared.read().unwrap().committed_pages();
    while_paused(
        &shared,
        CommitStage::PagesWritten,
        vec![shipment("s9", "4")],
        |store| {
            assert!(visible(store, "s1"));
            assert!(!visible(store, "s9"), "unsynced record visible");
            assert_eq!(store.committed_pages(), committed);
        },
    );
    let store = shared.read().unwrap();
    assert!(visible(&store, "s9"));
    assert!(store.committed_pages() > committed);
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn durable_batch_stays_invisible_until_applied_then_appears_whole() {
    let (dir, _log, shared) = seeded("split-atomic");
    let keys: Vec<String> = (0..64).map(|i| format!("m{i}")).collect();
    let batch = keys.iter().map(|key| shipment(key, "2")).collect();
    let before = shared.read().unwrap().committed_pages();
    while_paused(&shared, CommitStage::Durable, batch, |store| {
        assert_eq!(keys.iter().filter(|key| visible(store, key)).count(), 0);
        assert_eq!(store.committed_pages(), before);
        let history = store.history(NS, "Shipment", "m0", &PropertyAcl::allow_all());
        assert!(history.is_err(), "history read past the applied commit");
    });
    let store = shared.read().unwrap();
    assert_eq!(
        keys.iter().filter(|key| visible(&store, key)).count(),
        keys.len()
    );
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            std::fs::copy(&path, to.join(path.file_name().unwrap())).unwrap();
        }
    }
}

#[test]
fn crash_between_sync_and_apply_recovers_the_batch_once() {
    let (dir, log, shared) = seeded("split-crash");
    let crash = dir.join("crash");
    let batch: Vec<_> = (0..8)
        .map(|i| claimed(&format!("r{i}"), &format!("act-{i}")))
        .collect();
    while_paused(&shared, CommitStage::Durable, batch.clone(), |_| {
        copy_dir(&dir, &crash)
    });
    let applied = shared.read().unwrap().committed_pages();
    drop(shared);

    let crash_log = crash.join(log.file_name().unwrap());
    let mut recovered = Store::open(&crash_log).unwrap();
    assert_eq!(recovered.committed_pages(), applied);
    for record in &batch {
        let loaded = recovered
            .load(NS, "Shipment", &record.key, &PropertyAcl::allow_all())
            .unwrap();
        assert_eq!(loaded.gen, 1, "{}", record.key);
    }
    // The client never saw an ack; its retry under the same ids is a replay.
    recovered.append_batch(batch.clone()).unwrap();
    assert_eq!(recovered.committed_pages(), applied);
    drop(recovered);
    let logged = read_records(&crash_log).unwrap();
    for record in &batch {
        assert_eq!(
            logged.iter().filter(|r| r.key == record.key).count(),
            1,
            "{} logged more than once",
            record.key
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn crash_before_sync_loses_only_the_unacknowledged_batch() {
    let (dir, log, shared) = seeded("split-crash-early");
    let crash = dir.join("crash");
    let before = shared.read().unwrap().committed_pages();
    while_paused(
        &shared,
        CommitStage::PagesWritten,
        vec![shipment("r1", "3")],
        |_| copy_dir(&dir, &crash),
    );
    drop(shared);
    let recovered = Store::open(&crash.join(log.file_name().unwrap())).unwrap();
    assert_eq!(recovered.committed_pages(), before);
    assert!(visible(&recovered, "s1"));
    assert!(!visible(&recovered, "r1"));
    drop(recovered);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn concurrent_writers_serialize() {
    let (dir, _log, shared) = seeded("split-writers");
    let gen = |shared: &SharedStore| {
        shared
            .read()
            .unwrap()
            .load(NS, "Shipment", "s1", &PropertyAcl::allow_all())
            .unwrap()
            .gen
    };
    let start = gen(&shared);
    let exclusive_done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let mut second = None;
        let mut exclusive = None;
        while_paused(
            &shared,
            CommitStage::PagesWritten,
            vec![shipment("s1", "20")],
            |_| {
                second = Some(scope.spawn(|| split(&shared, vec![shipment("s1", "30")])));
                exclusive = Some(scope.spawn(|| {
                    let mut store = shared.write().unwrap();
                    store.append(shipment("s1", "40")).unwrap();
                    exclusive_done.store(true, Ordering::SeqCst);
                }));
                std::thread::sleep(Duration::from_millis(50));
                assert!(
                    !exclusive_done.load(Ordering::SeqCst),
                    "an exclusive writer ran during a split commit"
                );
            },
        );
        second.unwrap().join().unwrap();
        exclusive.unwrap().join().unwrap();
    });
    // Each writer admitted against the state the one before it applied.
    assert_eq!(gen(&shared), start + 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn batches_outside_the_split_path_fall_back_untouched() {
    let (dir, _log, shared) = seeded("split-fallback");
    let committed = shared.read().unwrap().committed_pages();
    let duplicate = vec![shipment("s1", "1"), shipment("s1", "2")];
    let schema = shipment_sum_schema().to_record(NS).unwrap();
    for batch in [duplicate, vec![schema], Vec::new()] {
        let expected = batch.len();
        match shared.append_batch(batch) {
            BatchWrite::Exclusive(records) => assert_eq!(records.len(), expected),
            BatchWrite::Done(result) => panic!("split path ran: {result:?}"),
        }
    }
    assert_eq!(shared.read().unwrap().committed_pages(), committed);
    let _ = std::fs::remove_dir_all(&dir);
}

fn grant(tenant: &str) -> UnwrapGrant {
    UnwrapGrant {
        tenant_id: TenantId::new(tenant).unwrap(),
        purpose: KeyPurpose::Store,
        expires_unix_secs: i64::MAX,
    }
}

fn tenant_rec(tenant: &str, key: &str, secret: &str) -> ObjectRecord {
    rec_in(
        &tenant_namespace(tenant, "ns").unwrap(),
        "Item",
        key,
        false,
        &[("secret", secret)],
    )
}

#[test]
fn encrypted_batches_split_until_a_key_changes() {
    let (dir, log) = temp_log("split-encrypted");
    let provider: Arc<dyn KeyProvider + Send + Sync> =
        Arc::new(MemoryKeyProvider::new(RuntimeProfile::Development).unwrap());
    let grants = || vec![grant(STORE_KEY_TENANT), grant("acme"), grant("beta")];
    let keys = LogKeys::new(Arc::clone(&provider), grants()).unwrap();
    let mut store = Store::create_with_keys(&log, keys).unwrap();
    store.append(tenant_rec("acme", "a0", "x")).unwrap();
    store.append(tenant_rec("beta", "b0", "x")).unwrap();
    let shared = SharedStore::new(store);

    split(&shared, vec![tenant_rec("acme", "a1", "split-secret")]);
    let bytes = std::fs::read(&log).unwrap();
    assert!(!bytes.windows(12).any(|w| w == b"split-secret"));

    // A tenant without a key needs a mint: exclusive path.
    assert!(matches!(
        shared.append_batch(vec![tenant_rec("gamma", "g1", "x")]),
        BatchWrite::Exclusive(_)
    ));

    // A revoked key changes the catalog: exclusive path, which refuses it.
    let mut catalog = &bytes[18..];
    let catalog = decode_catalog(&mut catalog).unwrap();
    let acme = KeyId::new(catalog.get("acme").unwrap().dek_id.as_str()).unwrap();
    provider.revoke_dek(&acme).unwrap();
    let BatchWrite::Exclusive(records) = shared.append_batch(vec![tenant_rec("acme", "a2", "x")])
    else {
        panic!("revoked tenant took the split path");
    };
    assert!(shared.write().unwrap().append_batch(records).is_err());
    // Once revalidated, another tenant splits again.
    split(&shared, vec![tenant_rec("beta", "b1", "x")]);
    drop(shared);

    let keys = LogKeys::new(provider, grants()).unwrap();
    let reopened = Store::open_with_keys(&log, keys).unwrap();
    let beta = tenant_namespace("beta", "ns").unwrap();
    assert!(reopened
        .load(&beta, "Item", "b1", &PropertyAcl::allow_all())
        .is_ok());
    drop(reopened);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn history_reads_during_a_torn_superblock_write_return_applied_history() {
    let (dir, _log, shared) = seeded("split-torn-superblock");
    split(&shared, vec![shipment("s1", "11")]);
    let expected = shared
        .read()
        .unwrap()
        .history(NS, "Shipment", "s1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(expected.len(), 2);
    while_paused(
        &shared,
        CommitStage::SuperblockTorn,
        vec![shipment("s1", "12")],
        |store| {
            for _ in 0..20 {
                let history = store
                    .history(NS, "Shipment", "s1", &PropertyAcl::allow_all())
                    .expect("history during a torn superblock");
                assert_eq!(history, expected);
            }
        },
    );
    let after = shared
        .read()
        .unwrap()
        .history(NS, "Shipment", "s1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(after.len(), 3);
    let _ = std::fs::remove_dir_all(&dir);
}
