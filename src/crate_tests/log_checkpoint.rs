use super::super::*;
use super::helpers::*;

fn evaluate_sum(store: &Store) -> i64 {
    ObjectSet::new(LocalCompute)
        .evaluate(store, &fixture_request())
        .unwrap()
        .sum_amount
}

fn remove_joins(log: &std::path::Path) {
    let _ = std::fs::remove_file(Store::join_map_path(log));
    let _ = std::fs::remove_file(Store::join_delta_path(log));
}

#[test]
fn checkpoint_open_matches_full_replay() {
    let (dir, log) = temp_log("ckpt-dual");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, generic_records());
    store.checkpoint().unwrap();
    let live = evaluate_sum(&store);
    drop(store);

    remove_joins(&log);
    assert!(Store::checkpoint_path(&log).is_file());
    crate::log::take_pages_decoded();
    let from_ckpt = Store::open(&log).unwrap();
    let ckpt_pages = crate::log::take_pages_decoded();
    assert_eq!(
        ckpt_pages, 0,
        "matching checkpoint must not decode log pages"
    );
    assert_eq!(evaluate_sum(&from_ckpt), live);
    drop(from_ckpt);

    std::fs::remove_file(Store::checkpoint_path(&log)).unwrap();
    let _ = std::fs::remove_file(Store::checkpoint_prev_path(&log));
    remove_joins(&log);
    crate::log::take_pages_decoded();
    let replayed = Store::open(&log).unwrap();
    assert!(crate::log::take_pages_decoded() > 0);
    assert_eq!(evaluate_sum(&replayed), live);
    let _ = std::fs::remove_dir_all(&dir);
}

fn wide_item(key: &str, fill: char) -> ObjectRecord {
    ObjectRecord {
        gen: 1,
        namespace: NS.into(),
        kind: "Item".into(),
        key: key.into(),
        hidden: false,
        action_id: None,
        props: HashMap::from([("payload".into(), fill.to_string().repeat(1500))]),
    }
}

