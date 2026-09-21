//! Host-process e2e suite. Spawns `mikura-host`; not same-thread `serve_one`.
//!
//! Run with `cargo test -p mikura-host --test e2e --locked`. Also included in
//! `cargo test --workspace --locked`. Distinct from `tests/integration.rs`.

use mikura::{
    Aggregate, EvaluateRequest, Hop, LocalCompute, ObjectRecord, ObjectSet, PropertyAcl,
    PropertyType, SchemaDescriptor, SchemaLink, Store,
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
                        predicate: None,
                    },
                    Hop {
                        far_kind: "Shipment".into(),
                        join_property: "order_id".into(),
                        incoming: false,
                        predicate: None,
                    },
                ],
                sum_kind: "Shipment".into(),
                sum_property: "amount".into(),
                aggregate: Aggregate::CountAndSum,
                acl: PropertyAcl::allow_all(),
                filter: None,
                predicate: None,
                object_bound: 0,
                sort: None,
                page_size: 0,
                cursor: None,
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
    let replay = host.rpc(&serde_json::json!({
        "op": "apply_action",
        "id": "act-s2",
        "kind": "Shipment",
        "key": "s2",
        "props": {"order_id": "o1", "amount": "5"}
    }));
    assert!(replay.ok, "{replay:?}");
    let replayed = host.rpc(&serde_json::json!({
        "op": "load",
        "kind": "Shipment",
        "key": "s2"
    }));
    assert_eq!(replayed.load.expect("load payload").gen, record.gen);
    let conflict = host.rpc(&serde_json::json!({
        "op": "apply_action",
        "id": "act-s2",
        "kind": "Shipment",
        "key": "s2",
        "props": {"order_id": "o1", "amount": "9"}
    }));
    assert!(!conflict.ok);
    assert!(
        conflict
            .error
            .as_deref()
            .unwrap_or("")
            .contains("body conflict"),
        "{conflict:?}"
    );
    let other = host.rpc(&serde_json::json!({
        "op": "apply_action",
        "id": "act-s2",
        "kind": "Shipment",
        "key": "s1",
        "props": {"order_id": "o1", "amount": "5"}
    }));
    assert!(!other.ok);
    assert!(
        other
            .error
            .as_deref()
            .unwrap_or("")
            .contains("already committed"),
        "{other:?}"
    );
    drop(host);
    let reopened = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let after_open = reopened.rpc(&serde_json::json!({
        "op": "apply_action",
        "id": "act-s2",
        "kind": "Shipment",
        "key": "s2",
        "props": {"order_id": "o1", "amount": "5"}
    }));
    assert!(after_open.ok, "{after_open:?}");
    let after_open = reopened.rpc(&serde_json::json!({
        "op": "load",
        "kind": "Shipment",
        "key": "s2"
    }));
    assert_eq!(after_open.load.expect("load payload").gen, record.gen);
    let missing = reopened.rpc(&serde_json::json!({
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
fn process_ingest_hide_under_a_claimed_action_id_conflicts_and_replays() {
    let tmp = TempLog::new("ingest-action-hide");
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let mut visible = rec(
        "Shipment",
        "s2",
        false,
        &[("order_id", "o1"), ("amount", "5")],
    );
    visible.action_id = Some("act-ingest-1".into());
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": [&visible],
    }));
    assert!(ingest.ok, "{ingest:?}");
    let load = || {
        host.rpc(&serde_json::json!({
            "op": "load",
            "kind": "Shipment",
            "key": "s2"
        }))
        .load
        .expect("load payload")
    };
    let visible_gen = load().gen;

    let mut conflicting = visible.clone();
    conflicting.hidden = true;
    conflicting.props.insert("amount".into(), "999".into());
    let conflict = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": [&conflicting],
    }));
    assert!(!conflict.ok, "{conflict:?}");
    assert!(
        conflict
            .error
            .as_deref()
            .unwrap_or("")
            .contains("body conflict"),
        "{conflict:?}"
    );
    let unchanged = load();
    assert!(!unchanged.hidden);
    assert_eq!(unchanged.gen, visible_gen);

    let mut hide = visible.clone();
    hide.hidden = true;
    let hidden = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": [&hide],
    }));
    assert!(hidden.ok, "{hidden:?}");
    let hidden_gen = load().gen;
    assert_eq!(hidden_gen, visible_gen + 1);
    let retry = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": [&hide],
    }));
    assert!(retry.ok, "{retry:?}");
    assert_eq!(load().gen, hidden_gen);
    drop(host);

    let reopened = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let after_open = reopened.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": [&hide],
    }));
    assert!(after_open.ok, "{after_open:?}");
    let reloaded = reopened
        .rpc(&serde_json::json!({
            "op": "load",
            "kind": "Shipment",
            "key": "s2"
        }))
        .load
        .expect("load payload");
    assert_eq!(reloaded.gen, hidden_gen);
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
    let retried = host.rpc(&serde_json::json!({
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
    assert!(retried.ok, "{retried:?}");
    let after_retry = host.rpc(&serde_json::json!({
        "op": "load",
        "kind": "incident",
        "key": "inc-1"
    }));
    assert!(after_retry.ok, "{after_retry:?}");
    let after_retry = after_retry.load.expect("load payload");
    assert_eq!(after_retry.gen, after_edit.gen);
    assert_eq!(after_retry.action_id.as_deref(), Some("act-inc-1-note"));
    assert_eq!(
        after_retry.props.get("note").map(String::as_str),
        Some("acked")
    );
    drop(host);
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let retried_open = host.rpc(&serde_json::json!({
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
    assert!(retried_open.ok, "{retried_open:?}");
    let after_open = host.rpc(&serde_json::json!({
        "op": "load",
        "kind": "incident",
        "key": "inc-1"
    }));
    assert!(after_open.ok, "{after_open:?}");
    let after_open = after_open.load.expect("load payload");
    assert_eq!(after_open.gen, after_edit.gen);
    assert_eq!(after_open.action_id.as_deref(), Some("act-inc-1-note"));
    assert_eq!(
        after_open.props.get("note").map(String::as_str),
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
            sums: Vec::new(),
            types: Vec::new(),
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
            sums: Vec::new(),
            types: Vec::new(),
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

fn product_loop_hide_view(
    host: &HostProcess,
) -> (ObjectRecord, ObjectRecord, Vec<String>, Vec<String>) {
    let service = load_object(host, "component", "svc-api");
    let incident = load_object(host, "incident", "inc-1");
    let list = evaluate_members(host, &product_loop_list_wire());
    let hop = evaluate_members(host, &product_loop_hop_wire());
    (service, incident, list, hop)
}

#[test]
fn process_product_loop_hide() {
    let tmp = TempLog::new("product-loop-hide");
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": product_loop_source(),
    }));
    assert!(ingest.ok, "{ingest:?}");
    let hidden = host.rpc(&serde_json::json!({
        "op": "hide",
        "kind": "incident",
        "key": "inc-1"
    }));
    assert!(hidden.ok, "{hidden:?}");
    let live = product_loop_hide_view(&host);
    assert_eq!(live.0.key, "svc-api");
    assert!(live.1.hidden);
    assert_eq!(
        live.1.props.get("name").map(String::as_str),
        Some("elevated latency")
    );
    assert_eq!(live.2, vec!["svc-api".to_string()]);
    assert!(live.3.is_empty());
    let unknown = host.rpc(&serde_json::json!({
        "op": "hide",
        "kind": "incident",
        "key": "nope"
    }));
    assert!(!unknown.ok, "{unknown:?}");
    assert!(
        unknown
            .error
            .as_deref()
            .unwrap_or("")
            .contains("unknown identity"),
        "{unknown:?}"
    );
    let denied = host.rpc(&serde_json::json!({
        "op": "hide",
        "kind": "component",
        "key": "svc-api",
        "deny": [{"kind": "component", "property": "tier"}]
    }));
    assert!(!denied.ok, "{denied:?}");
    assert!(
        denied.error.as_deref().unwrap_or("").contains("Denied"),
        "{denied:?}"
    );
    let unknown_v = host.rpc(&serde_json::json!({
        "v": 2,
        "op": "hide",
        "kind": "incident",
        "key": "inc-1"
    }));
    assert!(!unknown_v.ok, "{unknown_v:?}");
    assert!(
        unknown_v.error.as_deref().unwrap_or("").contains("wire v"),
        "{unknown_v:?}"
    );
    drop(host);

    let reopened = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    assert_eq!(product_loop_hide_view(&reopened), live);
    drop(reopened);

    let sidecar = Store::join_map_path(tmp.path());
    if sidecar.is_file() {
        std::fs::remove_file(&sidecar).expect("delete sidecar");
    }
    let delta = Store::join_delta_path(tmp.path());
    if delta.is_file() {
        std::fs::remove_file(&delta).expect("delete join delta");
    }
    let rebuilt = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    assert_eq!(product_loop_hide_view(&rebuilt), live);
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
        "id": "act-inc-1-note-2",
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

fn incident_typed_schema_record() -> ObjectRecord {
    SchemaDescriptor {
        kind: "incident".into(),
        properties: vec![
            "affects".into(),
            "cost".into(),
            "name".into(),
            "open".into(),
            "opened_at".into(),
            "priority".into(),
        ],
        required: vec!["name".into()],
        links: vec![SchemaLink {
            name: "affects".into(),
            far_kind: "component".into(),
            outgoing: true,
        }],
        sums: Vec::new(),
        types: vec![
            ("cost".into(), PropertyType::Decimal { scale: 2 }),
            ("open".into(), PropertyType::Boolean),
            ("opened_at".into(), PropertyType::Timestamp),
            ("priority".into(), PropertyType::Integer),
        ],
    }
    .to_record()
    .unwrap()
}

#[test]
fn process_typed_scalars_round_trip() {
    let tmp = TempLog::new("typed-values");
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": [
            incident_typed_schema_record(),
            rec(
                "incident",
                "inc-1",
                false,
                &[
                    ("name", "elevated latency"),
                    ("affects", "svc-api"),
                    ("open", "true"),
                    ("priority", "2"),
                    ("opened_at", "2026-09-19T18:00:00+01:00"),
                    ("cost", "1500.00"),
                ],
            ),
        ]
    }));
    assert!(ingest.ok, "{ingest:?}");
    let loaded = host.rpc(&serde_json::json!({
        "op": "load",
        "kind": "incident",
        "key": "inc-1"
    }));
    assert!(loaded.ok, "{loaded:?}");
    let loaded = loaded.load.expect("load payload");
    assert_eq!(loaded.props.get("open").map(String::as_str), Some("true"));
    assert_eq!(
        loaded.props.get("opened_at").map(String::as_str),
        Some("2026-09-19T17:00:00.000Z")
    );
    let invalid = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": [rec("incident", "inc-2", false, &[("name", "n"), ("open", "TRUE")])]
    }));
    assert!(!invalid.ok, "{invalid:?}");
    let overlay = host.rpc(&serde_json::json!({
        "op": "apply_overlay",
        "id": "act-inc-1-open",
        "kind": "incident",
        "key": "inc-1",
        "props": {"open": "false"}
    }));
    assert!(overlay.ok, "{overlay:?}");
    drop(host);

    let reopened = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let again = reopened.rpc(&serde_json::json!({
        "op": "load",
        "kind": "incident",
        "key": "inc-1"
    }));
    assert!(again.ok, "{again:?}");
    let again = again.load.expect("load payload");
    assert_eq!(again.props.get("open").map(String::as_str), Some("false"));
    assert_eq!(again.props.get("cost").map(String::as_str), Some("1500.00"));
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(tmp.path())).unwrap();
    let replayed = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let rebuilt = replayed.rpc(&serde_json::json!({
        "op": "load",
        "kind": "incident",
        "key": "inc-1"
    }));
    assert!(rebuilt.ok, "{rebuilt:?}");
    let rebuilt = rebuilt.load.expect("load payload");
    assert_eq!(
        rebuilt.props.get("opened_at").map(String::as_str),
        Some("2026-09-19T17:00:00.000Z")
    );
}

