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
