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

    /// Accept connections until the listener closes or an I/O error occurs.
    pub fn serve(&mut self, listener: TcpListener) -> Result<(), String> {
        for incoming in listener.incoming() {
            self.serve_one(incoming.map_err(|err| err.to_string())?)?;
        }
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
mod tests;