fn evolving_incident_schema() -> SchemaDescriptor {
    SchemaDescriptor {
        kind: "incident".into(),
        properties: vec!["affects".into(), "name".into(), "note".into()],
        required: vec!["name".into()],
        links: vec![SchemaLink {
            name: "affects".into(),
            far_kind: "component".into(),
            outgoing: true,
        }],
        sums: Vec::new(),
        types: Vec::new(),
    }
}

#[test]
fn process_schema_evolution_preserves_meaning() {
    let tmp = TempLog::new("schema-evolution");
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let initial = evolving_incident_schema();
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": [
            initial.to_record().unwrap(),
            rec(
                "incident",
                "inc-1",
                false,
                &[
                    ("name", "elevated latency"),
                    ("affects", "svc-api"),
                    ("note", "acked"),
                ],
            ),
        ]
    }));
    assert!(ingest.ok, "{ingest:?}");

    let mut recast = initial.clone();
    recast.types = vec![("name".into(), PropertyType::Integer)];
    let recast_rpc = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": [recast.to_record().unwrap()]
    }));
    assert!(!recast_rpc.ok, "{recast_rpc:?}");
    assert!(
        recast_rpc
            .error
            .as_deref()
            .unwrap_or("")
            .contains("cannot recast name"),
        "{recast_rpc:?}"
    );

    let mut evolved = initial.clone();
    evolved
        .properties
        .extend(["open".into(), "priority".into()]);
    evolved.types = vec![
        ("open".into(), PropertyType::Boolean),
        ("priority".into(), PropertyType::Integer),
    ];
    evolved.links.push(SchemaLink {
        name: "opened_by".into(),
        far_kind: "component".into(),
        outgoing: false,
    });
    evolved.sums = vec!["name".into()];
    let evolve = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": [evolved.to_record().unwrap()]
    }));
    assert!(evolve.ok, "{evolve:?}");
    let overlay = host.rpc(&serde_json::json!({
        "op": "apply_overlay",
        "id": "act-inc-1-note",
        "kind": "incident",
        "key": "inc-1",
        "props": {"note": "paged"}
    }));
    assert!(overlay.ok, "{overlay:?}");
    let action = host.rpc(&serde_json::json!({
        "op": "apply_action",
        "id": "act-inc-1-body",
        "kind": "incident",
        "key": "inc-1",
        "props": {
            "name": "elevated latency",
            "affects": "svc-api",
            "note": "paged",
            "open": "true"
        }
    }));
    assert!(action.ok, "{action:?}");

    let mut shrunk = evolved.clone();
    shrunk.properties.retain(|name| name != "note");
    let shrink = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": [shrunk.to_record().unwrap()]
    }));
    assert!(shrink.ok, "{shrink:?}");
    let stale_overlay = host.rpc(&serde_json::json!({
        "op": "apply_overlay",
        "id": "act-inc-1-note-2",
        "kind": "incident",
        "key": "inc-1",
        "props": {"note": "stale"}
    }));
    assert!(!stale_overlay.ok, "{stale_overlay:?}");
    let stale_action = host.rpc(&serde_json::json!({
        "op": "apply_action",
        "id": "act-inc-1-stale",
        "kind": "incident",
        "key": "inc-1",
        "props": {
            "name": "elevated latency",
            "affects": "svc-api",
            "note": "paged"
        }
    }));
    assert!(!stale_action.ok, "{stale_action:?}");
    let replay = host.rpc(&serde_json::json!({
        "op": "apply_action",
        "id": "act-inc-1-body",
        "kind": "incident",
        "key": "inc-1",
        "props": {
            "name": "elevated latency",
            "affects": "svc-api",
            "note": "paged",
            "open": "true"
        }
    }));
    assert!(!replay.ok, "{replay:?}");
    assert!(
        replay
            .error
            .as_deref()
            .unwrap_or("")
            .contains("unknown property"),
        "{replay:?}"
    );

    let mut hidden = shrunk.to_record().unwrap();
    hidden.hidden = true;
    let hide = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": [hidden]
    }));
    assert!(hide.ok, "{hide:?}");
    let restore = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": [shrunk.to_record().unwrap()]
    }));
    assert!(restore.ok, "{restore:?}");
    drop(host);

    let reopened = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let schema = reopened.rpc(&serde_json::json!({
        "op": "load",
        "kind": "mikura.schema",
        "key": "incident"
    }));
    assert!(schema.ok, "{schema:?}");
    let schema = schema.load.expect("schema payload");
    assert!(schema
        .props
        .get("properties")
        .unwrap()
        .split(',')
        .any(|name| name == "open"));
    assert!(!schema
        .props
        .get("properties")
        .unwrap()
        .split(',')
        .any(|name| name == "note"));
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(tmp.path())).unwrap();
    let replayed = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let loaded = replayed.rpc(&serde_json::json!({
        "op": "load",
        "kind": "incident",
        "key": "inc-1"
    }));
    assert!(loaded.ok, "{loaded:?}");
    let loaded = loaded.load.expect("load payload");
    assert_eq!(loaded.props.get("open").map(String::as_str), Some("true"));
    assert_eq!(loaded.props.get("note").map(String::as_str), Some("paged"));
}

