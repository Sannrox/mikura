//! Thin loopback client for the M0 product-loop contract.
//!
//! Speaks existing host JSON-line ops only. Does not depend on a control
//! plane. Seed ids match the public Sekai product-loop fixture.
//!
//! ```text
//! cargo run -p mikura-host --example product_loop
//! ```

use mikura::{ObjectRecord, PropertyAcl, Store};
use mikura_host::{Host, HostResponse};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::thread;

struct Session {
    host: Host,
    listener: TcpListener,
    addr: SocketAddr,
}

impl Session {
    fn open(log: &Path) -> Result<Self, String> {
        let (host, listener) = Host::listen(log, 8, "127.0.0.1:0".parse().unwrap(), None)?;
        let addr = listener
            .local_addr()
            .map_err(|err| format!("listener address: {err}"))?;
        Ok(Self {
            host,
            listener,
            addr,
        })
    }

    fn rpc(&mut self, body: &serde_json::Value) -> Result<HostResponse, String> {
        thread::scope(|scope| {
            let addr = self.addr;
            let client = scope.spawn(move || rpc_once(addr, body));
            self.host.serve_one(
                self.listener
                    .accept()
                    .map_err(|err| format!("accept: {err}"))?
                    .0,
            )?;
            client.join().unwrap()
        })
    }
}

