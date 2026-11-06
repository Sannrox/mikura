//! Published projections and backups sync their directory (#341).
//!
//! Crash model: after power loss, a name published in a directory that was
//! not synced afterwards may be gone. Each publish must list its directory
//! in [`take_synced_dirs`] before it returns.

use super::super::*;
use super::helpers::*;
use crate::backup::{backup_log, backup_log_incremental, restore_log};
use crate::durable::take_synced_dirs;

fn item(key: &str, n: &str) -> ObjectRecord {
    rec("Item", key, false, &[("n", n)])
}

fn synced(dir: &std::path::Path) -> bool {
    take_synced_dirs().iter().any(|synced| synced == dir)
}

#[test]
fn join_sidecar_and_log_checkpoint_survive_power_loss_once_written() {
    let (dir, _) = temp_log("durable-projections");
    let log = dir.join("live").join("objects.mikura");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    let mut store = Store::create(&log).unwrap();
    take_synced_dirs();

    store.append(item("a", "1")).unwrap();
    assert!(Store::join_map_path(&log).is_file());
    assert!(synced(log.parent().unwrap()), "join sidecar rename");

    store.checkpoint().unwrap();
    assert!(Store::checkpoint_path(&log).is_file());
    assert!(synced(log.parent().unwrap()), "log checkpoint rename");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn backups_and_restores_survive_power_loss_once_returned() {
    let (dir, log) = temp_log("durable-backup");
    let mut store = Store::create(&log).unwrap();
    store.append(item("a", "1")).unwrap();
    let backups = dir.join("backups");
    std::fs::create_dir_all(&backups).unwrap();
    let backup = backups.join("backup.mikura");
    take_synced_dirs();

    // Without a log checkpoint to carry, only the hard link publishes into
    // `backups`.
    let _ = std::fs::remove_file(Store::checkpoint_path(&log));
    let _ = std::fs::remove_file(Store::checkpoint_prev_path(&log));
    backup_log(&log, &backup).unwrap();
    assert!(!Store::checkpoint_path(&backup).exists());
    assert!(synced(&backups), "full backup link");

    store.append(item("a", "2")).unwrap();
    store.checkpoint().unwrap();
    take_synced_dirs();
    backup_log_incremental(&log, &backup).unwrap();
    assert!(Store::checkpoint_path(&backup).is_file());
    assert!(synced(&backups), "incremental checkpoint copy");

    let restores = dir.join("restores");
    std::fs::create_dir_all(&restores).unwrap();
    let restored = restores.join("restored.mikura");
    restore_log(&backup, &restored, None).unwrap();
    assert!(synced(&restores), "restored log link");
    let reopened = Store::open(&restored).unwrap();
    assert_eq!(
        reopened
            .load(NS, "Item", "a", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("n")
            .map(String::as_str),
        Some("2")
    );
    let _ = std::fs::remove_dir_all(&dir);
}