fn query_component_schema() -> ObjectRecord {
    SchemaDescriptor {
        kind: "component".into(),
        properties: vec!["name".into(), "tier".into()],
        required: vec!["name".into(), "tier".into()],
        links: Vec::new(),
        sums: Vec::new(),
        types: Vec::new(),
    }
    .to_record()
    .unwrap()
}

fn query_incident_schema() -> ObjectRecord {
    SchemaDescriptor {
        kind: "incident".into(),
        properties: vec![
            "affects".into(),
            "name".into(),
            "note".into(),
            "open".into(),
            "priority".into(),
        ],
        required: vec!["name".into()],
        links: vec![SchemaLink {
            name: "affects".into(),
            far_kind: "component".into(),
            outgoing: true,
        }],
        sums: Vec::new(),
        types: vec![
            ("open".into(), PropertyType::Boolean),
            ("priority".into(), PropertyType::Integer),
        ],
    }
    .to_record()
    .unwrap()
}

#[test]
fn process_composed_predicates_select_and_fail_closed() {
    let tmp = TempLog::new("predicates");
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": [
            query_component_schema(),
            query_incident_schema(),
            rec("component", "svc-api", false, &[("name", "billing-api"), ("tier", "prod")]),
            rec("component", "svc-web", false, &[("name", "web"), ("tier", "prod")]),
            rec("component", "svc-batch", false, &[("name", "batch"), ("tier", "staging")]),
            rec("incident", "inc-1", false, &[
                ("name", "elevated latency"),
                ("affects", "svc-api"),
                ("open", "true"),
                ("priority", "2"),
            ]),
            rec("incident", "inc-2", false, &[
                ("name", "disk full"),
                ("affects", "svc-api"),
                ("open", "false"),
                ("priority", "3"),
                ("note", "pager"),
            ]),
            rec("incident", "inc-3", false, &[
                ("name", "job delay"),
                ("affects", "svc-batch"),
                ("open", "true"),
                ("priority", "1"),
            ]),
        ]
    }));
    assert!(ingest.ok, "{ingest:?}");

    let mut keys = evaluate_members(
        &host,
        &serde_json::json!({
            "op": "evaluate",
            "request": {
                "root_kind": "incident",
                "hops": [],
                "sum_kind": "incident",
                "sum_property": "priority",
                "predicate": {"op": "eq", "property": "open", "value": "true"},
                "object_bound": 8
            }
        }),
    );
    keys.sort();
    assert_eq!(keys, ["inc-1", "inc-3"]);

    let filter_then_hop = host.rpc(&serde_json::json!({
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
            "predicate": {"op": "eq", "property": "open", "value": "true"},
            "object_bound": 8
        }
    }));
    assert!(filter_then_hop.ok, "{filter_then_hop:?}");
    let mut keys = evaluate_members(
        &host,
        &serde_json::json!({
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
                "predicate": {"op": "eq", "property": "open", "value": "true"},
                "object_bound": 8
            }
        }),
    );
    keys.sort();
    assert_eq!(keys, ["svc-api", "svc-batch"]);

    let hop_then_filter = host.rpc(&serde_json::json!({
        "op": "evaluate",
        "request": {
            "root_kind": "incident",
            "hops": [{
                "far_kind": "component",
                "join_property": "affects",
                "incoming": true,
                "predicate": {"op": "eq", "property": "tier", "value": "prod"}
            }],
            "sum_kind": "component",
            "sum_property": "tier",
            "object_bound": 8
        }
    }));
    assert!(hop_then_filter.ok, "{hop_then_filter:?}");
    let hopped = hop_then_filter.evaluate.expect("evaluate payload");
    assert_eq!(hopped.two_hop_count, 2);
    assert_eq!(hopped.objects.len(), 1);
    assert_eq!(hopped.objects[0].key, "svc-api");

    let range = host.rpc(&serde_json::json!({
        "op": "evaluate",
        "request": {
            "root_kind": "incident",
            "hops": [],
            "sum_kind": "incident",
            "sum_property": "priority",
            "predicate": {"op": "range", "property": "priority", "min": "2", "max": "3"},
            "object_bound": 8
        }
    }));
    assert!(range.ok, "{range:?}");
    let mut keys: Vec<String> = range
        .evaluate
        .unwrap()
        .objects
        .into_iter()
        .map(|row| row.key)
        .collect();
    keys.sort();
    assert_eq!(keys, ["inc-1", "inc-2"]);

    let missing = host.rpc(&serde_json::json!({
        "op": "evaluate",
        "request": {
            "root_kind": "incident",
            "hops": [],
            "sum_kind": "incident",
            "sum_property": "priority",
            "predicate": {"op": "missing", "property": "note"},
            "object_bound": 8
        }
    }));
    assert!(missing.ok, "{missing:?}");
    let mut keys: Vec<String> = missing
        .evaluate
        .unwrap()
        .objects
        .into_iter()
        .map(|row| row.key)
        .collect();
    keys.sort();
    assert_eq!(keys, ["inc-1", "inc-3"]);

    let unsupported = host.rpc(&serde_json::json!({
        "op": "evaluate",
        "request": {
            "root_kind": "incident",
            "hops": [],
            "sum_kind": "incident",
            "sum_property": "priority",
            "predicate": {"op": "union", "args": []},
            "object_bound": 8
        }
    }));
    assert!(!unsupported.ok, "{unsupported:?}");

    let denied = host.rpc(&serde_json::json!({
        "op": "evaluate",
        "request": {
            "root_kind": "incident",
            "hops": [],
            "sum_kind": "incident",
            "sum_property": "priority",
            "deny": [{"kind": "incident", "property": "open"}],
            "predicate": {"op": "eq", "property": "open", "value": "true"},
            "object_bound": 8
        }
    }));
    assert!(!denied.ok, "{denied:?}");

    let hide = host.rpc(&serde_json::json!({
        "op": "hide",
        "kind": "incident",
        "key": "inc-3"
    }));
    assert!(hide.ok, "{hide:?}");
    let mut keys: Vec<String> = host
        .rpc(&serde_json::json!({
            "op": "evaluate",
            "request": {
                "root_kind": "incident",
                "hops": [],
                "sum_kind": "incident",
                "sum_property": "priority",
                "predicate": {"op": "eq", "property": "open", "value": "true"},
                "object_bound": 8
            }
        }))
        .evaluate
        .unwrap()
        .objects
        .into_iter()
        .map(|row| row.key)
        .collect();
    keys.sort();
    assert_eq!(keys, ["inc-1"]);
    drop(host);

    let reopened = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let mut keys: Vec<String> = reopened
        .rpc(&serde_json::json!({
            "op": "evaluate",
            "request": {
                "root_kind": "incident",
                "hops": [],
                "sum_kind": "incident",
                "sum_property": "priority",
                "predicate": {"op": "eq", "property": "open", "value": "true"},
                "object_bound": 8
            }
        }))
        .evaluate
        .unwrap()
        .objects
        .into_iter()
        .map(|row| row.key)
        .collect();
    keys.sort();
    assert_eq!(keys, ["inc-1"]);
}

