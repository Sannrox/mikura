//! Host-process e2e suite. Spawns `mikura-host`; not same-thread `serve_one`.
//!
//! Run with `cargo test -p mikura-host --test e2e --locked`. Also included in
//! `cargo test --workspace --locked`. Distinct from `tests/integration.rs`.

use mikura::{
    Aggregate, EvaluateRequest, Hop, LocalCompute, ObjectRecord, ObjectSet, PropertyAcl,
    SchemaDescriptor, SchemaLink, Store,
};
use mikura_host::HostResponse;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

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
    stdin: Option<ChildStdin>,
}

impl HostProcess {
    fn spawn(log: &Path, bind: &str, stream_bound: usize) -> Self {
        Self::spawn_with(log, bind, stream_bound, None)
    }

    fn spawn_with(log: &Path, bind: &str, stream_bound: usize, bearer: Option<&str>) -> Self {
        Self::spawn_args(log, bind, stream_bound, bearer, &[])
    }

    fn spawn_args(
        log: &Path,
        bind: &str,
        stream_bound: usize,
        bearer: Option<&str>,
        extra: &[&str],
    ) -> Self {
        let mut args = vec![
            "--log".to_string(),
            log.to_str().expect("utf-8 log path").to_string(),
            "--bind".to_string(),
            bind.to_string(),
            "--stream-bound".to_string(),
            stream_bound.to_string(),
        ];
        if let Some(token) = bearer {
            args.push("--bearer".into());
            args.push(token.into());
        }
        args.extend(extra.iter().map(|flag| (*flag).to_string()));
        let mut child = Command::new(env!("CARGO_BIN_EXE_mikura-host"))
            .args(&args)
            .stdin(Stdio::piped())
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
        let stdin = child.stdin.take();
        Self { child, addr, stdin }
    }

    /// Close stdin and wait for a clean exit. Does not flush the stream.
    fn stop(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.child.try_wait().expect("wait host") {
                Some(status) => {
                    assert!(
                        status.success(),
                        "expected graceful host exit, got {status}"
                    );
                    return;
                }
                None if Instant::now() >= deadline => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    panic!("host did not exit after stdin close");
                }
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }

