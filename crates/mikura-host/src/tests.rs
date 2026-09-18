use super::*;
use std::io::{Read, Write};
use std::net::SocketAddr;

fn rec(kind: &str, key: &str, hidden: bool, props: &[(&str, &str)]) -> ObjectRecord {
    ObjectRecord {
        gen: 1,
        kind: kind.into(),
        key: key.into(),
        hidden,
        action_id: None,
        props: props
            .iter()
            .map(|(name, value)| ((*name).into(), (*value).into()))
            .collect(),
    }
}

fn fixture() -> Vec<ObjectRecord> {
    vec![
        rec("Customer", "c0", true, &[("region", "eu")]),
        rec("Customer", "c1", false, &[("region", "us")]),
        rec("Order", "o1", false, &[("customer_id", "c1")]),
        rec("Order", "o0", true, &[("customer_id", "c0")]),
        rec(
            "Shipment",
            "s1",
            false,
            &[("order_id", "o1"), ("amount", "10")],
        ),
    ]
}

fn eval_req() -> WireEvaluate {
    WireEvaluate {
        root_kind: "Customer".into(),
        hops: vec![
            WireHop {
                far_kind: "Order".into(),
                join_property: "customer_id".into(),
                incoming: false,
            },
            WireHop {
                far_kind: "Shipment".into(),
                join_property: "order_id".into(),
                incoming: false,
            },
        ],
        sum_kind: "Shipment".into(),
        sum_property: "amount".into(),
        deny: Vec::new(),
        filter: None,
        object_bound: 0,
    }
}