#[test]
fn stale_sidecar_replays_only_the_tail() {
    let (dir, log) = temp_log("ckpt-tail");
    let mut store = Store::create(&log).unwrap();
    store.set_join_delta_compact_bytes(0);
    for i in 0..8 {
        store.append(wide_item(&format!("k{i:02}"), 'x')).unwrap();
    }
    store.checkpoint().unwrap();
    let prefix_pages = store.committed_pages();
    let sidecar_at_prefix = std::fs::read(Store::join_map_path(&log)).unwrap();
    drop(store);

    let mut store = Store::open(&log).unwrap();
    store.set_join_delta_compact_bytes(0);
    store.append(wide_item("tail", 'y')).unwrap();
    let total_pages = store.committed_pages();
    assert!(total_pages > prefix_pages);
    drop(store);

    std::fs::write(Store::join_map_path(&log), sidecar_at_prefix).unwrap();
    let _ = std::fs::remove_file(Store::join_delta_path(&log));
    let _ = std::fs::remove_file(Store::checkpoint_path(&log));
    let _ = std::fs::remove_file(Store::checkpoint_prev_path(&log));
    crate::log::take_pages_decoded();
    let reopened = Store::open(&log).unwrap();
    let decoded = crate::log::take_pages_decoded();
    assert_eq!(
        decoded,
        total_pages - prefix_pages,
        "stale sidecar must decode only pages after its stamp"
    );
    assert!(reopened
        .load(NS, "Item", "tail", &PropertyAcl::allow_all())
        .is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn torn_checkpoint_falls_back_to_previous_then_replay() {
    let (dir, log) = temp_log("ckpt-torn");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    store.checkpoint().unwrap();
    let first = std::fs::read(Store::checkpoint_path(&log)).unwrap();
    store
        .append(rec(
            "Shipment",
            "s2",
            false,
            &[("order_id", "o1"), ("amount", "5")],
        ))
        .unwrap();
    store.checkpoint().unwrap();
    let live = evaluate_sum(&store);
    drop(store);

    std::fs::write(Store::checkpoint_prev_path(&log), &first).unwrap();
    let mut torn = std::fs::read(Store::checkpoint_path(&log)).unwrap();
    torn.truncate(8);
    std::fs::write(Store::checkpoint_path(&log), torn).unwrap();
    remove_joins(&log);

    let from_prev = Store::open(&log).unwrap();
    assert_eq!(evaluate_sum(&from_prev), live);
    drop(from_prev);

    std::fs::write(Store::checkpoint_path(&log), &first[..3]).unwrap();
    std::fs::remove_file(Store::checkpoint_prev_path(&log)).unwrap();
    remove_joins(&log);
    let from_log = Store::open(&log).unwrap();
    assert_eq!(evaluate_sum(&from_log), live);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn compact_to_keeps_live_answers_and_action_retry() {
    let (dir, log) = temp_log("ckpt-compact");
    let compact = dir.join("compacted.mikura");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    store
        .apply_action(
            Action {
                namespace: NS.into(),
                id: "act-s2".into(),
                kind: "Shipment".into(),
                key: "s2".into(),
                props: HashMap::from([
                    ("order_id".into(), "o1".into()),
                    ("amount".into(), "5".into()),
                ]),
            },
            None,
        )
        .unwrap();
    store
        .apply_action(
            Action {
                namespace: NS.into(),
                id: "act-s2".into(),
                kind: "Shipment".into(),
                key: "s2".into(),
                props: HashMap::from([
                    ("order_id".into(), "o1".into()),
                    ("amount".into(), "7".into()),
                ]),
            },
            None,
        )
        .unwrap_err();
    let live = evaluate_sum(&store);
    let n = store.compact_to(&compact).unwrap();
    assert!(n > 0);
    drop(store);

    let compacted = Store::open(&compact).unwrap();
    assert_eq!(evaluate_sum(&compacted), live);
    let loaded = compacted
        .load(NS, "Shipment", "s2", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(loaded.action_id.as_deref(), Some("act-s2"));
    drop(compacted);

    let mut compacted = Store::open(&compact).unwrap();
    compacted
        .apply_action(
            Action {
                namespace: NS.into(),
                id: "act-s2".into(),
                kind: "Shipment".into(),
                key: "s2".into(),
                props: HashMap::from([
                    ("order_id".into(), "o1".into()),
                    ("amount".into(), "5".into()),
                ]),
            },
            None,
        )
        .unwrap();
    let conflict = compacted
        .apply_action(
            Action {
                namespace: NS.into(),
                id: "act-s2".into(),
                kind: "Shipment".into(),
                key: "s2".into(),
                props: HashMap::from([
                    ("order_id".into(), "o1".into()),
                    ("amount".into(), "9".into()),
                ]),
            },
            None,
        )
        .unwrap_err();
    assert!(conflict.contains("conflict") || conflict.contains("already"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn truncated_log_fails_closed_with_matching_checkpoint() {
    let (dir, log) = temp_log("ckpt-trunc");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    store.checkpoint().unwrap();
    drop(store);

    remove_joins(&log);
    assert!(Store::checkpoint_path(&log).is_file());
    let bytes = std::fs::read(&log).unwrap();
    assert!(bytes.len() > crate::log::PAGE);
    std::fs::write(&log, &bytes[..crate::log::PAGE]).unwrap();

    let err = match Store::open(&log) {
        Err(err) => err,
        Ok(_) => panic!("truncated log plus matching checkpoint must fail closed"),
    };
    assert!(
        err.contains("committed pages missing"),
        "missing committed pages must fail closed: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn join_checksum_mismatch_fails_closed_even_with_checkpoint() {
    let (dir, log) = temp_log("ckpt-join-crc");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    store.checkpoint().unwrap();
    drop(store);

    let sidecar = Store::join_map_path(&log);
    let mut flipped = std::fs::read(&sidecar).unwrap();
    let last = flipped.len() - 5;
    flipped[last] ^= 0x01;
    std::fs::write(&sidecar, flipped).unwrap();
    assert!(Store::checkpoint_path(&log).is_file());
    let err = match Store::open(&log) {
        Err(err) => err,
        Ok(_) => panic!("bit-flip should fail closed"),
    };
    assert!(
        err.contains("checksum"),
        "corrupt joins must fail closed despite a log checkpoint: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn compact_checkpoint_keeps_superseded_action_id() {
    let (dir, log) = temp_log("ckpt-compact-action");
    let compact = dir.join("compacted.mikura");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    store
        .apply_action(
            Action {
                namespace: NS.into(),
                id: "act-old".into(),
                kind: "Shipment".into(),
                key: "s2".into(),
                props: HashMap::from([
                    ("order_id".into(), "o1".into()),
                    ("amount".into(), "5".into()),
                ]),
            },
            None,
        )
        .unwrap();
    store
        .apply_action(
            Action {
                namespace: NS.into(),
                id: "act-new".into(),
                kind: "Shipment".into(),
                key: "s2".into(),
                props: HashMap::from([
                    ("order_id".into(), "o1".into()),
                    ("amount".into(), "8".into()),
                ]),
            },
            None,
        )
        .unwrap();
    store.compact_to(&compact).unwrap();
    drop(store);

    let mut compacted = Store::open(&compact).unwrap();
    compacted
        .apply_action(
            Action {
                namespace: NS.into(),
                id: "act-old".into(),
                kind: "Shipment".into(),
                key: "s2".into(),
                props: HashMap::from([
                    ("order_id".into(), "o1".into()),
                    ("amount".into(), "5".into()),
                ]),
            },
            None,
        )
        .unwrap();
    assert_eq!(
        compacted
            .load(NS, "Shipment", "s2", &PropertyAcl::allow_all())
            .unwrap()
            .action_id
            .as_deref(),
        Some("act-new")
    );
    drop(compacted);

    std::fs::remove_file(Store::checkpoint_path(&compact)).unwrap();
    let _ = std::fs::remove_file(Store::checkpoint_prev_path(&compact));
    remove_joins(&compact);
    let mut rebuilt = Store::open(&compact).unwrap();
    rebuilt
        .apply_action(
            Action {
                namespace: NS.into(),
                id: "act-old".into(),
                kind: "Shipment".into(),
                key: "s2".into(),
                props: HashMap::from([
                    ("order_id".into(), "o1".into()),
                    ("amount".into(), "5".into()),
                ]),
            },
            None,
        )
        .unwrap();
    assert_eq!(
        rebuilt
            .load(NS, "Shipment", "s2", &PropertyAcl::allow_all())
            .unwrap()
            .action_id
            .as_deref(),
        Some("act-old")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn future_join_delta_does_not_install_uncommitted_rows() {
    let (dir, log) = temp_log("ckpt-future-delta");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    store.set_join_delta_compact_bytes(0);
    store
        .append(rec("Customer", "c-force", false, &[("region", "us")]))
        .unwrap();
    let prefix_log = std::fs::read(&log).unwrap();
    let prefix_joins = std::fs::read(Store::join_map_path(&log)).unwrap();
    store.set_join_delta_compact_bytes(u64::MAX);
    store
        .append(rec("Customer", "c-future", false, &[("region", "eu")]))
        .unwrap();
    let future_delta = std::fs::read(Store::join_delta_path(&log)).unwrap();
    drop(store);

    std::fs::write(&log, prefix_log).unwrap();
    std::fs::write(Store::join_map_path(&log), prefix_joins).unwrap();
    std::fs::write(Store::join_delta_path(&log), future_delta).unwrap();
    let _ = std::fs::remove_file(Store::checkpoint_path(&log));
    let _ = std::fs::remove_file(Store::checkpoint_prev_path(&log));

    let reopened = Store::open(&log).unwrap();
    assert!(reopened
        .load(NS, "Customer", "c-force", &PropertyAcl::allow_all())
        .is_ok());
    let missing = reopened.load(NS, "Customer", "c-future", &PropertyAcl::allow_all());
    assert!(
        missing.is_err(),
        "future delta row must not survive a shorter log"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn future_join_sidecar_is_rebuilt_from_checkpoint() {
    let (dir, log) = temp_log("ckpt-future-joins");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    store.checkpoint().unwrap();
    let prefix_log = std::fs::read(&log).unwrap();
    let prefix_ckpt = std::fs::read(Store::checkpoint_path(&log)).unwrap();
    let prefix_pages = store.committed_pages();
    store.append(wide_item("c-future", 'z')).unwrap();
    let future_pages = store.committed_pages();
    assert!(future_pages > prefix_pages);
    let future_joins = std::fs::read(Store::join_map_path(&log)).unwrap();
    drop(store);

    std::fs::write(&log, prefix_log).unwrap();
    std::fs::write(Store::checkpoint_path(&log), prefix_ckpt).unwrap();
    std::fs::write(Store::join_map_path(&log), future_joins).unwrap();
    let _ = std::fs::remove_file(Store::join_delta_path(&log));
    let _ = std::fs::remove_file(Store::checkpoint_prev_path(&log));

    let mut store = Store::open(&log).unwrap();
    assert!(store
        .load(NS, "Item", "c-future", &PropertyAcl::allow_all())
        .is_err());
    let mut n = 0u32;
    while store.committed_pages() < future_pages {
        store.append(wide_item(&format!("pad{n:02}"), 'w')).unwrap();
        n += 1;
    }
    drop(store);

    let reopened = Store::open(&log).unwrap();
    let phantom = reopened.load(NS, "Item", "c-future", &PropertyAcl::allow_all());
    assert!(
        phantom.is_err(),
        "future sidecar rows must not return after the log catches up"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn future_join_delta_rebuild_keeps_compacted_action_catalog() {
    let (dir, log) = temp_log("ckpt-future-delta-catalog");
    let compact = dir.join("compacted.mikura");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    store
        .apply_action(
            Action {
                namespace: NS.into(),
                id: "act-old".into(),
                kind: "Shipment".into(),
                key: "s2".into(),
                props: HashMap::from([
                    ("order_id".into(), "o1".into()),
                    ("amount".into(), "5".into()),
                ]),
            },
            None,
        )
        .unwrap();
    store
        .apply_action(
            Action {
                namespace: NS.into(),
                id: "act-new".into(),
                kind: "Shipment".into(),
                key: "s2".into(),
                props: HashMap::from([
                    ("order_id".into(), "o1".into()),
                    ("amount".into(), "8".into()),
                ]),
            },
            None,
        )
        .unwrap();
    store.compact_to(&compact).unwrap();
    drop(store);

    let mut compacted = Store::open(&compact).unwrap();
    compacted.set_join_delta_compact_bytes(0);
    compacted
        .append(rec("Customer", "c-force", false, &[("region", "us")]))
        .unwrap();
    let prefix_log = std::fs::read(&compact).unwrap();
    let prefix_joins = std::fs::read(Store::join_map_path(&compact)).unwrap();
    let prefix_ckpt = std::fs::read(Store::checkpoint_path(&compact)).unwrap();
    compacted.set_join_delta_compact_bytes(u64::MAX);
    compacted
        .append(rec("Customer", "c-future", false, &[("region", "eu")]))
        .unwrap();
    let future_delta = std::fs::read(Store::join_delta_path(&compact)).unwrap();
    drop(compacted);

    std::fs::write(&compact, prefix_log).unwrap();
    std::fs::write(Store::join_map_path(&compact), prefix_joins).unwrap();
    std::fs::write(Store::join_delta_path(&compact), future_delta).unwrap();
    std::fs::write(Store::checkpoint_path(&compact), prefix_ckpt).unwrap();
    let _ = std::fs::remove_file(Store::checkpoint_prev_path(&compact));

    let mut reopened = Store::open(&compact).unwrap();
    assert!(reopened
        .load(NS, "Customer", "c-future", &PropertyAcl::allow_all())
        .is_err());
    reopened
        .apply_action(
            Action {
                namespace: NS.into(),
                id: "act-old".into(),
                kind: "Shipment".into(),
                key: "s2".into(),
                props: HashMap::from([
                    ("order_id".into(), "o1".into()),
                    ("amount".into(), "5".into()),
                ]),
            },
            None,
        )
        .unwrap();
    assert_eq!(
        reopened
            .load(NS, "Shipment", "s2", &PropertyAcl::allow_all())
            .unwrap()
            .action_id
            .as_deref(),
        Some("act-new")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn checkpoint_and_compact_refuse_uncommitted_pages() {
    let (dir, log) = temp_log("ckpt-pending");
    let compact = dir.join("compacted.mikura");
    let mut store = Store::create(&log).unwrap();
    store
        .append_uncommitted(rec("Customer", "c1", false, &[("region", "us")]))
        .unwrap();
    let checkpoint = store.checkpoint().unwrap_err();
    assert!(
        checkpoint.contains("uncommitted"),
        "checkpoint must refuse pending writes: {checkpoint}"
    );
    let compact_err = store.compact_to(&compact).unwrap_err();
    assert!(
        compact_err.contains("uncommitted"),
        "compact_to must refuse pending writes: {compact_err}"
    );
    drop(store);
    assert!(!compact.exists());
    remove_joins(&log);
    let _ = std::fs::remove_file(Store::checkpoint_path(&log));
    let _ = std::fs::remove_file(Store::checkpoint_prev_path(&log));
    let reopened = Store::open(&log).unwrap();
    assert!(reopened
        .load(NS, "Customer", "c1", &PropertyAcl::allow_all())
        .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn join_sidecar_open_keeps_compacted_action_catalog() {
    let (dir, log) = temp_log("ckpt-sidecar-catalog");
    let compact = dir.join("compacted.mikura");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    store
        .apply_action(
            Action {
                namespace: NS.into(),
                id: "act-old".into(),
                kind: "Shipment".into(),
                key: "s2".into(),
                props: HashMap::from([
                    ("order_id".into(), "o1".into()),
                    ("amount".into(), "5".into()),
                ]),
            },
            None,
        )
        .unwrap();
    store
        .apply_action(
            Action {
                namespace: NS.into(),
                id: "act-new".into(),
                kind: "Shipment".into(),
                key: "s2".into(),
                props: HashMap::from([
                    ("order_id".into(), "o1".into()),
                    ("amount".into(), "8".into()),
                ]),
            },
            None,
        )
        .unwrap();
    store.compact_to(&compact).unwrap();
    drop(store);

    let mut compacted = Store::open(&compact).unwrap();
    compacted.set_join_delta_compact_bytes(0);
    compacted
        .append(rec("Customer", "c2", false, &[("region", "us")]))
        .unwrap();
    assert!(Store::join_map_path(&compact).is_file());
    drop(compacted);

    let mut compacted = Store::open(&compact).unwrap();
    compacted
        .apply_action(
            Action {
                namespace: NS.into(),
                id: "act-old".into(),
                kind: "Shipment".into(),
                key: "s2".into(),
                props: HashMap::from([
                    ("order_id".into(), "o1".into()),
                    ("amount".into(), "5".into()),
                ]),
            },
            None,
        )
        .unwrap();
    assert_eq!(
        compacted
            .load(NS, "Shipment", "s2", &PropertyAcl::allow_all())
            .unwrap()
            .action_id
            .as_deref(),
        Some("act-new")
    );
    let _ = std::fs::remove_dir_all(&dir);
}