    fn rpc(&self, body: &serde_json::Value) -> HostResponse {
        let mut addr = self.addr;
        if addr.ip().is_unspecified() {
            addr.set_ip(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        }
        let mut client = TcpStream::connect(addr).expect("connect host");
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

    fn connect(&self) -> TcpStream {
        let mut addr = self.addr;
        if addr.ip().is_unspecified() {
            addr.set_ip(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        }
        let client = TcpStream::connect(addr).expect("connect host");
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        client
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        client
    }

    fn rpc_raw(&self, bytes: &[u8]) -> HostResponse {
        let mut client = self.connect();
        client.write_all(bytes).unwrap();
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
fn process_filter_restricts_roots() {
    let tmp = TempLog::new("filter");
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": [
            rec("Customer", "c1", false, &[("region", "us")]),
            rec("Customer", "c2", false, &[("region", "eu")]),
            rec("Order", "o1", false, &[("customer_id", "c1")]),
            rec("Order", "o2", false, &[("customer_id", "c2")]),
            rec("Shipment", "s1", false, &[("order_id", "o1"), ("amount", "10")]),
            rec("Shipment", "s2", false, &[("order_id", "o2"), ("amount", "7")]),
        ],
    }));
    assert!(ingest.ok, "{ingest:?}");
    let mut body = evaluate_wire();
    body["request"]["filter"] = serde_json::json!({"property": "region", "value": "us"});
    let hosted = host.rpc(&body);
    assert!(hosted.ok, "{hosted:?}");
    let evaluate = hosted.evaluate.expect("evaluate payload");
    assert_eq!(evaluate.two_hop_count, 1);
    assert_eq!(evaluate.sum_amount, 10);
    let mut empty = evaluate_wire();
    empty["request"]["filter"] = serde_json::json!({"property": "", "value": ""});
    let rejected = host.rpc(&empty);
    assert!(!rejected.ok, "{rejected:?}");
    assert!(
        rejected.error.as_deref().unwrap_or("").contains("filter"),
        "{rejected:?}"
    );
    assert!(rejected.evaluate.is_none());
}

#[test]
fn process_load_matches_in_process_and_survives_exit() {
    let tmp = TempLog::new("load");
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": fixture(),
    }));
    assert!(ingest.ok, "{ingest:?}");
    let hosted = host.rpc(&serde_json::json!({
        "op": "load",
        "kind": "Customer",
        "key": "c1"
    }));
    assert!(hosted.ok, "{hosted:?}");
    let loaded = hosted.load.expect("load payload");
    assert_eq!(loaded.key, "c1");
    assert_eq!(loaded.props.get("region").map(String::as_str), Some("us"));
    let omitted = host.rpc(&serde_json::json!({
        "op": "load",
        "kind": "Customer",
        "key": "c1",
        "deny": [{"kind": "Customer", "property": "region"}]
    }));
    assert!(omitted.ok, "{omitted:?}");
    let omitted = omitted.load.expect("load payload");
    assert!(!omitted.props.contains_key("region"));
    drop(host);
    let store = Store::open(tmp.path()).unwrap();
    assert_eq!(
        loaded,
        store
            .load("Customer", "c1", &PropertyAcl::allow_all())
            .unwrap()
    );
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
fn process_apply_action_round_trips_and_empty_id_fails_closed() {
    let tmp = TempLog::new("apply-action");
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": fixture(),
    }));
    assert!(ingest.ok, "{ingest:?}");
    let written = host.rpc(&serde_json::json!({
        "op": "apply_action",
        "id": "act-s2",
        "kind": "Shipment",
        "key": "s2",
        "props": {"order_id": "o1", "amount": "5"}
    }));
    assert!(written.ok, "{written:?}");
    let loaded = host.rpc(&serde_json::json!({
        "op": "load",
        "kind": "Shipment",
        "key": "s2"
    }));
    assert!(loaded.ok, "{loaded:?}");
    let record = loaded.load.expect("load payload");
    assert_eq!(record.action_id.as_deref(), Some("act-s2"));
    assert_eq!(record.props.get("amount").map(String::as_str), Some("5"));
    let missing = host.rpc(&serde_json::json!({
        "op": "apply_action",
        "id": "",
        "kind": "Shipment",
        "key": "s3",
        "props": {"order_id": "o1"}
    }));
    assert!(!missing.ok);
    assert!(
        missing.error.as_deref().unwrap_or("").contains("action id"),
        "{missing:?}"
    );
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

#[test]
fn process_non_loopback_bearer_accepts_matching_token() {
    let tmp = TempLog::new("bearer");
    let host = HostProcess::spawn_with(tmp.path(), "0.0.0.0:0", 8, Some("secret"));
    let denied = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": fixture(),
    }));
    assert!(!denied.ok);
    assert!(
        denied.error.as_deref().unwrap_or("").contains("bearer"),
        "{denied:?}"
    );
    assert!(denied.evaluate.is_none());
    let wrong = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "token": "nope",
        "records": fixture(),
    }));
    assert!(!wrong.ok);
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "token": "secret",
        "records": fixture(),
    }));
    assert!(ingest.ok, "{ingest:?}");
    let hosted = host.rpc(&serde_json::json!({
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
    }));
    assert!(hosted.ok, "{hosted:?}");
    let evaluate = hosted.evaluate.expect("evaluate payload");
    assert_eq!(evaluate.two_hop_count, 1);
    assert_eq!(evaluate.sum_amount, 10);
    drop(host);
    let store = Store::open(tmp.path()).unwrap();
    let live = store
        .load("Customer", "c1", &PropertyAcl::allow_all())
        .unwrap();
    assert!(!live.props.contains_key("token"));
    assert_eq!(live.props.get("region").map(String::as_str), Some("us"));
}

#[test]
fn process_loopback_bearer_enforces_token() {
    let tmp = TempLog::new("loopback-bearer");
    let host = HostProcess::spawn_with(tmp.path(), "127.0.0.1:0", 8, Some("secret"));
    let denied = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": fixture(),
    }));
    assert!(!denied.ok);
    assert!(
        denied.error.as_deref().unwrap_or("").contains("bearer"),
        "{denied:?}"
    );
    let wrong = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "token": "nope",
        "records": fixture(),
    }));
    assert!(!wrong.ok);
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "token": "secret",
        "records": fixture(),
    }));
    assert!(ingest.ok, "{ingest:?}");
}

