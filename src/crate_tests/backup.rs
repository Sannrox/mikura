use super::super::*;
use super::helpers::*;
use crate::backup::{
    backup_log, backup_log_incremental, restore_log, restore_point_at, verify_log, RestorePoint,
};

fn item(key: &str, n: &str) -> ObjectRecord {
    rec("Item", key, false, &[("n", n)])
}

fn load_n(store: &Store, key: &str) -> Option<String> {
    store
        .load(NS, "Item", key, &PropertyAcl::allow_all())
        .ok()
        .and_then(|record| record.props.get("n").cloned())
}

#[test]
fn restore_to_before_a_bad_batch_answers_as_before_it() {
    let (dir, log) = temp_log("backup-drill");
    let mut store = Store::create(&log).unwrap();
    store.append(item("a", "1")).unwrap();
    store.append(item("b", "2")).unwrap();
    let before = (load_n(&store, "a"), load_n(&store, "b"));
    store
        .append_batch(vec![item("a", "bad"), item("c", "bad")])
        .unwrap();

    let backup = dir.join("backup.mikura");
    let head = backup_log(&log, &backup).unwrap();
    assert_eq!(head.commit_point, store.committed_pages());

    // The bad batch starts on a fresh page; the commit before it ends on the
    // page before.
    let bad = store
        .history(NS, "Item", "c", &PropertyAcl::allow_all())
        .unwrap()[0]
        .page;
    let point = restore_point_at(&backup, bad - 1).unwrap();
    let restored = dir.join("restored.mikura");
    assert_eq!(restore_log(&backup, &restored, Some(point)).unwrap(), point);

    let reopened = Store::open(&restored).unwrap();
    assert_eq!((load_n(&reopened, "a"), load_n(&reopened, "b")), before);
    assert_eq!(load_n(&reopened, "c"), None);
    // Neither the live log nor the backup changed.
    assert_eq!(verify_log(&backup).unwrap().restore_point, head);
    assert_eq!(load_n(&store, "a").as_deref(), Some("bad"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn backups_taken_during_writes_verify_and_restore() {
    let (dir, log) = temp_log("backup-concurrent");
    let store = Store::create(&log).unwrap();
    let writer = std::thread::spawn(move || {
        let mut store = store;
        for i in 0..200 {
            let key = format!("k{i}");
            store.append(item(&key, &i.to_string())).unwrap();
        }
        store
    });
    let mut backups = Vec::new();
    while !writer.is_finished() || backups.is_empty() {
        let path = dir.join(format!("backup-{}.mikura", backups.len()));
        let point = backup_log(&log, &path).unwrap();
        backups.push((path, point));
    }
    let store = writer.join().unwrap();
    let mut points: Vec<_> = backups
        .iter()
        .map(|(_, point)| point.commit_point)
        .collect();
    points.dedup();
    assert!(points.len() > 1, "backups must overlap writes: {points:?}");
    let full = crate::log::read_records(&log).unwrap();
    for (index, (path, point)) in backups.iter().enumerate() {
        let report = verify_log(path).unwrap();
        assert_eq!(report.restore_point, *point);
        let restored = dir.join(format!("restored-{index}.mikura"));
        restore_log(path, &restored, None).unwrap();
        let records = crate::log::read_records(&restored).unwrap();
        assert_eq!(records, full[..records.len()], "backup {index} is a prefix");
        Store::open(&restored).unwrap();
    }
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn incremental_backup_copies_only_new_pages_of_the_same_log() {
    let (dir, log) = temp_log("backup-incremental");
    let mut store = Store::create(&log).unwrap();
    store.append(item("a", "1")).unwrap();
    let backup = dir.join("backup.mikura");
    backup_log(&log, &backup).unwrap();
    store.append(item("b", "2")).unwrap();
    store.append(item("a", "3")).unwrap();

    let point = backup_log_incremental(&log, &backup).unwrap();
    assert_eq!(point.commit_point, store.committed_pages());
    let full = dir.join("full.mikura");
    backup_log(&log, &full).unwrap();
    assert_eq!(
        std::fs::read(&backup).unwrap(),
        std::fs::read(&full).unwrap()
    );

    // A backup of a different log is refused.
    let (other_dir, other) = temp_log("backup-incremental-other");
    let mut other_store = Store::create(&other).unwrap();
    other_store.append(item("z", "9")).unwrap();
    other_store.append(item("y", "8")).unwrap();
    other_store.append(item("x", "7")).unwrap();
    let err = backup_log_incremental(&other, &backup).unwrap_err();
    assert!(err.contains("does not belong"), "{err}");
    let _ = std::fs::remove_dir_all(&other_dir);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn backup_and_restore_fail_closed() {
    let (dir, log) = temp_log("backup-fail-closed");
    let mut store = Store::create(&log).unwrap();
    store.append(item("a", "1")).unwrap();
    store.append(item("b", "2")).unwrap();
    let backup = dir.join("backup.mikura");
    let head = backup_log(&log, &backup).unwrap();

    // Existing destinations are never overwritten, the live log included.
    assert!(backup_log(&log, &backup).is_err());
    assert!(restore_log(&backup, &log, None).is_err());
    // A point from another history, or past the backup, is refused.
    let foreign = RestorePoint {
        commit_point: head.commit_point,
        page_crc: head.page_crc ^ 1,
    };
    assert!(restore_log(&backup, &dir.join("r1.mikura"), Some(foreign)).is_err());
    let past = RestorePoint {
        commit_point: head.commit_point + 1,
        page_crc: 0,
    };
    assert!(restore_log(&backup, &dir.join("r2.mikura"), Some(past)).is_err());

    // An incremental backup onto the source itself, or a link to it, is
    // refused and leaves the log intact.
    let link = dir.join("link.mikura");
    std::fs::hard_link(&log, &link).unwrap();
    let before = std::fs::read(&log).unwrap();
    assert!(backup_log_incremental(&log, &log).is_err());
    assert!(backup_log_incremental(&log, &link).is_err());
    assert_eq!(std::fs::read(&log).unwrap(), before);

    // A flipped byte in a committed page fails verify.
    let mut bytes = std::fs::read(&backup).unwrap();
    bytes[crate::log::PAGE + 100] ^= 0xff;
    let corrupt = dir.join("corrupt.mikura");
    std::fs::write(&corrupt, bytes).unwrap();
    assert!(verify_log(&corrupt).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn restore_points_round_trip_as_text() {
    let point = RestorePoint {
        commit_point: 42,
        page_crc: 0x00ab_cdef,
    };
    assert_eq!(point.to_string(), "mkrp1:42:00abcdef");
    assert_eq!("mkrp1:42:00abcdef".parse::<RestorePoint>().unwrap(), point);
    for bad in [
        "mkrp2:42:00abcdef",
        "mkrp1:42",
        "mkrp1:x:00abcdef",
        "mkrp1:42:abc",
    ] {
        assert!(bad.parse::<RestorePoint>().is_err(), "{bad}");
    }
}

fn shipment(id: &str, amount: &str) -> Action {
    Action {
        namespace: NS.into(),
        id: id.into(),
        kind: "Item".into(),
        key: "s2".into(),
        props: HashMap::from([("n".into(), amount.into())]),
    }
}

#[test]
fn a_compacted_log_keeps_its_superseded_action_ids_through_restore() {
    let (dir, log) = temp_log("backup-compacted");
    let compact = dir.join("compacted.mikura");
    let mut store = Store::create(&log).unwrap();
    store.append(item("s2", "1")).unwrap();
    store.apply_action(shipment("act-old", "5"), None).unwrap();
    store.apply_action(shipment("act-new", "8"), None).unwrap();
    store.compact_to(&compact).unwrap();
    drop(store);

    let backup = dir.join("backup.mikura");
    backup_log(&compact, &backup).unwrap();
    let replay_old = |path: &std::path::Path| {
        let mut store = Store::open(path).unwrap();
        store.apply_action(shipment("act-old", "5"), None).unwrap();
        load_n(&store, "s2")
    };

    // With the carried checkpoint, act-old is a replay, not a new write.
    let restored = dir.join("restored.mikura");
    restore_log(&backup, &restored, None).unwrap();
    assert_eq!(replay_old(&restored).as_deref(), Some("8"));

    // Without it, the superseded id would be applied again.
    let bare = dir.join("bare.mikura");
    restore_log(&backup, &bare, None).unwrap();
    std::fs::remove_file(Store::checkpoint_path(&bare)).unwrap();
    assert_eq!(replay_old(&bare).as_deref(), Some("5"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_checkpoint_past_the_restore_point_is_left_behind() {
    let (dir, log) = temp_log("backup-checkpoint-past");
    let mut store = Store::create(&log).unwrap();
    store.append(item("a", "1")).unwrap();
    let early = store.committed_pages();
    store.append(item("a", "2")).unwrap();
    store.checkpoint().unwrap();
    let backup = dir.join("backup.mikura");
    backup_log(&log, &backup).unwrap();
    assert!(Store::checkpoint_path(&backup).is_file());

    let restored = dir.join("restored.mikura");
    let point = restore_point_at(&backup, early).unwrap();
    restore_log(&backup, &restored, Some(point)).unwrap();
    assert!(!Store::checkpoint_path(&restored).exists());
    let reopened = Store::open(&restored).unwrap();
    assert_eq!(load_n(&reopened, "a").as_deref(), Some("1"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_checkpoint_copy_leaves_no_restored_log() {
    let (dir, log) = temp_log("backup-checkpoint-fails");
    let mut store = Store::create(&log).unwrap();
    store.append(item("a", "1")).unwrap();
    store.checkpoint().unwrap();
    let backup = dir.join("backup.mikura");
    backup_log(&log, &backup).unwrap();

    // A stale checkpoint temp file blocks the exclusive checkpoint copy.
    let restored = dir.join("restored.mikura");
    let mut stale = Store::checkpoint_path(&restored).into_os_string();
    stale.push(".tmp");
    std::fs::write(&stale, b"stale").unwrap();
    assert!(restore_log(&backup, &restored, None).is_err());
    assert!(!restored.exists());
    let _ = std::fs::remove_dir_all(&dir);
}