#[test]
fn process_ordered_pages_bind_snapshot() {
    let tmp = TempLog::new("pages");
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": [
            query_component_schema(),
            query_incident_schema(),
            rec("component", "svc-api", false, &[("name", "billing-api"), ("tier", "prod")]),
            rec("component", "svc-web", false, &[("name", "web"), ("tier", "prod")]),
            rec("component", "svc-batch", false, &[("name", "batch"), ("tier", "staging")]),
            rec("incident", "inc-1", false, &[
                ("name", "elevated latency"),
                ("affects", "svc-api"),
                ("open", "true"),
                ("priority", "2"),
            ]),
            rec("incident", "inc-2", false, &[
                ("name", "disk full"),
                ("affects", "svc-api"),
                ("open", "false"),
                ("priority", "3"),
                ("note", "pager"),
            ]),
            rec("incident", "inc-3", false, &[
                ("name", "job delay"),
                ("affects", "svc-batch"),
                ("open", "true"),
                ("priority", "1"),
            ]),
        ]
    }));
    assert!(ingest.ok, "{ingest:?}");

    let page1 = host.rpc(&serde_json::json!({
        "op": "evaluate",
        "request": {
            "root_kind": "incident",
            "hops": [],
            "sum_kind": "incident",
            "sum_property": "priority",
            "sort": {"property": "priority"},
            "page_size": 1,
            "object_bound": 8
        }
    }));
    assert!(page1.ok, "{page1:?}");
    let first = page1.evaluate.expect("evaluate payload");
    assert_eq!(first.objects.len(), 1);
    assert_eq!(first.objects[0].key, "inc-3");
    assert_eq!(first.two_hop_count, 3);
    let cursor = first.cursor.clone().expect("continuation");

    let page2 = host.rpc(&serde_json::json!({
        "op": "evaluate",
        "request": {
            "root_kind": "incident",
            "hops": [],
            "sum_kind": "incident",
            "sum_property": "priority",
            "sort": {"property": "priority"},
            "page_size": 1,
            "cursor": cursor,
            "object_bound": 8
        }
    }));
    assert!(page2.ok, "{page2:?}");
    let second = page2.evaluate.expect("evaluate payload");
    assert_eq!(second.objects[0].key, "inc-1");

    let ties = evaluate_members(
        &host,
        &serde_json::json!({
            "op": "evaluate",
            "request": {
                "root_kind": "incident",
                "hops": [],
                "sum_kind": "incident",
                "sum_property": "priority",
                "sort": {"property": "open"},
                "object_bound": 8
            }
        }),
    );
    assert_eq!(ties, ["inc-2", "inc-1", "inc-3"]);

    let denied = host.rpc(&serde_json::json!({
        "op": "evaluate",
        "request": {
            "root_kind": "incident",
            "hops": [],
            "sum_kind": "incident",
            "sum_property": "priority",
            "deny": [{"kind": "incident", "property": "priority"}],
            "sort": {"property": "priority"},
            "page_size": 1,
            "object_bound": 8
        }
    }));
    assert!(!denied.ok, "{denied:?}");

    let hide = host.rpc(&serde_json::json!({
        "op": "hide",
        "kind": "incident",
        "key": "inc-2"
    }));
    assert!(hide.ok, "{hide:?}");
    let stale = host.rpc(&serde_json::json!({
        "op": "evaluate",
        "request": {
            "root_kind": "incident",
            "hops": [],
            "sum_kind": "incident",
            "sum_property": "priority",
            "sort": {"property": "priority"},
            "page_size": 1,
            "cursor": first.cursor,
            "object_bound": 8
        }
    }));
    assert!(!stale.ok, "{stale:?}");
}