fn temp_log(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("mikura-host-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("objects.mikura");
    (dir, log)
}

#[test]
fn non_loopback_bind_is_refused() {
    let addr: SocketAddr = "8.8.8.8:9".parse().unwrap();
    let err = Host::bind(addr, None).unwrap_err();
    assert!(err.contains("non-loopback"), "{err}");
    let empty = Host::bind(addr, Some("")).unwrap_err();
    assert!(empty.contains("non-loopback"), "{empty}");
}

#[test]
fn listen_stores_presented_bearer_on_loopback() {
    let (dir, log) = temp_log("listen-bearer");
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let (mut host, listener) = Host::listen(&log, 8, addr, Some("secret")).unwrap();
    drop(listener);
    let denied = host.handle_line(r#"{"op":"ingest_batch","records":[]}"#);
    assert!(!denied.ok, "{denied:?}");
    assert!(
        denied.error.as_deref().unwrap_or("").contains("bearer"),
        "{denied:?}"
    );
    let accepted = host.handle_line(r#"{"op":"ingest_batch","token":"secret","records":[]}"#);
    assert!(accepted.ok, "{accepted:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn listen_loopback_without_bearer_keeps_handle_open() {
    let (dir, log) = temp_log("listen-open");
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let (mut host, listener) = Host::listen(&log, 8, addr, None).unwrap();
    drop(listener);
    let accepted = host.handle_line(r#"{"op":"ingest_batch","records":[]}"#);
    assert!(accepted.ok, "{accepted:?}");
    assert_eq!(accepted.v, crate::WIRE_V);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn handle_line_wire_v1_omitted_or_one_unknown_fails_closed() {
    let (dir, log) = temp_log("wire-v");
    let mut host = Host::open(&log, 8).unwrap();
    let omitted = host.handle_line(r#"{"op":"ingest_batch","records":[]}"#);
    assert!(omitted.ok, "{omitted:?}");
    assert_eq!(omitted.v, crate::WIRE_V);
    let versioned = host.handle_line(r#"{"v":1,"op":"ingest_batch","records":[]}"#);
    assert!(versioned.ok, "{versioned:?}");
    assert_eq!(versioned.v, crate::WIRE_V);
    let unknown = host.handle_line(r#"{"v":2,"op":"ingest_batch","records":[]}"#);
    assert!(!unknown.ok, "{unknown:?}");
    assert_eq!(unknown.v, crate::WIRE_V);
    assert!(
        unknown.error.as_deref().unwrap_or("").contains("wire v"),
        "{unknown:?}"
    );
    let not_int = host.handle_line(r#"{"v":"1","op":"ingest_batch","records":[]}"#);
    assert!(!not_int.ok, "{not_int:?}");
    assert!(
        not_int
            .error
            .as_deref()
            .unwrap_or("")
            .contains("must be an integer"),
        "{not_int:?}"
    );
    host.require_bearer("secret").unwrap();
    let nested = host
        .handle_line(r#"{"v":1,"op":"ingest_batch","request":{"token":"secret"},"records":[]}"#);
    assert!(!nested.ok, "{nested:?}");
    assert!(
        nested.error.as_deref().unwrap_or("").contains("bearer"),
        "{nested:?}"
    );
    let sibling = host.handle_line(r#"{"v":1,"op":"ingest_batch","token":"secret","records":[]}"#);
    assert!(sibling.ok, "{sibling:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn non_loopback_bind_with_bearer_listens() {
    let listener = Host::bind("0.0.0.0:0".parse().unwrap(), Some("secret")).unwrap();
    assert!(listener.local_addr().is_ok());
}

#[test]
fn non_loopback_serve_without_bearer_fails_closed() {
    let (dir, log) = temp_log("serve-bearer");
    let listener = Host::bind("0.0.0.0:0".parse().unwrap(), Some("secret")).unwrap();
    let mut host = Host::open(&log, 4).unwrap();
    let err = host.serve(listener).unwrap_err();
    assert!(err.contains("non-loopback"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bearer_line_rejects_missing_or_wrong_token() {
    let (dir, log) = temp_log("bearer-line");
    let mut host = Host::open(&log, 4).unwrap();
    host.handle(HostRequest::IngestBatch { records: fixture() });
    host.require_bearer("secret").unwrap();
    let denied = host.handle_line(
        &serde_json::json!({
            "op": "evaluate",
            "request": {
                "root_kind": "Customer",
                "hops": [
                    {"far_kind": "Order", "join_property": "customer_id"},
                    {"far_kind": "Shipment", "join_property": "order_id"}
                ],
                "sum_kind": "Shipment",
                "sum_property": "amount"
            }
        })
        .to_string(),
    );
    assert!(!denied.ok);
    assert!(
        denied.error.as_deref().unwrap_or("").contains("bearer"),
        "{denied:?}"
    );
    assert!(denied.evaluate.is_none());
    let wrong = host.handle_line(
        &serde_json::json!({
            "op": "evaluate",
            "token": "nope",
            "request": {
                "root_kind": "Customer",
                "hops": [
                    {"far_kind": "Order", "join_property": "customer_id"},
                    {"far_kind": "Shipment", "join_property": "order_id"}
                ],
                "sum_kind": "Shipment",
                "sum_property": "amount"
            }
        })
        .to_string(),
    );
    assert!(!wrong.ok);
    let ok = host.handle_line(
        &serde_json::json!({
            "op": "evaluate",
            "token": "secret",
            "request": {
                "root_kind": "Customer",
                "hops": [
                    {"far_kind": "Order", "join_property": "customer_id"},
                    {"far_kind": "Shipment", "join_property": "order_id"}
                ],
                "sum_kind": "Shipment",
                "sum_property": "amount"
            }
        })
        .to_string(),
    );
    assert!(ok.ok, "{ok:?}");
    assert_eq!(ok.evaluate.unwrap().sum_amount, 10);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn loopback_ingest_evaluate_matches_in_process() {
    let (dir, log) = temp_log("match");
    let mut host = Host::open(&log, 8).unwrap();
    let ingest = host.handle(HostRequest::IngestBatch { records: fixture() });
    assert!(ingest.ok, "{ingest:?}");
    let hosted = host.handle(HostRequest::Evaluate {
        request: eval_req(),
    });
    assert!(hosted.ok, "{hosted:?}");
    let hosted = hosted.evaluate.unwrap();

    let store = Store::open(&log).unwrap();
    let direct = ObjectSet::new(LocalCompute)
        .evaluate(
            &store,
            &EvaluateRequest {
                root_kind: "Customer".into(),
                hops: vec![
                    Hop {
                        far_kind: "Order".into(),
                        join_property: "customer_id".into(),
                        incoming: false,
                    },
                    Hop {
                        far_kind: "Shipment".into(),
                        join_property: "order_id".into(),
                        incoming: false,
                    },
                ],
                sum_kind: "Shipment".into(),
                sum_property: "amount".into(),
                aggregate: Aggregate::CountAndSum,
                acl: PropertyAcl::allow_all(),
                filter: None,
                object_bound: 0,
            },
        )
        .unwrap();
    assert_eq!(hosted.two_hop_count, direct.two_hop_count);
    assert_eq!(hosted.sum_amount, direct.sum_amount);
    assert_eq!(hosted.sum_amount, 10);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_overflow_fails_closed() {
    let (dir, log) = temp_log("bound");
    let mut host = Host::open(&log, 1).unwrap();
    let first = host.handle(HostRequest::IngestStreamPush {
        record: rec(
            "Shipment",
            "s3",
            false,
            &[("order_id", "o1"), ("amount", "1")],
        ),
    });
    assert!(first.ok, "{first:?}");
    let overflow = host.handle(HostRequest::IngestStreamPush {
        record: rec(
            "Shipment",
            "s4",
            false,
            &[("order_id", "o1"), ("amount", "99")],
        ),
    });
    assert!(!overflow.ok);
    assert!(
        overflow.error.as_deref().unwrap_or("").contains("bound"),
        "{overflow:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn evaluate_acl_fails_closed() {
    let (dir, log) = temp_log("acl");
    let mut host = Host::open(&log, 4).unwrap();
    host.handle(HostRequest::IngestBatch { records: fixture() });
    let mut request = eval_req();
    request.deny.push(WireDeny {
        kind: "Shipment".into(),
        property: "amount".into(),
    });
    let denied = host.handle(HostRequest::Evaluate { request });
    assert!(!denied.ok);
    assert!(
        denied.error.as_deref().unwrap_or("").contains("Denied"),
        "{denied:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn loopback_tcp_roundtrip() {
    let (dir, log) = temp_log("tcp");
    let listener = Host::bind("127.0.0.1:0".parse().unwrap(), None).unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let mut host = Host::open(&log, 4).unwrap();
        host.handle(HostRequest::IngestBatch { records: fixture() });
        let (stream, _) = listener.accept().unwrap();
        host.serve_one(stream).unwrap();
    });
    let mut client = TcpStream::connect(addr).unwrap();
    let line = serde_json::to_string(&serde_json::json!({
        "op": "evaluate",
        "request": {
            "root_kind": "Customer",
            "hops": [
                {"far_kind": "Order", "join_property": "customer_id"},
                {"far_kind": "Shipment", "join_property": "order_id"}
            ],
            "sum_kind": "Shipment",
            "sum_property": "amount"
        }
    }))
    .unwrap();
    client.write_all(line.as_bytes()).unwrap();
    client.write_all(b"\n").unwrap();
    let mut buf = String::new();
    client.read_to_string(&mut buf).unwrap();
    let response: HostResponse = serde_json::from_str(buf.trim()).unwrap();
    assert!(response.ok, "{response:?}");
    let evaluate = response.evaluate.unwrap();
    assert_eq!(evaluate.two_hop_count, 1);
    assert_eq!(evaluate.sum_amount, 10);
    handle.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn evaluate_filter_matches_in_process() {
    let (dir, log) = temp_log("filter");
    let mut host = Host::open(&log, 8).unwrap();
    host.handle(HostRequest::IngestBatch {
        records: vec![
            rec("Customer", "c1", false, &[("region", "us")]),
            rec("Customer", "c2", false, &[("region", "eu")]),
            rec("Order", "o1", false, &[("customer_id", "c1")]),
            rec("Order", "o2", false, &[("customer_id", "c2")]),
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
                &[("order_id", "o2"), ("amount", "7")],
            ),
        ],
    });
    let mut request = eval_req();
    request.filter = Some(WireFilter {
        property: "region".into(),
        value: "us".into(),
    });
    let hosted = host.handle(HostRequest::Evaluate { request });
    assert!(hosted.ok, "{hosted:?}");
    let hosted = hosted.evaluate.unwrap();
    assert_eq!(hosted.two_hop_count, 1);
    assert_eq!(hosted.sum_amount, 10);
    let mut empty = eval_req();
    empty.filter = Some(WireFilter {
        property: String::new(),
        value: String::new(),
    });
    let rejected = host.handle(HostRequest::Evaluate { request: empty });
    assert!(!rejected.ok, "{rejected:?}");
    assert!(
        rejected.error.as_deref().unwrap_or("").contains("filter"),
        "{rejected:?}"
    );
    assert!(rejected.evaluate.is_none());
    let omitted = host.handle(HostRequest::Evaluate {
        request: eval_req(),
    });
    assert!(omitted.ok, "{omitted:?}");
    let omitted = omitted.evaluate.unwrap();
    assert_eq!(omitted.two_hop_count, 2);
    assert_eq!(omitted.sum_amount, 17);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn apply_action_stores_provenance_and_empty_id_fails_closed() {
    let (dir, log) = temp_log("apply-action");
    let mut host = Host::open(&log, 8).unwrap();
    host.handle(HostRequest::IngestBatch { records: fixture() });
    let omitted = host.handle(HostRequest::Load {
        kind: "Shipment".into(),
        key: "s1".into(),
        deny: Vec::new(),
    });
    assert!(omitted.ok, "{omitted:?}");
    assert_eq!(omitted.load.expect("load payload").action_id, None);
    let written = host.handle(HostRequest::ApplyAction {
        id: "act-s2".into(),
        kind: "Shipment".into(),
        key: "s2".into(),
        props: [
            ("order_id".into(), "o1".into()),
            ("amount".into(), "5".into()),
        ]
        .into_iter()
        .collect(),
    });
    assert!(written.ok, "{written:?}");
    let loaded = host.handle(HostRequest::Load {
        kind: "Shipment".into(),
        key: "s2".into(),
        deny: Vec::new(),
    });
    assert!(loaded.ok, "{loaded:?}");
    let loaded = loaded.load.expect("load payload");
    assert_eq!(loaded.action_id.as_deref(), Some("act-s2"));
    assert_eq!(loaded.props.get("amount").map(String::as_str), Some("5"));
    let missing = host.handle(HostRequest::ApplyAction {
        id: String::new(),
        kind: "Shipment".into(),
        key: "s3".into(),
        props: [("order_id".into(), "o1".into())].into_iter().collect(),
    });
    assert!(!missing.ok);
    assert!(
        missing.error.as_deref().unwrap_or("").contains("action id"),
        "{missing:?}"
    );
    let empty_ingest = host.handle(HostRequest::IngestBatch {
        records: vec![{
            let mut record = rec("Shipment", "s4", false, &[("order_id", "o1")]);
            record.action_id = Some(String::new());
            record
        }],
    });
    assert!(!empty_ingest.ok);
    assert!(
        empty_ingest
            .error
            .as_deref()
            .unwrap_or("")
            .contains("empty action id"),
        "{empty_ingest:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn load_matches_in_process_and_omits_denied() {
    let (dir, log) = temp_log("load");
    let mut host = Host::open(&log, 8).unwrap();
    host.handle(HostRequest::IngestBatch { records: fixture() });
    let hosted = host.handle(HostRequest::Load {
        kind: "Customer".into(),
        key: "c1".into(),
        deny: Vec::new(),
    });
    assert!(hosted.ok, "{hosted:?}");
    let loaded = hosted.load.expect("load payload");
    let store = Store::open(&log).unwrap();
    assert_eq!(
        loaded,
        store
            .load("Customer", "c1", &PropertyAcl::allow_all())
            .unwrap()
    );
    let omitted = host.handle(HostRequest::Load {
        kind: "Customer".into(),
        key: "c1".into(),
        deny: vec![WireDeny {
            kind: "Customer".into(),
            property: "region".into(),
        }],
    });
    assert!(omitted.ok, "{omitted:?}");
    let omitted = omitted.load.expect("load payload");
    assert!(!omitted.props.contains_key("region"));
    assert_ne!(omitted.props.get("region"), Some(&String::new()));
    let wire = serde_json::to_value(&omitted).expect("load wire");
    assert!(
        wire.get("props")
            .and_then(|props| props.get("region"))
            .is_none(),
        "{wire}"
    );
    let mut denied_eval = eval_req();
    denied_eval.deny.push(WireDeny {
        kind: "Shipment".into(),
        property: "amount".into(),
    });
    let denied_eval = host.handle(HostRequest::Evaluate {
        request: denied_eval,
    });
    assert!(!denied_eval.ok, "{denied_eval:?}");
    assert!(
        denied_eval
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Denied"),
        "{denied_eval:?}"
    );
    let missing = host.handle(HostRequest::Load {
        kind: "Customer".into(),
        key: "nope".into(),
        deny: Vec::new(),
    });
    assert!(!missing.ok);
    assert!(missing.load.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}
