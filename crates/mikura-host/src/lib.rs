//! Single-process loopback host over [`mikura::Store`].
//!
//! RPCs are local names (`IngestBatch`, `IngestStreamPush`,
//! `IngestStreamFlush`, `Evaluate`). Non-loopback bind is refused until an
//! authentication story exists. This crate does not know tenants, policy,
//! receipts, or principals.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;

use mikura::{
    Aggregate, EvaluateRequest, EvaluateResponse, Hop, LocalCompute, ObjectRecord, ObjectSet,
    PropertyAcl, Store,
};
use mikura_ingest::{BatchIngest, StreamIngest};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize)]
pub struct WireHop {
    pub far_kind: String,
    pub join_property: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct WireDeny {
    pub kind: String,
    pub property: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct WireEvaluate {
    pub root_kind: String,
    pub hops: Vec<WireHop>,
    pub sum_kind: String,
    pub sum_property: String,
    #[serde(default)]
    pub deny: Vec<WireDeny>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum HostRequest {
    IngestBatch { records: Vec<ObjectRecord> },
    IngestStreamPush { record: ObjectRecord },
    IngestStreamFlush,
    Evaluate { request: WireEvaluate },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluate: Option<EvaluateWire>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvaluateWire {
    pub two_hop_count: usize,
    pub sum_amount: i64,
}

pub struct Host {
    store: Store,
    stream: StreamIngest,
}

impl Host {
    pub fn open(log: &Path, stream_bound: usize) -> Result<Self, String> {
        let store = if log.exists() {
            Store::open(log)?
        } else {
            Store::create(log)?
        };
        Ok(Self {
            store,
            stream: StreamIngest::new(stream_bound)?,
        })
    }

    /// Bind a TCP listener. Non-loopback addresses are refused.
    pub fn bind(addr: SocketAddr) -> Result<TcpListener, String> {
        if !addr.ip().is_loopback() {
            return Err("non-loopback bind refused until auth exists".into());
        }
        TcpListener::bind(addr).map_err(|err| err.to_string())
    }

    pub fn handle(&mut self, request: HostRequest) -> HostResponse {
        match request {
            HostRequest::IngestBatch { records } => {
                match BatchIngest::run(&mut self.store, records) {
                    Ok(()) => ok(),
                    Err(error) => fail(error),
                }
            }
            HostRequest::IngestStreamPush { record } => {
                match self.stream.push(&mut self.store, record) {
                    Ok(()) => ok(),
                    Err(error) => fail(error),
                }
            }
            HostRequest::IngestStreamFlush => match self.stream.flush(&mut self.store) {
                Ok(()) => ok(),
                Err(error) => fail(error),
            },
            HostRequest::Evaluate { request } => match evaluate(&self.store, request) {
                Ok(response) => HostResponse {
                    ok: true,
                    error: None,
                    evaluate: Some(EvaluateWire {
                        two_hop_count: response.two_hop_count,
                        sum_amount: response.sum_amount,
                    }),
                },
                Err(error) => fail(error),
            },
        }
    }

    pub fn handle_line(&mut self, line: &str) -> HostResponse {
        match serde_json::from_str::<HostRequest>(line) {
            Ok(request) => self.handle(request),
            Err(error) => fail(error.to_string()),
        }
    }

    pub fn serve_one(&mut self, mut stream: TcpStream) -> Result<(), String> {
        use std::io::{BufRead, BufReader, Write};
        let mut reader = BufReader::new(stream.try_clone().map_err(|err| err.to_string())?);
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|err| err.to_string())?;
        let response = self.handle_line(line.trim());
        let body = serde_json::to_string(&response).map_err(|err| err.to_string())?;
        stream
            .write_all(body.as_bytes())
            .map_err(|err| err.to_string())?;
        stream.write_all(b"\n").map_err(|err| err.to_string())?;
        Ok(())
    }
}

fn ok() -> HostResponse {
    HostResponse {
        ok: true,
        error: None,
        evaluate: None,
    }
}

fn fail(error: impl ToString) -> HostResponse {
    HostResponse {
        ok: false,
        error: Some(error.to_string()),
        evaluate: None,
    }
}

fn evaluate(store: &Store, request: WireEvaluate) -> Result<EvaluateResponse, String> {
    let acl = match request.deny.as_slice() {
        [] => PropertyAcl::allow_all(),
        [deny] => PropertyAcl::deny_property(&deny.kind, &deny.property),
        _ => return Err("host evaluate accepts at most one deny pair in v1".into()),
    };
    let request = EvaluateRequest {
        root_kind: request.root_kind,
        hops: request
            .hops
            .into_iter()
            .map(|hop| Hop {
                far_kind: hop.far_kind,
                join_property: hop.join_property,
            })
            .collect(),
        sum_kind: request.sum_kind,
        sum_property: request.sum_property,
        aggregate: Aggregate::CountAndSum,
        acl,
    };
    ObjectSet::new(LocalCompute)
        .evaluate(store, &request)
        .map_err(|err| format!("{err:?}"))
}

#[cfg(test)]
mod tests {
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
}