fn association_records() -> Vec<ObjectRecord> {
    let mut records = product_loop_schema_records();
    records.push(
        SchemaDescriptor {
            kind: "label".into(),
            properties: vec!["name".into()],
            required: vec!["name".into()],
            links: Vec::new(),
            sums: Vec::new(),
            types: Vec::new(),
        }
        .to_record()
        .unwrap(),
    );
    records.push(
        SchemaDescriptor {
            kind: "incident_label".into(),
            properties: vec!["incident".into(), "label".into()],
            required: vec!["incident".into(), "label".into()],
            links: vec![
                SchemaLink {
                    name: "incident".into(),
                    far_kind: "incident".into(),
                    outgoing: true,
                },
                SchemaLink {
                    name: "label".into(),
                    far_kind: "label".into(),
                    outgoing: true,
                },
            ],
            sums: Vec::new(),
            types: Vec::new(),
        }
        .to_record()
        .unwrap(),
    );
    records.extend(product_loop_source());
    records.push(rec("label", "sev-high", false, &[("name", "high")]));
    records.push(rec("label", "region-eu", false, &[("name", "eu")]));
    records.push(rec(
        "incident_label",
        "il-inc-1-sev-high",
        false,
        &[("incident", "inc-1"), ("label", "sev-high")],
    ));
    records.push(rec(
        "incident_label",
        "il-inc-1-region-eu",
        false,
        &[("incident", "inc-1"), ("label", "region-eu")],
    ));
    records
}

