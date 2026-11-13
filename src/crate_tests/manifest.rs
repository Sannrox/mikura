//! Swap-committed `MIKURAV4` file backend (ADR 0047, #340).

use super::super::*;
use super::helpers::*;
use crate::log::{objects_dir, read_records, LogWriter, SyncPolicy};
use std::io::Read;

fn item(key: &str, n: &str) -> ObjectRecord {
    rec("Item", key, false, &[("n", n)])
}

#[test]
fn new_log_writes_manifest_stub() {
    let (dir, log) = temp_log("v4-magic");
    let mut store = Store::create(&log).unwrap();
    store.append(item("a", "1")).unwrap();
    drop(store);
    let mut stub = [0u8; 12];
    std::fs::File::open(&log)
        .unwrap()
        .read_exact(&mut stub)
        .unwrap();
    assert_eq!(&stub[4..12], LOG_FORMAT_MANIFEST);
    assert!(objects_dir(&log).is_dir());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn put_without_swap_is_not_acked() {
    let (dir, log) = temp_log("v4-put");
    let mut store = Store::create(&log).unwrap();
    store.append(item("a", "1")).unwrap();
    store.set_crash_before_swap(true);
    let err = store.append(item("b", "2")).unwrap_err();
    assert!(err.contains("crash before swap"), "{err}");
    store.crash();
    let reopened = Store::open(&log).unwrap();
    assert!(reopened
        .load(NS, "Item", "a", &PropertyAcl::allow_all())
        .is_ok());
    assert!(reopened
        .load(NS, "Item", "b", &PropertyAcl::allow_all())
        .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn swap_then_reopen_keeps_acked_history() {
    let (dir, log) = temp_log("v4-swap");
    let mut store = Store::create(&log).unwrap();
    store.append(item("a", "1")).unwrap();
    store.append(item("b", "2")).unwrap();
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
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn displaced_writer_is_fenced() {
    let (dir, log) = temp_log("v4-fence");
    let mut first = Store::create(&log).unwrap();
    first.append(item("a", "1")).unwrap();
    first.release_writer_lock();
    let mut second = Store::open(&log).unwrap();
    second.append(item("b", "2")).unwrap();
    let err = first.append(item("c", "3")).unwrap_err();
    assert!(err.contains(LOG_FENCED), "{err}");
    drop(second);
    let reopened = Store::open(&log).unwrap();
    assert!(reopened
        .load(NS, "Item", "a", &PropertyAcl::allow_all())
        .is_ok());
    assert!(reopened
        .load(NS, "Item", "b", &PropertyAcl::allow_all())
        .is_ok());
    assert!(reopened
        .load(NS, "Item", "c", &PropertyAcl::allow_all())
        .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn migrate_superblock_log_round_trips_under_dual_read() {
    let (dir, src) = temp_log("v4-migrate-src");
    let mut writer = LogWriter::create_superblock(&src, SyncPolicy::Group(32)).unwrap();
    writer.append_record(&item("a", "1")).unwrap();
    writer.append_record(&item("b", "2")).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let dst = dir.join("dst.mikura");
    let copied = Store::migrate_to_manifest(&src, &dst).unwrap();
    assert_eq!(copied, 2);
    let source = read_records(&src).unwrap();
    let dest = read_records(&dst).unwrap();
    assert_eq!(source, dest);
    let _ = std::fs::remove_file(Store::join_map_path(&dst));
    let rebuilt = Store::open(&dst).unwrap();
    assert!(rebuilt
        .load(NS, "Item", "a", &PropertyAcl::allow_all())
        .is_ok());
    assert!(rebuilt
        .load(NS, "Item", "b", &PropertyAcl::allow_all())
        .is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn migrate_carries_the_log_checkpoint() {
    let (dir, src) = temp_log("v4-migrate-ckpt");
    let mut writer = LogWriter::create_superblock(&src, SyncPolicy::Group(32)).unwrap();
    writer.append_record(&item("a", "1")).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let mut store = Store::open(&src).unwrap();
    store.checkpoint().unwrap();
    drop(store);
    assert!(Store::checkpoint_path(&src).is_file());
    let dst = dir.join("dst.mikura");
    Store::migrate_to_manifest(&src, &dst).unwrap();
    assert!(
        Store::checkpoint_path(&dst).is_file(),
        "migrate must carry the source checkpoint"
    );
    Store::open(&dst).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn open_collects_unreferenced_segments() {
    let (dir, log) = temp_log("v4-gc");
    let mut store = Store::create(&log).unwrap();
    store.append(item("a", "1")).unwrap();
    store.set_crash_before_swap(true);
    let _ = store.append(item("b", "2"));
    store.crash();
    let before = std::fs::read_dir(objects_dir(&log)).unwrap().count();
    drop(Store::open(&log).unwrap());
    let after = std::fs::read_dir(objects_dir(&log)).unwrap().count();
    assert!(after < before, "open should collect the unacked segment");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn open_collects_a_leftover_probe() {
    let (dir, log) = temp_log("v4-probe-gc");
    let mut store = Store::create(&log).unwrap();
    store.append(item("a", "1")).unwrap();
    drop(store);
    let probe = objects_dir(&log).join("probe");
    std::fs::write(&probe, b"stale").unwrap();
    Store::open(&log).unwrap();
    assert!(
        !probe.exists(),
        "open should collect leftover probe before probing"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn leftover_segments_without_manifest_fail_closed() {
    let (dir, log) = temp_log("v4-missing-manifest");
    let mut store = Store::create(&log).unwrap();
    store.append(item("a", "1")).unwrap();
    drop(store);
    let objects = objects_dir(&log);
    let mut leftover = 0;
    for entry in std::fs::read_dir(&objects).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap();
        if name.starts_with("manifest.") {
            std::fs::remove_file(&path).unwrap();
        } else if !name.starts_with('.') {
            leftover += 1;
        }
    }
    assert!(leftover > 0, "test needs leftover durable objects");
    let err = match Store::open(&log) {
        Ok(_) => panic!("open should fail closed"),
        Err(err) => err,
    };
    assert!(err.contains("committed pages missing"), "{err}");
    let still = std::fs::read_dir(&objects)
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .ok()
                .and_then(|e| e.file_name().into_string().ok())
                .is_some_and(|name| !name.starts_with('.'))
        })
        .count();
    assert_eq!(still, leftover, "open must not collect leftover objects");
    let _ = std::fs::remove_dir_all(&dir);
}

fn tiny(i: u64) -> ObjectRecord {
    rec("Item", &format!("{i}"), false, &[("n", "1")])
}

fn commit_delta_at(objects: u64) -> crate::objects::StorageCalls {
    let (dir, log) = temp_log(&format!("v4-waves-{objects}"));
    let mut writer = LogWriter::create(&log, SyncPolicy::None).unwrap();
    writer.hold_commits(true);
    for i in 0..objects {
        writer.append_record(&tiny(i)).unwrap();
    }
    writer.hold_commits(false);
    writer.flush().unwrap();
    let before = writer.storage_calls();
    writer.append_record(&tiny(objects)).unwrap();
    writer.flush().unwrap();
    let delta = writer.storage_calls().saturating_sub(before);
    drop(writer);
    let _ = std::fs::remove_dir_all(&dir);
    delta
}

#[test]
fn data_commit_is_two_waves_at_10k_and_1m_objects() {
    let at_10k = commit_delta_at(10_000);
    let at_1m = commit_delta_at(1_000_000);
    assert_eq!(at_10k.puts, 1, "10^4 objects put wave {at_10k:?}");
    assert_eq!(at_10k.cas, 1, "10^4 objects CAS wave {at_10k:?}");
    assert_eq!(at_10k.commit_waves(), 2, "10^4 objects");
    assert_eq!(at_1m.puts, 1, "10^6 objects put wave {at_1m:?}");
    assert_eq!(at_1m.cas, 1, "10^6 objects CAS wave {at_1m:?}");
    assert_eq!(at_1m.commit_waves(), 2, "10^6 objects");
}

fn two_wave_commit(store: &crate::objects::FileStore, expected: u64, i: u64) -> u64 {
    store.put(&format!("seg.{i}"), b"x").unwrap();
    let mut snapshot = crate::objects::Manifest::empty(false);
    snapshot.checkpoint_key = "ckpt".into();
    snapshot.prefix_key = "prefix".into();
    snapshot.checkpoint_pages = 1;
    let (version, _) = store
        .cas(
            expected,
            &crate::objects::encode_manifest(&snapshot).unwrap(),
        )
        .unwrap();
    version
}

fn snapshot_open_gets(store: &crate::objects::FileStore) -> u64 {
    let before = store.storage_calls();
    let _ = store.current().unwrap();
    let _ = store.get("prefix").unwrap();
    let _ = store.get("ckpt").unwrap();
    store
        .storage_calls()
        .saturating_sub(before)
        .open_named_gets()
}

// 10^5 on-disk LogWriter commits exhaust inodes. The two-wave protocol is
// FileStore put-then-CAS; LogWriter 10^4/10^6 and the snapshot-tail test
// cover the writer path.
#[test]
fn data_commit_is_two_waves_after_100k_commits() {
    let store = crate::objects::FileStore::memory();
    store.put("prefix", b"prefix").unwrap();
    store.put("ckpt", b"ckpt").unwrap();
    let mut expected = 0;
    for i in 0..100_000 {
        expected = two_wave_commit(&store, expected, i);
    }
    let before = store.storage_calls();
    two_wave_commit(&store, expected, 100_000);
    let delta = store.storage_calls().saturating_sub(before);
    assert_eq!(delta.puts, 1, "{delta:?}");
    assert_eq!(delta.cas, 1, "{delta:?}");
    assert_eq!(delta.commit_waves(), 2);
}

#[test]
fn open_named_gets_stay_bounded_after_100k_commits() {
    let store = crate::objects::FileStore::memory();
    store.put("prefix", b"prefix").unwrap();
    store.put("ckpt", b"ckpt").unwrap();
    let expected = two_wave_commit(&store, 0, 0);
    let first = snapshot_open_gets(&store);
    let mut expected = expected;
    for i in 1..=100_000 {
        expected = two_wave_commit(&store, expected, i);
    }
    let after = snapshot_open_gets(&store);
    assert_eq!(
        first, after,
        "open named gets before and after 10^5 commits"
    );
}

#[test]
fn checkpoint_keeps_the_snapshot_from_listing_the_prefix() {
    let (dir, log) = temp_log("v4-snapshot-tail");
    let mut store = Store::create_with_sync(&log, SyncPolicy::None).unwrap();
    for i in 0..8 {
        store.append(tiny(i)).unwrap();
    }
    store.checkpoint().unwrap();
    store.append(tiny(8)).unwrap();
    drop(store);
    let objects = crate::objects::FileStore::open(&objects_dir(&log), false).unwrap();
    let (_, bytes) = objects.current().unwrap().unwrap();
    let snapshot = crate::objects::decode_manifest(&bytes).unwrap();
    assert!(
        snapshot.checkpoint_pages > 0,
        "checkpoint must be named in the snapshot"
    );
    assert!(
        !snapshot.prefix_key.is_empty(),
        "frozen segments live in the prefix object"
    );
    assert_eq!(
        snapshot.segments.len(),
        1,
        "snapshot lists the tail, not the prefix"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