fn rpc_once(addr: SocketAddr, body: &serde_json::Value) -> Result<HostResponse, String> {
    let mut stream = TcpStream::connect(addr).map_err(|err| format!("connect: {err}"))?;
    let line = serde_json::to_string(body).map_err(|err| err.to_string())?;
    stream
        .write_all(line.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .map_err(|err| format!("write: {err}"))?;
    let mut buf = String::new();
    stream
        .read_to_string(&mut buf)
        .map_err(|err| format!("read: {err}"))?;
    serde_json::from_str(buf.trim()).map_err(|err| format!("decode: {err}"))
}

fn rec(kind: &str, key: &str, props: &[(&str, &str)]) -> ObjectRecord {
    ObjectRecord {
        gen: 1,
        kind: kind.into(),
        key: key.into(),
        hidden: false,
        action_id: None,
        props: props
            .iter()
            .map(|(name, value)| ((*name).into(), (*value).into()))
            .collect(),
    }
}

/// Clerk mapping of the public product-loop seed onto `ObjectRecord`.
fn source_records() -> Vec<ObjectRecord> {
    vec![
        rec(
            "component",
            "svc-api",
            &[("name", "billing-api"), ("tier", "prod")],
        ),
        rec(
            "incident",
            "inc-1",
            &[("name", "elevated latency"), ("affects", "svc-api")],
        ),
    ]
}

fn require_ok(step: &str, response: HostResponse) -> Result<HostResponse, String> {
    if response.ok {
        Ok(response)
    } else {
        Err(format!(
            "{step} failed: {}",
            response.error.unwrap_or_else(|| "unknown".into())
        ))
    }
}

fn main() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("mikura-product-loop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    let log = dir.join("objects.mikura");

    let mut session = Session::open(&log)?;
    require_ok(
        "ingest_batch source",
        session.rpc(&serde_json::json!({
            "op": "ingest_batch",
            "records": source_records(),
        }))?,
    )?;

    let loaded = require_ok(
        "load svc-api",
        session.rpc(&serde_json::json!({
            "op": "load",
            "kind": "component",
            "key": "svc-api"
        }))?,
    )?
    .load
    .expect("load payload");
    assert_eq!(loaded.kind, "component");
    assert_eq!(loaded.key, "svc-api");
    assert_eq!(
        loaded.props.get("name").map(String::as_str),
        Some("billing-api")
    );
    assert_eq!(loaded.props.get("tier").map(String::as_str), Some("prod"));

    let filtered = require_ok(
        "evaluate filter component tier=prod",
        session.rpc(&serde_json::json!({
            "op": "evaluate",
            "request": {
                "root_kind": "component",
                "hops": [],
                "sum_kind": "component",
                "sum_property": "tier",
                "filter": {"property": "tier", "value": "prod"},
                "object_bound": 8
            }
        }))?,
    )?
    .evaluate
    .expect("evaluate payload");
    assert_eq!(filtered.two_hop_count, 1);
    assert_eq!(filtered.sum_amount, 0);
    assert_eq!(filtered.objects.len(), 1);
    assert_eq!(filtered.objects[0].key, "svc-api");

    let hopped = require_ok(
        "evaluate hop incident → component on affects",
        session.rpc(&serde_json::json!({
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
        }))?,
    )?
    .evaluate
    .expect("evaluate payload");
    assert_eq!(hopped.two_hop_count, 1);
    assert_eq!(hopped.sum_amount, 0);
    assert_eq!(hopped.objects.len(), 1);
    assert_eq!(hopped.objects[0].key, "svc-api");

    require_ok(
        "apply_action inc-1",
        session.rpc(&serde_json::json!({
            "op": "apply_action",
            "id": "act-inc-1-note",
            "kind": "incident",
            "key": "inc-1",
            "props": {
                "name": "elevated latency",
                "affects": "svc-api",
                "note": "acked"
            }
        }))?,
    )?;
    let edited = require_ok(
        "load inc-1 after edit",
        session.rpc(&serde_json::json!({
            "op": "load",
            "kind": "incident",
            "key": "inc-1"
        }))?,
    )?
    .load
    .expect("load payload");
    assert_eq!(edited.action_id.as_deref(), Some("act-inc-1-note"));
    assert_eq!(edited.props.get("note").map(String::as_str), Some("acked"));

    require_ok(
        "ingest_batch refresh source",
        session.rpc(&serde_json::json!({
            "op": "ingest_batch",
            "records": source_records(),
        }))?,
    )?;
    let refreshed = require_ok(
        "load inc-1 after refresh",
        session.rpc(&serde_json::json!({
            "op": "load",
            "kind": "incident",
            "key": "inc-1"
        }))?,
    )?
    .load
    .expect("load payload");
    assert_eq!(refreshed.action_id, None);
    assert!(!refreshed.props.contains_key("note"));
    assert_eq!(
        refreshed.props.get("name").map(String::as_str),
        Some("elevated latency")
    );

    drop(session);
    let mut reopened = Session::open(&log)?;
    let after_reopen = require_ok(
        "load svc-api after reopen",
        reopened.rpc(&serde_json::json!({
            "op": "load",
            "kind": "component",
            "key": "svc-api"
        }))?,
    )?
    .load
    .expect("load payload");
    assert_eq!(
        after_reopen.props.get("tier").map(String::as_str),
        Some("prod")
    );
    let store = Store::open(&log)?;
    assert_eq!(
        store
            .load("incident", "inc-1", &PropertyAcl::allow_all())?
            .props
            .get("name")
            .map(String::as_str),
        Some("elevated latency")
    );

    require_ok(
        "hide inc-1",
        reopened.rpc(&serde_json::json!({
            "op": "hide",
            "kind": "incident",
            "key": "inc-1"
        }))?,
    )?;
    let hidden = require_ok(
        "load inc-1 after hide",
        reopened.rpc(&serde_json::json!({
            "op": "load",
            "kind": "incident",
            "key": "inc-1"
        }))?,
    )?
    .load
    .expect("load payload");
    assert!(hidden.hidden);
    let after_hide = require_ok(
        "evaluate hop after hide",
        reopened.rpc(&serde_json::json!({
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
        }))?,
    )?
    .evaluate
    .expect("evaluate payload");
    assert_eq!(after_hide.two_hop_count, 0);
    assert!(after_hide.objects.is_empty());

    println!("step\thost op\tresult");
    println!("ingest seed\tingest_batch\tsupported");
    println!("load svc-api\tload\tsupported (component/svc-api, name=billing-api, tier=prod)");
    println!(
        "filter component tier=prod\tevaluate\tsupported ({} object)",
        filtered.objects[0].key
    );
    println!(
        "hop incident → component on affects\tevaluate\tsupported ({} object)",
        hopped.objects[0].key
    );
    println!("edit inc-1\tapply_action\tsupported (whole-record replace, action_id stored)");
    println!("refresh source\tingest_batch\tsupported as overwrite; edit note discarded");
    println!("reopen log\tload after Host::open\tsupported");
    println!("delete inc-1\thide\tsupported (load defined, evaluate omits)");
    println!("retry / object visibility\t(none)\tunsupported on the host wire");

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