fn evaluate_incident_labels() -> serde_json::Value {
    serde_json::json!({
        "op": "evaluate",
        "request": {
            "root_kind": "incident",
            "hops": [
                {"far_kind": "incident_label", "join_property": "incident"},
                {"far_kind": "label", "join_property": "label", "incoming": true}
            ],
            "sum_kind": "label",
            "sum_property": "name",
            "object_bound": 8
        }
    })
}

#[test]
fn process_association_objects_traverse_and_hide() {
    let tmp = TempLog::new("associates");
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": association_records(),
    }));
    assert!(ingest.ok, "{ingest:?}");

    let mut keys: Vec<String> = host
        .rpc(&evaluate_incident_labels())
        .evaluate
        .unwrap()
        .objects
        .into_iter()
        .map(|row| row.key)
        .collect();
    keys.sort();
    assert_eq!(keys, ["region-eu", "sev-high"]);

    let affects = host.rpc(&serde_json::json!({
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
    assert!(affects.ok, "{affects:?}");
    assert_eq!(affects.evaluate.unwrap().objects[0].key, "svc-api");

    let hidden = host.rpc(&serde_json::json!({
        "op": "hide",
        "kind": "incident_label",
        "key": "il-inc-1-region-eu"
    }));
    assert!(hidden.ok, "{hidden:?}");
    let after_hide = host.rpc(&evaluate_incident_labels());
    assert_eq!(after_hide.evaluate.unwrap().objects[0].key, "sev-high");

    let denied = host.rpc(&serde_json::json!({
        "op": "evaluate",
        "request": {
            "root_kind": "incident",
            "hops": [
                {"far_kind": "incident_label", "join_property": "incident"},
                {"far_kind": "label", "join_property": "label", "incoming": true}
            ],
            "sum_kind": "label",
            "sum_property": "name",
            "object_bound": 8,
            "deny": [{"kind": "incident_label", "property": "label"}]
        }
    }));
    assert!(!denied.ok, "{denied:?}");
    assert!(
        denied.error.as_deref().unwrap_or("").contains("Denied"),
        "{denied:?}"
    );
    drop(host);

    let reopened = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    assert_eq!(
        reopened
            .rpc(&evaluate_incident_labels())
            .evaluate
            .unwrap()
            .objects[0]
            .key,
        "sev-high"
    );
    drop(reopened);

    let sidecar = Store::join_map_path(tmp.path());
    if sidecar.is_file() {
        std::fs::remove_file(&sidecar).expect("delete sidecar");
    }
    let delta = Store::join_delta_path(tmp.path());
    if delta.is_file() {
        std::fs::remove_file(&delta).expect("delete join delta");
    }
    let rebuilt = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    assert_eq!(
        rebuilt
            .rpc(&evaluate_incident_labels())
            .evaluate
            .unwrap()
            .objects[0]
            .key,
        "sev-high"
    );
}

#[test]
fn process_restriction_hides_identity_and_fails_closed_on_unknown_keys() {
    let tmp = TempLog::new("restriction");
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": [
            {
                "gen": 1,
                "kind": "component",
                "key": "svc-api",
                "hidden": false,
                "props": {"name": "billing-api", "tier": "prod"}
            },
            {
                "gen": 1,
                "kind": "incident",
                "key": "inc-1",
                "hidden": false,
                "props": {"name": "elevated latency", "affects": "svc-api"}
            }
        ]
    }));
    assert!(ingest.ok, "{ingest:?}");
    let restriction = serde_json::json!({
        "hide_identities": [{"kind": "incident", "key": "inc-1"}]
    });
    let missing = host.rpc(&serde_json::json!({
        "op": "load",
        "kind": "incident",
        "key": "inc-1",
        "restriction": restriction
    }));
    assert!(!missing.ok, "{missing:?}");
    assert!(
        missing
            .error
            .as_deref()
            .unwrap_or("")
            .contains("unknown identity"),
        "{missing:?}"
    );
    let open = host.rpc(&serde_json::json!({
        "op": "load",
        "kind": "incident",
        "key": "inc-1"
    }));
    assert!(open.ok, "{open:?}");
    let hopped = host.rpc(&serde_json::json!({
        "op": "evaluate",
        "request": {
            "root_kind": "incident",
            "hops": [{"far_kind": "component", "join_property": "affects", "incoming": true}],
            "sum_kind": "component",
            "sum_property": "tier",
            "object_bound": 8,
            "restriction": restriction
        }
    }));
    assert!(hopped.ok, "{hopped:?}");
    assert_eq!(hopped.evaluate.unwrap().two_hop_count, 0);
    let overlay = host.rpc(&serde_json::json!({
        "op": "apply_overlay",
        "id": "act-hidden",
        "kind": "incident",
        "key": "inc-1",
        "props": {"note": "x"},
        "restriction": restriction
    }));
    assert!(!overlay.ok, "{overlay:?}");
    assert!(
        overlay
            .error
            .as_deref()
            .unwrap_or("")
            .contains("not in this view"),
        "{overlay:?}"
    );
    let unknown = host.rpc(&serde_json::json!({
        "op": "load",
        "kind": "incident",
        "key": "inc-1",
        "restriction": {"nope": true}
    }));
    assert!(!unknown.ok, "{unknown:?}");
    drop(host);
}