#[test]
fn process_rejects_unknown_wire_v_and_accepts_omit_or_one() {
    let tmp = TempLog::new("wire-v");
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let omitted = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": fixture(),
    }));
    assert!(omitted.ok, "{omitted:?}");
    assert_eq!(omitted.v, mikura_host::WIRE_V);
    let versioned = host.rpc(&serde_json::json!({
        "v": 1,
        "op": "load",
        "kind": "Customer",
        "key": "c1"
    }));
    assert!(versioned.ok, "{versioned:?}");
    assert_eq!(versioned.v, mikura_host::WIRE_V);
    let unknown = host.rpc(&serde_json::json!({
        "v": 2,
        "op": "load",
        "kind": "Customer",
        "key": "c1"
    }));
    assert!(!unknown.ok, "{unknown:?}");
    assert_eq!(unknown.v, mikura_host::WIRE_V);
    assert!(
        unknown.error.as_deref().unwrap_or("").contains("wire v"),
        "{unknown:?}"
    );
}

fn product_loop_source() -> Vec<ObjectRecord> {
    vec![
        rec(
            "component",
            "svc-api",
            false,
            &[("name", "billing-api"), ("tier", "prod")],
        ),
        rec(
            "incident",
            "inc-1",
            false,
            &[("name", "elevated latency"), ("affects", "svc-api")],
        ),
    ]
}

#[test]
fn process_product_loop_baseline() {
    let tmp = TempLog::new("product-loop");
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": product_loop_source(),
    }));
    assert!(ingest.ok, "{ingest:?}");

    let loaded = host.rpc(&serde_json::json!({
        "op": "load",
        "kind": "component",
        "key": "svc-api"
    }));
    assert!(loaded.ok, "{loaded:?}");
    let svc = loaded.load.expect("load payload");
    assert_eq!(svc.kind, "component");
    assert_eq!(svc.key, "svc-api");
    assert_eq!(
        svc.props.get("name").map(String::as_str),
        Some("billing-api")
    );
    assert_eq!(svc.props.get("tier").map(String::as_str), Some("prod"));

    let filtered = host.rpc(&serde_json::json!({
        "op": "evaluate",
        "request": {
            "root_kind": "component",
            "hops": [],
            "sum_kind": "component",
            "sum_property": "tier",
            "filter": {"property": "tier", "value": "prod"},
            "object_bound": 8
        }
    }));
    assert!(filtered.ok, "{filtered:?}");
    let filtered = filtered.evaluate.expect("evaluate payload");
    assert_eq!(filtered.two_hop_count, 1);
    assert_eq!(filtered.sum_amount, 0);
    assert_eq!(filtered.objects.len(), 1);
    assert_eq!(filtered.objects[0].key, "svc-api");

    let hopped = host.rpc(&serde_json::json!({
        "op": "evaluate",
        "request": {
            "root_kind": "incident",
            "hops": [{
                "far_kind": "component",
                "join_property": "affects",
                "incoming": true
            }],
            "sum_kind": "component",
            "sum_property": "tier",
            "object_bound": 8
        }
    }));
    assert!(hopped.ok, "{hopped:?}");
    let hopped = hopped.evaluate.expect("evaluate payload");
    assert_eq!(hopped.two_hop_count, 1);
    assert_eq!(hopped.sum_amount, 0);
    assert_eq!(hopped.objects.len(), 1);
    assert_eq!(hopped.objects[0].key, "svc-api");

    let edited = host.rpc(&serde_json::json!({
        "op": "apply_action",
        "id": "act-inc-1-note",
        "kind": "incident",
        "key": "inc-1",
        "props": {
            "name": "elevated latency",
            "affects": "svc-api",
            "note": "acked"
        }
    }));
    assert!(edited.ok, "{edited:?}");
    let after_edit = host.rpc(&serde_json::json!({
        "op": "load",
        "kind": "incident",
        "key": "inc-1"
    }));
    assert!(after_edit.ok, "{after_edit:?}");
    let after_edit = after_edit.load.expect("load payload");
    assert_eq!(after_edit.action_id.as_deref(), Some("act-inc-1-note"));
    assert_eq!(
        after_edit.props.get("note").map(String::as_str),
        Some("acked")
    );

    let refresh = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": product_loop_source(),
    }));
    assert!(refresh.ok, "{refresh:?}");
    let after_refresh = host.rpc(&serde_json::json!({
        "op": "load",
        "kind": "incident",
        "key": "inc-1"
    }));
    assert!(after_refresh.ok, "{after_refresh:?}");
    let after_refresh = after_refresh.load.expect("load payload");
    assert_eq!(after_refresh.action_id, None);
    assert!(!after_refresh.props.contains_key("note"));
    drop(host);

    let store = Store::open(tmp.path()).unwrap();
    let reopened = store
        .load("component", "svc-api", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(reopened.props.get("tier").map(String::as_str), Some("prod"));
    let incident = store
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        incident.props.get("name").map(String::as_str),
        Some("elevated latency")
    );
    assert!(!incident.props.contains_key("note"));
}

