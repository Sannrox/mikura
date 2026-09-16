use super::*;
use crate::{BatchIngest, StreamIngest};
use mikura::{ComputeError, LocalCompute};

#[test]
fn stream_flush_appends_pending_records() {
    let (dir, log) = temp_log("stream");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream
        .push(
            &mut store,
            rec(
                "Shipment",
                "s3",
                false,
                &[("order_id", "o1"), ("amount", "7")],
            ),
        )
        .unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let live = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(live.sum_amount, 17);
    stream.flush(&mut store).unwrap();
    let streamed = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(streamed.sum_amount, 17);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_bound_n_plus_one_fails_closed_without_drop() {
    let (dir, log) = temp_log("stream-bound");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let mut stream = StreamIngest::new(2).unwrap();
    stream
        .push(
            &mut store,
            rec(
                "Shipment",
                "s3",
                false,
                &[("order_id", "o1"), ("amount", "1")],
            ),
        )
        .unwrap();
    stream
        .push(
            &mut store,
            rec(
                "Shipment",
                "s4",
                false,
                &[("order_id", "o1"), ("amount", "2")],
            ),
        )
        .unwrap();
    let overflow = rec(
        "Shipment",
        "s5",
        false,
        &[("order_id", "o1"), ("amount", "99")],
    );
    let err = stream.push(&mut store, overflow).unwrap_err();
    assert!(
        err.contains("bound 2 exceeded"),
        "expected fail-closed bound error, got {err}"
    );
    let oss = ObjectSet::new(LocalCompute);
    let live = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(live.sum_amount, 13);
    stream.flush(&mut store).unwrap();
    let reopened = Store::open(&log).unwrap();
    let committed = oss.evaluate(&reopened, &fixture_request()).unwrap();
    assert_eq!(committed.sum_amount, 13);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_unflushed_records_are_not_on_rebuild() {
    let (dir, log) = temp_log("stream-unflushed");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let mut stream = StreamIngest::new(4).unwrap();
    stream
        .push(
            &mut store,
            rec(
                "Shipment",
                "s3",
                false,
                &[("order_id", "o1"), ("amount", "7")],
            ),
        )
        .unwrap();
    drop(store);
    let reopened = Store::open(&log).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let from_log = oss.evaluate(&reopened, &fixture_request()).unwrap();
    assert_eq!(from_log.sum_amount, 10);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_flush_survives_reopen() {
    let (dir, log) = temp_log("stream-flush-reopen");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let mut stream = StreamIngest::new(4).unwrap();
    stream
        .push(
            &mut store,
            rec(
                "Shipment",
                "s3",
                false,
                &[("order_id", "o1"), ("amount", "7")],
            ),
        )
        .unwrap();
    stream.flush(&mut store).unwrap();
    drop(store);
    let reopened = Store::open(&log).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let from_log = oss.evaluate(&reopened, &fixture_request()).unwrap();
    assert_eq!(from_log.sum_amount, 17);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_hidden_records_excluded_and_acl_fails_closed() {
    let (dir, log) = temp_log("stream-hidden-acl");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let mut stream = StreamIngest::new(4).unwrap();
    stream
        .push(
            &mut store,
            rec(
                "Shipment",
                "s_hidden",
                true,
                &[("order_id", "o1"), ("amount", "50")],
            ),
        )
        .unwrap();
    stream.flush(&mut store).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let visible = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(visible.sum_amount, 10);
    let mut denied = fixture_request();
    denied.acl = PropertyAcl::deny_property("Shipment", "amount");
    let err = oss.evaluate(&store, &denied).unwrap_err();
    assert!(matches!(
        err,
        ComputeError::Acl(mikura::AclError::Denied { .. })
    ));
    let _ = std::fs::remove_dir_all(&dir);
}