#[test]
fn process_overlay_replays_matching_id() {
    let tmp = TempLog::new("overlay-retry");
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": [{
            "gen": 1,
            "kind": "incident",
            "key": "inc-1",
            "hidden": false,
            "props": {"name": "elevated latency", "affects": "svc-api"}
        }]
    }));
    assert!(ingest.ok, "{ingest:?}");
    let overlay = serde_json::json!({
        "op": "apply_overlay",
        "id": "act-inc-1-note",
        "kind": "incident",
        "key": "inc-1",
        "props": {"note": "acked"}
    });
    let first = host.rpc(&overlay);
    assert!(first.ok, "{first:?}");
    let loaded = host.rpc(&serde_json::json!({
        "op": "load",
        "kind": "incident",
        "key": "inc-1"
    }));
    let gen = loaded.load.as_ref().unwrap().gen;
    let replay = host.rpc(&overlay);
    assert!(replay.ok, "{replay:?}");
    let again = host.rpc(&serde_json::json!({
        "op": "load",
        "kind": "incident",
        "key": "inc-1"
    }));
    assert_eq!(again.load.as_ref().unwrap().gen, gen);
    let conflict = host.rpc(&serde_json::json!({
        "op": "apply_overlay",
        "id": "act-inc-1-note",
        "kind": "incident",
        "key": "inc-1",
        "props": {"note": "other"}
    }));
    assert!(!conflict.ok, "{conflict:?}");
    assert!(
        conflict
            .error
            .as_deref()
            .unwrap_or("")
            .contains("body conflict"),
        "{conflict:?}"
    );
    drop(host);
}

