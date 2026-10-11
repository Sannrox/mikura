use super::*;
use crate::{
    ApplyStatus, StreamApply, StreamChange, SOURCE_KIND, STREAM_APPLY_LATENCY_TARGET_SECS,
};
use mikura::{ObjectRecord, OverlayPatch, PropertyAcl};
use std::collections::HashMap;
use std::time::{Duration, Instant};

fn change(offset: u64, record: ObjectRecord) -> StreamChange {
    StreamChange { offset, record }
}

fn shipment(key: &str, amount: &str) -> ObjectRecord {
    rec(
        "Shipment",
        key,
        false,
        &[("order_id", "o1"), ("amount", amount)],
    )
}

fn gen_of(store: &mikura::Store, kind: &str, key: &str) -> u64 {
    store
        .load("ns", kind, key, &PropertyAcl::allow_all())
        .unwrap()
        .gen
}

#[test]
fn apply_orders_per_key_and_skips_duplicate_and_stale() {
    let (dir, log) = temp_log("stream-apply-order");
    let mut store = Store::create(&log).unwrap();
    let mut apply = StreamApply::new("ns", "orders", 8).unwrap();
    assert_eq!(
        apply
            .apply(&mut store, change(1, shipment("s1", "10")))
            .unwrap(),
        ApplyStatus::Applied
    );
    assert_eq!(
        apply
            .apply(&mut store, change(1, shipment("s1", "99")))
            .unwrap(),
        ApplyStatus::Duplicate
    );
    assert_eq!(
        apply
            .apply(&mut store, change(0, shipment("s1", "1")))
            .unwrap(),
        ApplyStatus::Stale
    );
    assert_eq!(
        apply
            .apply(&mut store, change(2, shipment("s2", "4")))
            .unwrap(),
        ApplyStatus::Applied
    );
    assert_eq!(
        apply
            .apply(&mut store, change(3, shipment("s1", "7")))
            .unwrap(),
        ApplyStatus::Applied
    );
    apply.flush(&mut store).unwrap();
    assert_eq!(gen_of(&store, "Shipment", "s1"), 2);
    assert_eq!(gen_of(&store, "Shipment", "s2"), 1);
    assert_eq!(
        store
            .load("ns", "Shipment", "s1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("amount")
            .map(String::as_str),
        Some("7")
    );
    assert_eq!(apply.last_committed_offset(), Some(3));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn replay_from_earlier_offset_does_not_duplicate_generations() {
    let (dir, log) = temp_log("stream-apply-replay");
    let mut store = Store::create(&log).unwrap();
    let mut apply = StreamApply::new("ns", "orders", 8).unwrap();
    apply
        .apply(&mut store, change(1, shipment("s1", "10")))
        .unwrap();
    apply
        .apply(&mut store, change(2, shipment("s1", "11")))
        .unwrap();
    apply.flush(&mut store).unwrap();
    let gen = gen_of(&store, "Shipment", "s1");
    assert_eq!(
        apply
            .apply(&mut store, change(1, shipment("s1", "10")))
            .unwrap(),
        ApplyStatus::Stale
    );
    assert_eq!(
        apply
            .apply(&mut store, change(2, shipment("s1", "11")))
            .unwrap(),
        ApplyStatus::Duplicate
    );
    assert_eq!(
        apply
            .apply(&mut store, change(3, shipment("s1", "12")))
            .unwrap(),
        ApplyStatus::Applied
    );
    apply.flush(&mut store).unwrap();
    assert_eq!(gen_of(&store, "Shipment", "s1"), gen + 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn crash_before_flush_replays_from_last_committed_offset() {
    let (dir, log) = temp_log("stream-apply-crash");
    let mut store = Store::create(&log).unwrap();
    let mut apply = StreamApply::new("ns", "orders", 8).unwrap();
    apply
        .apply(&mut store, change(1, shipment("s1", "10")))
        .unwrap();
    apply.flush(&mut store).unwrap();
    apply
        .apply(&mut store, change(2, shipment("s1", "11")))
        .unwrap();
    drop(store);

    let mut store = Store::open(&log).unwrap();
    assert_eq!(
        store
            .load("ns", "Shipment", "s1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("amount")
            .map(String::as_str),
        Some("10")
    );
    let mut apply = StreamApply::resume(&store, "ns", "orders", 8).unwrap();
    assert_eq!(apply.last_committed_offset(), Some(1));
    assert_eq!(
        apply
            .apply(&mut store, change(1, shipment("s1", "10")))
            .unwrap(),
        ApplyStatus::Duplicate
    );
    assert_eq!(
        apply
            .apply(&mut store, change(2, shipment("s1", "11")))
            .unwrap(),
        ApplyStatus::Applied
    );
    apply.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Shipment", "s1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("amount")
            .map(String::as_str),
        Some("11")
    );
    assert_eq!(apply.last_committed_offset(), Some(2));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resume_after_flush_reads_cursor_from_the_log() {
    let (dir, log) = temp_log("stream-apply-resume");
    let mut store = Store::create(&log).unwrap();
    let mut apply = StreamApply::new("ns", "orders", 8).unwrap();
    apply
        .apply(&mut store, change(4, shipment("s1", "10")))
        .unwrap();
    apply.flush(&mut store).unwrap();
    drop(store);

    let store = Store::open(&log).unwrap();
    let apply = StreamApply::resume(&store, "ns", "orders", 8).unwrap();
    assert_eq!(apply.last_committed_offset(), Some(4));
    assert_eq!(
        store
            .load("ns", SOURCE_KIND, "orders", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("offset")
            .map(String::as_str),
        Some("4")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn overlay_still_wins_on_stream_apply_source_write() {
    let (dir, log) = temp_log("stream-apply-overlay");
    let mut store = Store::create(&log).unwrap();
    let mut apply = StreamApply::new("ns", "incidents", 8).unwrap();
    apply
        .apply(
            &mut store,
            change(
                1,
                rec(
                    "incident",
                    "inc-1",
                    false,
                    &[("name", "elevated latency"), ("affects", "svc-api")],
                ),
            ),
        )
        .unwrap();
    apply.flush(&mut store).unwrap();
    store
        .apply_overlay(
            OverlayPatch {
                namespace: "ns".into(),
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([("note".into(), "acked".into())]),
                cleared: Vec::new(),
                action_id: None,
            },
            "act-inc-1-note".into(),
            Some(1),
        )
        .unwrap();
    apply
        .apply(
            &mut store,
            change(
                2,
                rec(
                    "incident",
                    "inc-1",
                    false,
                    &[("name", "elevated latency"), ("affects", "svc-api")],
                ),
            ),
        )
        .unwrap();
    apply.flush(&mut store).unwrap();
    let refreshed = store
        .load("ns", "incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        refreshed.props.get("note").map(String::as_str),
        Some("acked")
    );
    assert_eq!(
        refreshed.props.get("name").map(String::as_str),
        Some("elevated latency")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn committed_cursor_floors_every_identity_and_never_regresses() {
    let (dir, log) = temp_log("stream-apply-cursor-floor");
    let mut store = Store::create(&log).unwrap();
    let mut apply = StreamApply::new("ns", "orders", 8).unwrap();
    apply
        .apply(&mut store, change(1, shipment("s1", "10")))
        .unwrap();
    apply
        .apply(&mut store, change(10, shipment("s2", "4")))
        .unwrap();
    apply.flush(&mut store).unwrap();
    assert_eq!(apply.last_committed_offset(), Some(10));
    assert_eq!(
        apply
            .apply(&mut store, change(2, shipment("s1", "11")))
            .unwrap(),
        ApplyStatus::Stale
    );
    apply.flush(&mut store).unwrap();
    assert_eq!(apply.last_committed_offset(), Some(10));
    drop(store);

    let mut store = Store::open(&log).unwrap();
    let mut apply = StreamApply::resume(&store, "ns", "orders", 8).unwrap();
    assert_eq!(apply.last_committed_offset(), Some(10));
    assert_eq!(
        apply
            .apply(&mut store, change(10, shipment("s2", "4")))
            .unwrap(),
        ApplyStatus::Duplicate
    );
    assert_eq!(gen_of(&store, "Shipment", "s2"), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn another_writer_commit_keeps_pending_cursor() {
    let (dir, log) = temp_log("stream-apply-two-sources");
    let mut store = Store::create(&log).unwrap();
    let mut orders = StreamApply::new("ns", "orders", 8).unwrap();
    let mut stock = StreamApply::new("ns", "stock", 64).unwrap();
    orders
        .apply(&mut store, change(1, shipment("s1", "10")))
        .unwrap();
    stock
        .apply(
            &mut store,
            change(7, rec("Stock", "k1", false, &[("qty", "3")])),
        )
        .unwrap();
    orders.flush(&mut store).unwrap();
    let payload = "x".repeat(3000);
    for i in 0..40 {
        let key = format!("k{}", i + 2);
        stock
            .apply(
                &mut store,
                change(
                    8 + i,
                    rec("Stock", &key, false, &[("payload", payload.as_str())]),
                ),
            )
            .unwrap();
    }
    drop(store);

    let store = Store::open(&log).unwrap();
    let stock = StreamApply::resume(&store, "ns", "stock", 64).unwrap();
    assert_eq!(stock.last_committed_offset(), Some(7));
    assert_eq!(
        store
            .load("ns", "Stock", "k1", &PropertyAcl::allow_all())
            .unwrap()
            .gen,
        1
    );
    assert!(store
        .load("ns", "Stock", "k2", &PropertyAcl::allow_all())
        .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resume_rejects_uncommitted_store() {
    let (dir, log) = temp_log("stream-apply-resume-pending");
    let mut store = Store::create(&log).unwrap();
    let mut apply = StreamApply::new("ns", "orders", 8).unwrap();
    apply
        .apply(&mut store, change(1, shipment("s1", "10")))
        .unwrap();
    let err = StreamApply::resume(&store, "ns", "orders", 8).unwrap_err();
    assert!(
        err.contains("committed store"),
        "expected resume to refuse pending writes, got {err}"
    );
    apply.flush(&mut store).unwrap();
    StreamApply::resume(&store, "ns", "orders", 8).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn source_name_too_long_fails_closed() {
    let err = StreamApply::new("ns", "s".repeat(2000), 8).unwrap_err();
    assert!(
        err.contains("too long"),
        "expected oversized source to fail, got {err}"
    );
}

#[test]
fn unrelated_commit_advances_superblock_during_hold() {
    let (dir, log) = temp_log("stream-apply-unrelated-commit");
    let mut store = Store::create(&log).unwrap();
    let mut apply = StreamApply::new("ns", "orders", 8).unwrap();
    apply
        .apply(&mut store, change(1, shipment("s1", "10")))
        .unwrap();
    store
        .append(rec("Customer", "c1", false, &[("region", "us")]))
        .unwrap();
    drop(store);
    let store = Store::open(&log).unwrap();
    let apply = StreamApply::resume(&store, "ns", "orders", 8).unwrap();
    assert_eq!(apply.last_committed_offset(), Some(1));
    assert!(store
        .load("ns", "Shipment", "s1", &PropertyAcl::allow_all())
        .is_ok());
    assert!(store
        .load("ns", "Customer", "c1", &PropertyAcl::allow_all())
        .is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn held_commits_keep_unflushed_pages_off_the_log() {
    let (dir, log) = temp_log("stream-apply-hold");
    let mut store = Store::create(&log).unwrap();
    let mut apply = StreamApply::new("ns", "orders", 64).unwrap();
    let payload = "x".repeat(3000);
    for i in 0..40 {
        let key = format!("b{i}");
        apply
            .apply(
                &mut store,
                change(
                    i,
                    rec("Blob", &key, false, &[("payload", payload.as_str())]),
                ),
            )
            .unwrap();
    }
    drop(store);
    let store = Store::open(&log).unwrap();
    let apply = StreamApply::resume(&store, "ns", "orders", 64).unwrap();
    assert_eq!(apply.last_committed_offset(), None);
    assert!(store
        .load("ns", "Blob", "b0", &PropertyAcl::allow_all())
        .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_apply_rejects_reserved_kinds() {
    let (dir, log) = temp_log("stream-apply-reserved");
    let mut store = Store::create(&log).unwrap();
    let mut apply = StreamApply::new("ns", "orders", 8).unwrap();
    for kind in [SOURCE_KIND, mikura::OVERLAY_KIND, mikura::SCHEMA_KIND] {
        let err = apply
            .apply(&mut store, change(1, rec(kind, "k", false, &[])))
            .unwrap_err();
        assert!(
            err.contains("reserved kind"),
            "expected reserved-kind rejection for {kind}, got {err}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn overlay_cannot_corrupt_source_cursor() {
    let (dir, log) = temp_log("stream-apply-cursor-overlay");
    let mut store = Store::create(&log).unwrap();
    let mut apply = StreamApply::new("ns", "orders", 8).unwrap();
    apply
        .apply(&mut store, change(1, shipment("s1", "10")))
        .unwrap();
    apply.flush(&mut store).unwrap();
    let err = store
        .apply_overlay(
            OverlayPatch {
                namespace: "ns".into(),
                kind: SOURCE_KIND.into(),
                key: "orders".into(),
                props: HashMap::from([("note".into(), "acked".into())]),
                cleared: Vec::new(),
                action_id: None,
            },
            "act-source-overlay".into(),
            None,
        )
        .unwrap_err();
    assert!(
        err.contains("overlay cannot describe"),
        "expected reserved-kind rejection, got {err}"
    );
    drop(store);
    let store = Store::open(&log).unwrap();
    let apply = StreamApply::resume(&store, "ns", "orders", 8).unwrap();
    assert_eq!(apply.last_committed_offset(), Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bound_overflow_fails_closed_without_drop() {
    let (dir, log) = temp_log("stream-apply-bound");
    let mut store = Store::create(&log).unwrap();
    let mut apply = StreamApply::new("ns", "orders", 1).unwrap();
    apply
        .apply(&mut store, change(1, shipment("s1", "10")))
        .unwrap();
    let err = apply
        .apply(&mut store, change(2, shipment("s2", "4")))
        .unwrap_err();
    assert!(
        err.contains("bound 1 exceeded"),
        "expected fail-closed bound error, got {err}"
    );
    apply.flush(&mut store).unwrap();
    assert!(store
        .load("ns", "Shipment", "s1", &PropertyAcl::allow_all())
        .is_ok());
    assert!(store
        .load("ns", "Shipment", "s2", &PropertyAcl::allow_all())
        .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn product_loop_apply_flush_load_is_within_seconds() {
    let (dir, log) = temp_log("stream-apply-latency");
    let mut store = Store::create(&log).unwrap();
    let mut apply = StreamApply::new("ns", "product-loop", 8).unwrap();
    apply
        .apply(
            &mut store,
            change(
                1,
                rec(
                    "component",
                    "svc-api",
                    false,
                    &[("name", "billing-api"), ("tier", "prod")],
                ),
            ),
        )
        .unwrap();
    apply
        .apply(
            &mut store,
            change(
                2,
                rec(
                    "incident",
                    "inc-1",
                    false,
                    &[("name", "elevated latency"), ("affects", "svc-api")],
                ),
            ),
        )
        .unwrap();
    apply.flush(&mut store).unwrap();

    let started = Instant::now();
    assert_eq!(
        apply
            .apply(
                &mut store,
                change(
                    3,
                    rec(
                        "incident",
                        "inc-1",
                        false,
                        &[("name", "elevated latency"), ("affects", "svc-api")],
                    ),
                ),
            )
            .unwrap(),
        ApplyStatus::Applied
    );
    apply.flush(&mut store).unwrap();
    let loaded = store
        .load("ns", "incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(loaded.gen, 2);
    assert!(
        elapsed < Duration::from_secs(STREAM_APPLY_LATENCY_TARGET_SECS),
        "product-loop apply+flush+load took {elapsed:?}, target is seconds"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
