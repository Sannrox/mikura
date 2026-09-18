use super::super::*;
use super::helpers::*;

#[test]
fn batch_commit_writes_delta_not_full_sidecar() {
    let (dir, log) = temp_log("join-delta");
    let sidecar = Store::join_map_path(&log);
    let delta = Store::join_delta_path(&log);
    let mut store = Store::create(&log).unwrap();
    for record in generic_records() {
        store.append_uncommitted(record).unwrap();
    }
    store.commit().unwrap();
    let checkpoint = std::fs::metadata(&sidecar).unwrap().len();
    assert!(!delta.exists());
    store
        .append(rec(
            "Shipment",
            "s2",
            false,
            &[("order_id", "o1"), ("amount", "5")],
        ))
        .unwrap();
    let after = std::fs::metadata(&sidecar).unwrap().len();
    assert_eq!(after, checkpoint, "checkpoint should not be rewritten");
    assert!(delta.is_file(), "dirty commit should write a delta");
    assert!(
        std::fs::metadata(&delta).unwrap().len() < checkpoint,
        "delta should be smaller than the checkpoint"
    );
    drop(store);
    let reopened = Store::open(&log).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let response = oss.evaluate(&reopened, &fixture_request()).unwrap();
    assert_eq!(response.two_hop_count, 1);
    assert_eq!(response.sum_amount, 15);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn growing_batch_commits_keep_delta_and_dual_read() {
    let (dir, log) = temp_log("join-delta-bounded");
    let sidecar = Store::join_map_path(&log);
    let delta = Store::join_delta_path(&log);
    let mut store = Store::create(&log).unwrap();
    for i in 0..16 {
        let key = format!("k{i}");
        let n = i.to_string();
        store
            .append_uncommitted(rec("Item", &key, false, &[("n", &n)]))
            .unwrap();
    }
    store.commit().unwrap();
    let checkpoint = std::fs::metadata(&sidecar).unwrap().len();
    assert!(!delta.exists());

    let mut next = 16usize;
    let mut prev_delta = 0u64;
    for (round, batch) in [3usize, 4, 5].into_iter().enumerate() {
        for j in 0..batch {
            let i = next + j;
            let key = format!("k{i}");
            let n = i.to_string();
            store
                .append_uncommitted(rec("Item", &key, false, &[("n", &n)]))
                .unwrap();
        }
        store.commit().unwrap();
        next += batch;
        let after = std::fs::metadata(&sidecar).unwrap().len();
        assert_eq!(
            after, checkpoint,
            "round {round}: later commits must not rewrite the checkpoint"
        );
        assert!(
            delta.is_file(),
            "round {round}: later commits must persist a dirty-set delta"
        );
        let delta_len = std::fs::metadata(&delta).unwrap().len();
        let grew = delta_len.saturating_sub(prev_delta);
        assert!(
            grew < checkpoint,
            "round {round}: persist must write this dirty set, not the checkpoint"
        );
        prev_delta = delta_len;
    }

    drop(store);
    {
        let mut bytes = std::fs::read(&delta).unwrap();
        bytes.extend_from_slice(&[0xff, 0x00, 0x01]);
        std::fs::write(&delta, bytes).unwrap();
    }
    let reopened = Store::open(&log).unwrap();
    for i in 0..next {
        let want = i.to_string();
        let loaded = reopened
            .load("Item", &format!("k{i}"), &PropertyAcl::allow_all())
            .unwrap();
        assert_eq!(
            loaded.props.get("n").map(String::as_str),
            Some(want.as_str())
        );
    }
    drop(reopened);

    std::fs::remove_file(&sidecar).unwrap();
    let _ = std::fs::remove_file(&delta);
    let replayed = Store::open(&log).unwrap();
    for i in 0..next {
        let want = i.to_string();
        let loaded = replayed
            .load("Item", &format!("k{i}"), &PropertyAcl::allow_all())
            .unwrap();
        assert_eq!(
            loaded.props.get("n").map(String::as_str),
            Some(want.as_str())
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn large_delta_compacts_without_dirty_quarter() {
    let (dir, log) = temp_log("join-delta-byte-bound");
    let sidecar = Store::join_map_path(&log);
    let delta = Store::join_delta_path(&log);
    let mut store = Store::create(&log).unwrap();
    const BOUND: u64 = 400;
    store.set_join_delta_compact_bytes(BOUND);
    for record in generic_records() {
        store.append_uncommitted(record).unwrap();
    }
    for i in 0..24 {
        let key = format!("seed{i:02}");
        store
            .append_uncommitted(rec("Item", &key, false, &[("n", &i.to_string())]))
            .unwrap();
    }
    store.commit().unwrap();
    let seed_checkpoint = std::fs::metadata(&sidecar).unwrap().len();
    let seed_identity = 8usize + 24;
    assert!(!delta.exists());
    assert!(
        2usize.saturating_mul(4) <= seed_identity,
        "later batches of two must stay under dirty*4 > identity"
    );

    let mut next = 0usize;
    let mut compacted = false;
    for round in 0..8 {
        for j in 0..2 {
            let i = next + j;
            let key = format!("k{i:02}");
            store
                .append_uncommitted(rec("Item", &key, false, &[("n", &i.to_string())]))
                .unwrap();
        }
        store.commit().unwrap();
        next += 2;
        let identity = seed_identity + next;
        assert!(
            2usize.saturating_mul(4) <= identity,
            "round {round}: dirty*4 must not be the compact trigger"
        );
        if !delta.exists() {
            assert!(
                round >= 2,
                "round {round}: several dirty-set appends must precede the byte-bound compact"
            );
            let after = std::fs::metadata(&sidecar).unwrap().len();
            assert!(
                after != seed_checkpoint,
                "round {round}: byte-bound compact must rewrite the checkpoint"
            );
            compacted = true;
            break;
        }
        let delta_len = std::fs::metadata(&delta).unwrap().len();
        assert!(
            delta_len <= BOUND,
            "round {round}: persist_delta must compact once the file exceeds the bound"
        );
        assert_eq!(
            std::fs::metadata(&sidecar).unwrap().len(),
            seed_checkpoint,
            "round {round}: checkpoint stays until the delta exceeds the bound"
        );
    }
    assert!(compacted, "several small batches must trip the byte bound");
    drop(store);

    let oss = ObjectSet::new(LocalCompute);
    let reopened = Store::open(&log).unwrap();
    let from_maps = oss.evaluate(&reopened, &fixture_request()).unwrap();
    assert_eq!(from_maps.two_hop_count, 1);
    assert_eq!(from_maps.sum_amount, 10);
    let from_maps_asset = oss.evaluate(&reopened, &asset_request()).unwrap();
    assert_eq!(from_maps_asset.two_hop_count, 1);
    assert_eq!(from_maps_asset.sum_amount, 4);
    drop(reopened);

    std::fs::remove_file(&sidecar).unwrap();
    let _ = std::fs::remove_file(&delta);
    let replayed = Store::open(&log).unwrap();
    let from_log = oss.evaluate(&replayed, &fixture_request()).unwrap();
    assert_eq!(from_log.two_hop_count, 1);
    assert_eq!(from_log.sum_amount, 10);
    let from_log_asset = oss.evaluate(&replayed, &asset_request()).unwrap();
    assert_eq!(from_log_asset.two_hop_count, 1);
    assert_eq!(from_log_asset.sum_amount, 4);
    let _ = std::fs::remove_dir_all(&dir);
}