#[test]
fn process_source_resume_replays_without_an_offset_file() {
    let tmp = TempLog::new("source-resume");
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
        "props": {"note": "acked"}
    }));
    assert!(overlay.ok, "{overlay:?}");
    let push = host.rpc(&serde_json::json!({
        "op": "ingest_stream_push",
        "record": {
            "gen": 1,
            "kind": "incident",
            "key": "inc-2",
            "hidden": false,
            "props": {"name": "tail", "affects": "svc-api"}
        }
    }));
    assert!(push.ok, "{push:?}");
    drop(host);
    let reopened = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let missing = reopened.rpc(&serde_json::json!({
        "op": "load",
        "kind": "incident",
        "key": "inc-2"
    }));
    assert!(!missing.ok, "{missing:?}");
    let replay = reopened.rpc(&serde_json::json!({
        "op": "apply_overlay",
        "id": "act-inc-1-note",
        "kind": "incident",
        "key": "inc-1",
        "props": {"note": "acked"}
    }));
    assert!(replay.ok, "{replay:?}");
    let note = load_object(&reopened, "incident", "inc-1");
    assert_eq!(note.props.get("note").map(String::as_str), Some("acked"));
    drop(reopened);
    let parent = tmp.path().parent().unwrap();
    for entry in std::fs::read_dir(parent).unwrap() {
        let name = entry.unwrap().file_name();
        let name = name.to_string_lossy();
        assert!(
            !name.contains("offset") && !name.contains("waiter"),
            "unexpected progress file {name}"
        );
    }
}

#[test]
fn process_health_reports_ready_after_ingest() {
    let tmp = TempLog::new("health");
    let host = HostProcess::spawn(tmp.path(), "127.0.0.1:0", 8);
    let first = host.rpc(&serde_json::json!({"op": "health"}));
    assert!(first.ok, "{first:?}");
    let health = first.health.expect("health payload");
    assert!(health.ready);
    let ingest = host.rpc(&serde_json::json!({
        "op": "ingest_batch",
        "records": product_loop_source(),
    }));
    assert!(ingest.ok, "{ingest:?}");
    let bad = host.rpc(&serde_json::json!({
        "op": "load",
        "kind": "incident",
        "key": "missing"
    }));
    assert!(!bad.ok, "{bad:?}");
    let after = host.rpc(&serde_json::json!({"op": "health"}));
    let health = after.health.expect("health payload");
    assert!(health.ready);
    assert!(health.committed_pages >= 1);
    assert!(health.accepted >= 1);
    assert!(health.rejected >= 1);
    drop(host);
}

#[test]
fn process_serial_second_rpc_waits_for_first_line() {
    let tmp = TempLog::new("serial-rpc");
    let host = HostProcess::spawn_args(
        tmp.path(),
        "127.0.0.1:0",
        8,
        None,
        &["--request-timeout-ms", "800"],
    );
    let mut first = host.connect();
    first.write_all(br#"{"op":"health""#).unwrap();
    first.flush().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let mut second = host.connect();
    second
        .set_read_timeout(Some(Duration::from_millis(120)))
        .unwrap();
    second.write_all(br#"{"op":"health"}"#).unwrap();
    second.write_all(b"\n").unwrap();
    let mut buf = [0u8; 8];
    let early = second.read(&mut buf);
    assert!(
        matches!(
            early,
            Err(ref err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut
        ) || matches!(early, Ok(0)),
        "second RPC must not complete while the first line is open: {early:?}"
    );
    drop(first);
    second
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut body = String::new();
    second.read_to_string(&mut body).unwrap();
    let response: HostResponse = serde_json::from_str(body.trim()).expect("host JSON response");
    assert!(response.ok, "{response:?}");
    assert!(response.health.expect("health").ready);
    drop(host);
}
