use super::*;
use crate::{
    merge_source_and_edits, snapshot_changelog, BatchIngest, ChangelogIngest, MergeIngest,
};
use mikura::LocalCompute;

#[test]
fn changelog_identical_snapshots_emit_nothing() {
    let snapshot = vec![
        rec("Shipment", "s1", false, &[("amount", "10")]),
        rec("Shipment", "s2", true, &[("amount", "99")]),
    ];
    assert!(snapshot_changelog(snapshot.clone(), snapshot).is_empty());
}

#[test]
fn changelog_new_key_emits_visible_record() {
    let records = snapshot_changelog(
        Vec::new(),
        vec![rec("Shipment", "s1", false, &[("amount", "10")])],
    );
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].key, "s1");
    assert!(!records[0].hidden);
    assert_eq!(
        records[0].props.get("amount").map(String::as_str),
        Some("10")
    );
}

#[test]
fn changelog_deleted_key_emits_hide() {
    let records = snapshot_changelog(
        vec![rec("Shipment", "s1", false, &[("amount", "10")])],
        Vec::new(),
    );
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].key, "s1");
    assert!(records[0].hidden);
    assert_eq!(
        records[0].props.get("amount").map(String::as_str),
        Some("10")
    );
}

#[test]
fn changelog_changed_action_id_emits_current_payload() {
    let mut previous = rec("Shipment", "s1", false, &[("amount", "10")]);
    previous.action_id = Some("act-a".into());
    let mut current = rec("Shipment", "s1", false, &[("amount", "10")]);
    current.action_id = Some("act-b".into());
    let records = snapshot_changelog(vec![previous], vec![current]);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].action_id.as_deref(), Some("act-b"));
    assert_eq!(
        records[0].props.get("amount").map(String::as_str),
        Some("10")
    );
}

#[test]
fn changelog_changed_prop_emits_current_payload() {
    let records = snapshot_changelog(
        vec![rec("Shipment", "s1", false, &[("amount", "10")])],
        vec![rec("Shipment", "s1", false, &[("amount", "5")])],
    );
    assert_eq!(records.len(), 1);
    assert!(!records[0].hidden);
    assert_eq!(
        records[0].props.get("amount").map(String::as_str),
        Some("5")
    );
}

#[test]
fn changelog_into_merge_matches_new_snapshot_plus_edits() {
    let previous = vec![
        rec(
            "Shipment",
            "s1",
            false,
            &[("order_id", "o1"), ("amount", "10")],
        ),
        rec(
            "Shipment",
            "s2",
            false,
            &[("order_id", "o1"), ("amount", "3")],
        ),
    ];
    let current = vec![
        rec(
            "Shipment",
            "s1",
            false,
            &[("order_id", "o1"), ("amount", "8")],
        ),
        rec(
            "Shipment",
            "s3",
            false,
            &[("order_id", "o1"), ("amount", "4")],
        ),
    ];
    let edits = vec![rec(
        "Shipment",
        "s3",
        false,
        &[("order_id", "o1"), ("amount", "7")],
    )];
    let changelog = snapshot_changelog(previous.clone(), current.clone());
    assert_eq!(changelog.len(), 3);

    let (dir, log) = temp_log("changelog-merge");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, previous).unwrap();
    MergeIngest::run(&mut store, changelog, edits.clone()).unwrap();

    let expected = merge_source_and_edits(current, edits);
    let oss = ObjectSet::new(LocalCompute);
    let live = oss.evaluate(&store, &shipment_request()).unwrap();
    assert_eq!(live.two_hop_count, 2);
    assert_eq!(live.sum_amount, 15);
    let reopened = Store::open(&log).unwrap();
    let from_log = oss.evaluate(&reopened, &shipment_request()).unwrap();
    assert_eq!(from_log.two_hop_count, live.two_hop_count);
    assert_eq!(from_log.sum_amount, live.sum_amount);
    assert_same_identity(&store, &reopened, &expected);
    assert!(!reopened.joins().is_visible("Shipment", "s2"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn changelog_empty_diff_does_not_append() {
    let snapshot = vec![rec("Shipment", "s1", false, &[("amount", "10")])];
    let (dir, log) = temp_log("changelog-empty");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, snapshot.clone()).unwrap();
    ChangelogIngest::run(&mut store, snapshot.clone(), snapshot).unwrap();
    assert!(store.joins().is_visible("Shipment", "s1"));
    let reopened = Store::open(&log).unwrap();
    assert!(reopened.joins().is_visible("Shipment", "s1"));
    let _ = std::fs::remove_dir_all(&dir);
}
