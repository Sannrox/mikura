//! Host-process e2e suite. Spawns `mikura-host`; not same-thread `serve_one`.
//!
//! Run with `cargo test -p mikura-host --test e2e --locked`. Also included in
//! `cargo test --workspace --locked`. Distinct from `tests/integration.rs`.

use mikura::{
    Aggregate, EvaluateRequest, Hop, LocalCompute, ObjectRecord, ObjectSet, PropertyAcl, Store,
};
use mikura_host::HostResponse;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct TempLog {
    dir: PathBuf,
    log: PathBuf,
}

impl TempLog {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("mikura-host-e2e-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp log directory");
        let log = dir.join("objects.mikura");
        Self { dir, log }
    }

    fn path(&self) -> &Path {
        &self.log
    }
}

impl Drop for TempLog {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct HostProcess {
    child: Child,
    addr: SocketAddr,
}

impl HostProcess {
    fn spawn(log: &Path, bind: &str, stream_bound: usize) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mikura-host"))
            .args([
                "--log",
                log.to_str().expect("utf-8 log path"),
                "--bind",
                bind,
                "--stream-bound",
                &stream_bound.to_string(),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn mikura-host");
        let stdout = child.stdout.take().expect("host stdout");
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read listening line");
        let addr = line
            .strip_prefix("listening ")
            .map(str::trim)
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| {
                let _ = child.kill();
                let stderr = child.stderr.take().map(|mut err| {
                    let mut buf = String::new();
                    let _ = err.read_to_string(&mut buf);
                    buf
                });
                panic!("expected listening address, got {line:?}, stderr={stderr:?}");
            });
        Self { child, addr }
    }

    fn rpc(&self, body: &serde_json::Value) -> HostResponse {
        let mut client = TcpStream::connect(self.addr).expect("connect host");
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        client
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let line = serde_json::to_string(body).unwrap();
        client.write_all(line.as_bytes()).unwrap();
        client.write_all(b"\n").unwrap();
        let mut buf = String::new();
        client.read_to_string(&mut buf).unwrap();
        serde_json::from_str(buf.trim()).expect("host JSON response")
    }
}

impl Drop for HostProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

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

fn evaluate_wire() -> serde_json::Value {
    serde_json::json!({
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
}

fn in_process_evaluate(log: &Path) -> (usize, i64) {
    let store = Store::open(log).unwrap();
    let response = ObjectSet::new(LocalCompute)
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
    (response.two_hop_count, response.sum_amount)
}

#[test]
fn process_wire_matches_in_process_and_survives_exit() {
    let tmp = TempLog::new("match-reopen");
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": fixture(),
    }));
    assert!(ingest.ok, "{ingest:?}");
    let hosted = host.rpc(&evaluate_wire());
    assert!(hosted.ok, "{hosted:?}");
    let evaluate = hosted.evaluate.expect("evaluate payload");
    assert_eq!(evaluate.two_hop_count, 1);
    assert_eq!(evaluate.sum_amount, 10);
    drop(host);

    let (count, sum) = in_process_evaluate(tmp.path());
    assert_eq!(count, evaluate.two_hop_count);
    assert_eq!(sum, evaluate.sum_amount);
}

#[test]
fn process_stream_overflow_fails_closed() {
    let tmp = TempLog::new("stream");
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 1);
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": fixture(),
    }));
    assert!(ingest.ok, "{ingest:?}");
    let push = host.rpc(&serde_json::json!({
        "op": "ingest_stream_push",
        "record": rec(
            "Shipment",
            "s3",
            false,
            &[("order_id", "o1"), ("amount", "2")],
        ),
    }));
    assert!(push.ok, "{push:?}");
    let overflow = host.rpc(&serde_json::json!({
        "op": "ingest_stream_push",
        "record": rec(
            "Shipment",
            "s4",
            false,
            &[("order_id", "o1"), ("amount", "99")],
        ),
    }));
    assert!(!overflow.ok);
    assert!(
        overflow.error.as_deref().unwrap_or("").contains("bound"),
        "{overflow:?}"
    );
    let flush = host.rpc(&serde_json::json!({ "op": "ingest_stream_flush" }));
    assert!(flush.ok, "{flush:?}");
    let hosted = host.rpc(&evaluate_wire());
    assert!(hosted.ok, "{hosted:?}");
    let evaluate = hosted.evaluate.expect("evaluate payload");
    assert_eq!(evaluate.sum_amount, 12);
    assert!(overflow.evaluate.is_none());
}

#[test]
fn process_acl_deny_returns_error_not_guess() {
    let tmp = TempLog::new("acl");
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 4);
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": fixture(),
    }));
    assert!(ingest.ok, "{ingest:?}");
    let denied = host.rpc(&serde_json::json!({
        "op": "evaluate",
        "request": {
            "root_kind": "Customer",
            "hops": [
                {"far_kind": "Order", "join_property": "customer_id"},
                {"far_kind": "Shipment", "join_property": "order_id"}
            ],
            "sum_kind": "Shipment",
            "sum_property": "amount",
            "deny": [{"kind": "Shipment", "property": "amount"}]
        }
    }));
    assert!(!denied.ok);
    assert!(
        denied.error.as_deref().unwrap_or("").contains("Denied"),
        "{denied:?}"
    );
    assert!(denied.evaluate.is_none());
}

#[test]
fn process_refuses_non_loopback_bind() {
    let tmp = TempLog::new("non-loopback");
    let output = Command::new(env!("CARGO_BIN_EXE_mikura-host"))
        .args([
            "--log",
            tmp.path().to_str().expect("utf-8 log path"),
            "--bind",
            "8.8.8.8:9",
        ])
        .output()
        .expect("run mikura-host");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("non-loopback"),
        "expected refuse, got {stderr}"
    );
}
