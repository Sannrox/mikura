use super::*;
use std::io::{Read, Write};
use std::net::SocketAddr;

fn rec(kind: &str, key: &str, hidden: bool, props: &[(&str, &str)]) -> ObjectRecord {
    ObjectRecord {
        gen: 1,
        kind: kind.into(),
        key: key.into(),
        hidden,
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
            },
            WireHop {
                far_kind: "Shipment".into(),
                join_property: "order_id".into(),
            },
        ],
        sum_kind: "Shipment".into(),
        sum_property: "amount".into(),
        deny: Vec::new(),
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
    let err = Host::bind(addr).unwrap_err();
    assert!(err.contains("non-loopback"), "{err}");
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
                    },
                    Hop {
                        far_kind: "Shipment".into(),
                        join_property: "order_id".into(),
                    },
                ],
                sum_kind: "Shipment".into(),
                sum_property: "amount".into(),
                aggregate: Aggregate::CountAndSum,
                acl: PropertyAcl::allow_all(),
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
    let listener = Host::bind("127.0.0.1:0".parse().unwrap()).unwrap();
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