fn product_loop_schema_records() -> Vec<ObjectRecord> {
    vec![
        SchemaDescriptor {
            kind: "component".into(),
            properties: vec!["name".into(), "tier".into()],
            required: vec!["name".into(), "tier".into()],
            links: vec![SchemaLink {
                name: "affects".into(),
                far_kind: "incident".into(),
                outgoing: false,
            }],
        }
        .to_record()
        .unwrap(),
        SchemaDescriptor {
            kind: "incident".into(),
            properties: vec!["affects".into(), "name".into()],
            required: vec!["name".into()],
            links: vec![SchemaLink {
                name: "affects".into(),
                far_kind: "component".into(),
                outgoing: true,
            }],
        }
        .to_record()
        .unwrap(),
    ]
}

#[test]
fn process_product_loop_with_committed_schema() {
    let tmp = TempLog::new("product-loop-schema");
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let mut records = product_loop_schema_records();
    records.extend(product_loop_source());
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": records,
    }));
    assert!(ingest.ok, "{ingest:?}");

    let loaded = host.rpc(&serde_json::json!({
        "op": "load",
        "kind": "component",
        "key": "svc-api"
    }));
    assert!(loaded.ok, "{loaded:?}");
    let svc = loaded.load.expect("load payload");
    assert_eq!(svc.props.get("tier").map(String::as_str), Some("prod"));

    let schema = host.rpc(&serde_json::json!({
        "op": "load",
        "kind": "mikura.schema",
        "key": "incident"
    }));
    assert!(schema.ok, "{schema:?}");
    let schema = schema.load.expect("schema payload");
    assert_eq!(
        schema.props.get("links").map(String::as_str),
        Some("affects:component:out:0..1")
    );

    let invalid = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": [rec("component", "svc-web", false, &[("name", "web")])]
    }));
    assert!(!invalid.ok, "{invalid:?}");
    assert!(
        invalid
            .error
            .as_deref()
            .unwrap_or("")
            .contains("missing required"),
        "{invalid:?}"
    );
    drop(host);

    let store = Store::open(tmp.path()).unwrap();
    assert_eq!(
        store.schema("component").unwrap().unwrap().kind,
        "component"
    );
    assert!(store
        .load("component", "svc-web", &PropertyAcl::allow_all())
        .is_err());
}

#[test]
fn process_evaluate_object_bound_fails_closed() {
    let tmp = TempLog::new("evaluate-bound");
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": [
            rec("component", "svc-api", false, &[("name", "billing-api"), ("tier", "prod")]),
            rec("component", "svc-web", false, &[("name", "web"), ("tier", "prod")]),
        ],
    }));
    assert!(ingest.ok, "{ingest:?}");
    let overflow = host.rpc(&serde_json::json!({
        "op": "evaluate",
        "request": {
            "root_kind": "component",
            "hops": [],
            "sum_kind": "component",
            "sum_property": "tier",
            "filter": {"property": "tier", "value": "prod"},
            "object_bound": 1
        }
    }));
    assert!(!overflow.ok, "{overflow:?}");
    assert!(
        overflow
            .error
            .as_deref()
            .unwrap_or("")
            .contains("ObjectBound"),
        "{overflow:?}"
    );
}

fn copy_log_and_optional_sidecar(src: &Path, dst: &Path) {
    std::fs::copy(src, dst).expect("copy object log");
    let src_joins = Store::join_map_path(src);
    if src_joins.is_file() {
        std::fs::copy(&src_joins, Store::join_map_path(dst)).expect("copy join sidecar");
    }
}

fn product_loop_list_wire() -> serde_json::Value {
    serde_json::json!({
        "op": "evaluate",
        "request": {
            "root_kind": "component",
            "hops": [],
            "sum_kind": "component",
            "sum_property": "tier",
            "filter": {"property": "tier", "value": "prod"},
            "object_bound": 8
        }
    })
}

