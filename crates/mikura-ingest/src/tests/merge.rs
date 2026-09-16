use super::*;
use crate::{merge_source_and_edits, MergeIngest};
use mikura::LocalCompute;

#[test]
fn merge_source_only_keeps_source_records() {
    let source = vec![
        rec("Shipment", "s1", false, &[("amount", "10")]),
        rec("Shipment", "s2", true, &[("amount", "99")]),
    ];
    let merged = merge_source_and_edits(source.clone(), Vec::new());
    assert_eq!(merged, source);
}

#[test]
fn merge_edits_only_keeps_edit_records() {
    let edits = vec![rec("Shipment", "s1", false, &[("amount", "5")])];
    let merged = merge_source_and_edits(Vec::new(), edits.clone());
    assert_eq!(merged, edits);
}

#[test]
fn merge_same_key_edit_wins() {
    let source = vec![rec(
        "Shipment",
        "s1",
        false,
        &[("order_id", "o1"), ("amount", "10")],
    )];
    let edits = vec![rec(
        "Shipment",
        "s1",
        false,
        &[("order_id", "o1"), ("amount", "5")],
    )];
    let merged = merge_source_and_edits(source, edits);
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].props.get("amount").map(String::as_str), Some("5"));
    assert!(!merged[0].hidden);
}

#[test]
fn merge_hidden_source_visible_edit_unhides() {
    let source = vec![rec("Shipment", "s1", true, &[("amount", "10")])];
    let edits = vec![rec("Shipment", "s1", false, &[("amount", "7")])];
    let merged = merge_source_and_edits(source, edits);
    assert_eq!(merged.len(), 1);
    assert!(!merged[0].hidden);
    assert_eq!(merged[0].props.get("amount").map(String::as_str), Some("7"));
}

#[test]
fn merge_visible_source_hidden_edit_hides() {
    let source = vec![rec("Shipment", "s1", false, &[("amount", "10")])];
    let edits = vec![rec("Shipment", "s1", true, &[("amount", "10")])];
    let merged = merge_source_and_edits(source, edits);
    assert_eq!(merged.len(), 1);
    assert!(merged[0].hidden);
}

#[test]
fn merge_then_append_matches_sequential_merged_set() {
    let source = vec![
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
        rec(
            "Shipment",
            "s3",
            true,
            &[("order_id", "o1"), ("amount", "99")],
        ),
    ];
    let edits = vec![
        rec(
            "Shipment",
            "s1",
            false,
            &[("order_id", "o1"), ("amount", "5")],
        ),
        rec(
            "Shipment",
            "s3",
            false,
            &[("order_id", "o1"), ("amount", "4")],
        ),
        rec(
            "Shipment",
            "s4",
            true,
            &[("order_id", "o1"), ("amount", "8")],
        ),
    ];
    let merged = merge_source_and_edits(source.clone(), edits.clone());

    let (dir, log_merge) = temp_log("merge-then-append");
    let mut merged_store = Store::create(&log_merge).unwrap();
    MergeIngest::run(&mut merged_store, source, edits).unwrap();

    let log_seq = dir.join("sequential.mikura");
    let mut sequential = Store::create(&log_seq).unwrap();
    for record in merged {
        sequential.append(record).unwrap();
    }

    let oss = ObjectSet::new(LocalCompute);
    let req = shipment_request();
    let from_merge = oss.evaluate(&merged_store, &req).unwrap();
    let from_seq = oss.evaluate(&sequential, &req).unwrap();
    assert_eq!(from_merge.two_hop_count, from_seq.two_hop_count);
    assert_eq!(from_merge.sum_amount, from_seq.sum_amount);
    assert_eq!(from_merge.two_hop_count, 3);
    assert_eq!(from_merge.sum_amount, 12);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn merge_append_reopen_matches_live_identity() {
    let source = vec![
        rec("Customer", "c1", false, &[("region", "us")]),
        rec(
            "Shipment",
            "s1",
            false,
            &[("order_id", "o1"), ("amount", "10")],
        ),
        rec(
            "Shipment",
            "s_hidden",
            true,
            &[("order_id", "o1"), ("amount", "99")],
        ),
    ];
    let edits = vec![
        rec(
            "Shipment",
            "s1",
            false,
            &[("order_id", "o1"), ("amount", "5")],
        ),
        rec(
            "Shipment",
            "s_hidden",
            false,
            &[("order_id", "o1"), ("amount", "4")],
        ),
        rec(
            "Shipment",
            "s_edit_only",
            true,
            &[("order_id", "o1"), ("amount", "8")],
        ),
    ];
    let merged = merge_source_and_edits(source.clone(), edits.clone());

    let (dir, log) = temp_log("merge-reopen");
    let mut store = Store::create(&log).unwrap();
    MergeIngest::run(&mut store, source, edits).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let live = oss.evaluate(&store, &shipment_request()).unwrap();
    assert_eq!(live.two_hop_count, 2);
    assert_eq!(live.sum_amount, 9);

    let reopened = Store::open(&log).unwrap();
    let from_log = oss.evaluate(&reopened, &shipment_request()).unwrap();
    assert_eq!(from_log.two_hop_count, live.two_hop_count);
    assert_eq!(from_log.sum_amount, live.sum_amount);
    assert_same_identity(&store, &reopened, &merged);
    assert!(!reopened.joins().is_visible("Shipment", "s_edit_only"));
    let _ = std::fs::remove_dir_all(&dir);
}
