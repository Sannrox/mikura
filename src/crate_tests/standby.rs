//! Read-only standby, minimum-version wait, and promote/fence (ADR 0047 §5, #362).

use super::super::*;
use super::helpers::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static NEXT: AtomicU64 = AtomicU64::new(1);

fn item(key: &str, n: &str) -> ObjectRecord {
    rec("Item", key, false, &[("n", n)])
}

fn unique_log(label: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "mikura-standby-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("objects.mikura");
    (dir, log)
}

fn n_of(store: &Store, key: &str) -> String {
    store
        .load(NS, "Item", key, &PropertyAcl::allow_all())
        .unwrap()
        .props
        .get("n")
        .cloned()
        .unwrap()
}

#[test]
fn read_only_open_does_not_need_the_writer_lock() {
    let (dir, log) = unique_log("lock");
    let mut writer = Store::create(&log).unwrap();
    writer.append(item("a", "1")).unwrap();
    let standby = Store::open_read_only(&log).unwrap();
    assert!(standby.is_read_only());
    assert!(!standby.is_fenced());
    assert_eq!(n_of(&standby, "a"), "1");
    let err = match Store::open(&log) {
        Ok(_) => panic!("second writer must fail while the first holds the log"),
        Err(err) => err,
    };
    assert!(err.contains(LOG_IN_USE), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn read_only_store_refuses_writes() {
    let (dir, log) = unique_log("nowrite");
    let mut writer = Store::create(&log).unwrap();
    writer.append(item("a", "1")).unwrap();
    let mut standby = Store::open_read_only(&log).unwrap();
    let err = standby.append(item("b", "2")).unwrap_err();
    assert!(err.contains("read-only"), "{err}");
    assert!(Store::open_read_only(&log)
        .unwrap()
        .load(NS, "Item", "b", &PropertyAcl::allow_all())
        .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn follow_applies_a_later_writer_commit() {
    let (dir, log) = unique_log("follow");
    let mut writer = Store::create(&log).unwrap();
    writer.append(item("a", "1")).unwrap();
    let mut standby = Store::open_read_only(&log).unwrap();
    let served = standby.manifest_version();
    writer.append(item("b", "2")).unwrap();
    assert!(standby
        .load(NS, "Item", "b", &PropertyAcl::allow_all())
        .is_err());
    standby.follow().unwrap();
    assert!(standby.manifest_version() > served);
    assert_eq!(n_of(&standby, "b"), "2");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wait_for_version_refuses_below_the_named_snapshot() {
    let (dir, log) = unique_log("min-miss");
    let mut writer = Store::create(&log).unwrap();
    writer.append(item("a", "1")).unwrap();
    let mut standby = Store::open_read_only(&log).unwrap();
    let want = standby.manifest_version() + 8;
    let err = standby
        .wait_for_version(want, Duration::from_millis(40))
        .unwrap_err();
    assert!(err.contains("minimum manifest version"), "{err}");
    assert!(err.contains("not reached"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wait_for_version_returns_the_write_that_reached_it() {
    let (dir, log) = unique_log("min-hit");
    let mut writer = Store::create(&log).unwrap();
    writer.append(item("a", "1")).unwrap();
    let mut standby = Store::open_read_only(&log).unwrap();
    writer.append(item("b", "2")).unwrap();
    let want = writer.manifest_version();
    standby
        .wait_for_version(want, Duration::from_secs(2))
        .unwrap();
    assert!(standby.manifest_version() >= want);
    assert_eq!(n_of(&standby, "b"), "2");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn promote_fences_the_previous_writer() {
    let (dir, log) = unique_log("promote");
    let mut writer = Store::create(&log).unwrap();
    writer.append(item("a", "1")).unwrap();
    let acked = writer.manifest_version();
    let mut standby = Store::open_read_only(&log).unwrap();
    writer.append(item("b", "2")).unwrap();
    standby.promote(Duration::from_secs(2)).unwrap();
    assert!(!standby.is_read_only());
    assert!(!standby.is_fenced());
    assert!(writer.is_fenced());
    let err = writer.append(item("c", "3")).unwrap_err();
    assert!(err.contains(LOG_FENCED), "{err}");
    standby.append(item("d", "4")).unwrap();
    assert_eq!(n_of(&standby, "a"), "1");
    assert_eq!(n_of(&standby, "b"), "2");
    assert!(standby
        .load(NS, "Item", "c", &PropertyAcl::allow_all())
        .is_err());
    assert_eq!(n_of(&standby, "d"), "4");
    assert!(standby.manifest_version() >= acked);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn two_promotes_leave_one_writer() {
    let (dir, log) = unique_log("two");
    let mut seed = Store::create(&log).unwrap();
    seed.append(item("a", "1")).unwrap();
    drop(seed);
    let mut first = Store::open_read_only(&log).unwrap();
    let mut second = Store::open_read_only(&log).unwrap();
    first.promote(Duration::from_secs(2)).unwrap();
    second.promote(Duration::from_secs(2)).unwrap();
    assert!(
        first.is_fenced(),
        "second claim must fence the first writer"
    );
    assert!(!second.is_read_only());
    assert!(!second.is_fenced());
    second.append(item("w", "1")).unwrap();
    let err = first.append(item("l", "1")).unwrap_err();
    assert!(err.contains(LOG_FENCED), "{err}");
    second.append(item("w2", "2")).unwrap();
    let err = first.append(item("l2", "2")).unwrap_err();
    assert!(
        err.contains(LOG_FENCED),
        "loser must not write again: {err}"
    );
    assert_eq!(n_of(&second, "w"), "1");
    assert_eq!(n_of(&second, "w2"), "2");
    assert!(second
        .load(NS, "Item", "l", &PropertyAcl::allow_all())
        .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn read_only_open_does_not_collect() {
    let (dir, log) = unique_log("nocollect");
    let mut writer = Store::create(&log).unwrap();
    writer.append(item("a", "1")).unwrap();
    writer.set_crash_before_swap(true);
    let _ = writer.append(item("b", "2"));
    writer.set_crash_before_swap(false);
    let before = std::fs::read_dir(crate::log::objects_dir(&log))
        .unwrap()
        .count();
    let _standby = Store::open_read_only(&log).unwrap();
    let after = std::fs::read_dir(crate::log::objects_dir(&log))
        .unwrap()
        .count();
    assert_eq!(before, after, "standby must not collect");
    let _ = std::fs::remove_dir_all(&dir);
}