fn product_loop_hop_wire() -> serde_json::Value {
    serde_json::json!({
        "op": "evaluate",
        "request": {
            "root_kind": "incident",
            "hops": [{
                "far_kind": "component",
                "join_property": "affects",
                "incoming": true
            }],
            "sum_kind": "component",
            "sum_property": "tier",
            "object_bound": 8
        }
    })
}

fn load_object(host: &HostProcess, kind: &str, key: &str) -> ObjectRecord {
    let response = host.rpc(&serde_json::json!({
        "op": "load",
        "kind": kind,
        "key": key
    }));
    assert!(response.ok, "{response:?}");
    response.load.expect("load payload")
}

fn evaluate_members(host: &HostProcess, body: &serde_json::Value) -> Vec<String> {
    let response = host.rpc(body);
    assert!(response.ok, "{response:?}");
    let evaluate = response.evaluate.expect("evaluate payload");
    evaluate
        .objects
        .into_iter()
        .map(|object| object.key)
        .collect()
}

/// Product-loop answers after ingest + overlay. Used to compare a live host,
/// a restored copy, and a dual-read reopen.
fn product_loop_overlay_view(
    host: &HostProcess,
) -> (
    ObjectRecord,
    ObjectRecord,
    ObjectRecord,
    Vec<String>,
    Vec<String>,
) {
    let service = load_object(host, "component", "svc-api");
    let incident = load_object(host, "incident", "inc-1");
    let overlay = load_object(host, "mikura.overlay", "incident/inc-1");
    let list = evaluate_members(host, &product_loop_list_wire());
    let hop = evaluate_members(host, &product_loop_hop_wire());
    (service, incident, overlay, list, hop)
}

#[test]
fn process_backup_restore_product_loop() {
    let src = TempLog::new("backup-src");
    let host = HostProcess::spawn(src.path(), "127.0.0.1:0", 8);
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": product_loop_source(),
    }));
    assert!(ingest.ok, "{ingest:?}");
    let overlay = host.rpc(&serde_json::json!({
        "op": "apply_overlay",
        "id": "act-inc-1-note",
        "kind": "incident",
        "key": "inc-1",
        "props": {"note": "acked"},
        "expected_gen": 1
    }));
    assert!(overlay.ok, "{overlay:?}");
    let live = product_loop_overlay_view(&host);
    assert_eq!(live.0.key, "svc-api");
    assert_eq!(live.0.props.get("tier").map(String::as_str), Some("prod"));
    assert_eq!(live.1.props.get("note").map(String::as_str), Some("acked"));
    assert_eq!(live.2.props.get("note").map(String::as_str), Some("acked"));
    assert_eq!(live.3, vec!["svc-api".to_string()]);
    assert_eq!(live.4, vec!["svc-api".to_string()]);
    drop(host);

    let dst = TempLog::new("backup-dst");
    copy_log_and_optional_sidecar(src.path(), dst.path());
    let restored = HostProcess::spawn(dst.path(), "127.0.0.1:0", 8);
    assert_eq!(product_loop_overlay_view(&restored), live);
    drop(restored);

    let sidecar = Store::join_map_path(dst.path());
    if sidecar.is_file() {
        std::fs::remove_file(&sidecar).expect("delete restored sidecar");
    }
    let delta = Store::join_delta_path(dst.path());
    if delta.is_file() {
        std::fs::remove_file(&delta).expect("delete restored join delta");
    }
    let rebuilt = HostProcess::spawn(dst.path(), "127.0.0.1:0", 8);
    assert_eq!(product_loop_overlay_view(&rebuilt), live);
}

#[test]
fn process_product_loop_survives_shutdown_and_reopen() {
    let tmp = TempLog::new("shutdown-reopen");
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": product_loop_source(),
    }));
    assert!(ingest.ok, "{ingest:?}");
    let overlay = host.rpc(&serde_json::json!({
        "op": "apply_overlay",
        "id": "act-inc-1-note",
        "kind": "incident",
        "key": "inc-1",
        "props": {"note": "acked"},
        "expected_gen": 1
    }));
    assert!(overlay.ok, "{overlay:?}");
    let live = product_loop_overlay_view(&host);
    assert_eq!(live.0.key, "svc-api");
    assert_eq!(live.0.props.get("tier").map(String::as_str), Some("prod"));
    assert_eq!(live.1.props.get("note").map(String::as_str), Some("acked"));
    assert_eq!(live.2.props.get("note").map(String::as_str), Some("acked"));
    assert_eq!(live.3, vec!["svc-api".to_string()]);
    assert_eq!(live.4, vec!["svc-api".to_string()]);
    host.stop();

    let upgraded = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    assert_eq!(product_loop_overlay_view(&upgraded), live);
}

#[test]
fn process_uncommitted_stream_push_invisible_after_reopen() {
    let tmp = TempLog::new("uncommitted-tail");
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": product_loop_source(),
    }));
    assert!(ingest.ok, "{ingest:?}");
    let push = host.rpc(&serde_json::json!({
        "op": "ingest_stream_push",
        "record": rec(
            "incident",
            "inc-uncommitted",
            false,
            &[("name", "tail"), ("affects", "svc-api")],
        ),
    }));
    assert!(push.ok, "{push:?}");
    let live = host.rpc(&serde_json::json!({
        "op": "load",
        "kind": "incident",
        "key": "inc-uncommitted"
    }));
    assert!(live.ok, "{live:?}");
    assert_eq!(live.load.expect("live tail").key, "inc-uncommitted");
    host.stop();

    let reopened = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let missing = reopened.rpc(&serde_json::json!({
        "op": "load",
        "kind": "incident",
        "key": "inc-uncommitted"
    }));
    assert!(!missing.ok, "{missing:?}");
    assert!(missing.load.is_none());
    let committed = load_object(&reopened, "component", "svc-api");
    assert_eq!(
        committed.props.get("tier").map(String::as_str),
        Some("prod")
    );
}

#[test]
fn process_overlay_refresh_keeps_note() {
    let tmp = TempLog::new("overlay-refresh");
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": product_loop_source(),
    }));
    assert!(ingest.ok, "{ingest:?}");
    let overlay = host.rpc(&serde_json::json!({
        "op": "apply_overlay",
        "id": "act-inc-1-note",
        "kind": "incident",
        "key": "inc-1",
        "props": {"note": "acked"},
        "expected_gen": 1
    }));
    assert!(overlay.ok, "{overlay:?}");
    let refresh = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": product_loop_source(),
    }));
    assert!(refresh.ok, "{refresh:?}");
    let loaded = host.rpc(&serde_json::json!({
        "op": "load",
        "kind": "incident",
        "key": "inc-1"
    }));
    assert!(loaded.ok, "{loaded:?}");
    let incident = loaded.load.expect("load payload");
    assert_eq!(
        incident.props.get("note").map(String::as_str),
        Some("acked")
    );
    assert_eq!(incident.action_id.as_deref(), Some("act-inc-1-note"));
    let stale = host.rpc(&serde_json::json!({
        "op": "apply_overlay",
        "id": "act-inc-1-note",
        "kind": "incident",
        "key": "inc-1",
        "props": {"note": "again"},
        "expected_gen": 1
    }));
    assert!(!stale.ok, "{stale:?}");
    assert!(
        stale
            .error
            .as_deref()
            .unwrap_or("")
            .contains("stale generation"),
        "{stale:?}"
    );
}

#[test]
fn process_oversize_request_fails_closed() {
    let tmp = TempLog::new("request-bound");
    let host = HostProcess::spawn_args(
        tmp.path(),
        "127.0.0.1:0",
        8,
        None,
        &["--request-bound", "48"],
    );
    let overflow = host.rpc_raw(&[b'x'; 64]);
    assert!(!overflow.ok, "{overflow:?}");
    assert!(
        overflow
            .error
            .as_deref()
            .unwrap_or("")
            .contains("RequestBound"),
        "{overflow:?}"
    );
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": [],
    }));
    assert!(ingest.ok, "{ingest:?}");
}

#[test]
fn process_disconnect_then_next_request_serves() {
    let tmp = TempLog::new("disconnect");
    let host = HostProcess::spawn_args(
        tmp.path(),
        "127.0.0.1:0",
        8,
        None,
        &["--request-timeout-ms", "200"],
    );
    drop(host.connect());
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": product_loop_source(),
    }));
    assert!(ingest.ok, "{ingest:?}");
    let loaded = host.rpc(&serde_json::json!({
        "op": "load",
        "kind": "component",
        "key": "svc-api"
    }));
    assert!(loaded.ok, "{loaded:?}");
    assert_eq!(loaded.load.expect("load payload").key, "svc-api");
}
