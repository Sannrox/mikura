//! Single-process host over [`mikura::Store`].
//!
//! RPCs are local names (`IngestBatch`, `IngestStreamPush`,
//! `IngestStreamFlush`, `Evaluate`, `Load`). Loopback bind is unauthenticated.
//! Non-loopback bind requires a clerk-owned bearer (ADR 0007).
//! This crate does not know tenants, policy, receipts, or principals.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;

use mikura::{
    Aggregate, EvaluateRequest, EvaluateResponse, ExactMatch, Hop, LocalCompute, ObjectRecord,
    ObjectSet, PropertyAcl, Store,
};
use mikura_ingest::{BatchIngest, StreamIngest};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize)]
pub struct WireHop {
    pub far_kind: String,
    pub join_property: String,
    #[serde(default)]
    pub incoming: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct WireDeny {
    pub kind: String,
    pub property: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct WireFilter {
    pub property: String,
    pub value: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct WireEvaluate {
    pub root_kind: String,
    pub hops: Vec<WireHop>,
    pub sum_kind: String,
    pub sum_property: String,
    #[serde(default)]
    pub deny: Vec<WireDeny>,
    #[serde(default)]
    pub filter: Option<WireFilter>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum HostRequest {
    IngestBatch {
        records: Vec<ObjectRecord>,
    },
    IngestStreamPush {
        record: ObjectRecord,
    },
    IngestStreamFlush,
    Evaluate {
        request: WireEvaluate,
    },
    Load {
        kind: String,
        key: String,
        #[serde(default)]
        deny: Vec<WireDeny>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluate: Option<EvaluateWire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load: Option<ObjectRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvaluateWire {
    pub two_hop_count: usize,
    pub sum_amount: i64,
}

pub struct Host {
    store: Store,
    stream: StreamIngest,
    bearer: Option<String>,
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
            bearer: None,
        })
    }

    /// Require a matching token on every JSON-line RPC.
    pub fn require_bearer(&mut self, bearer: impl Into<String>) -> Result<(), String> {
        let bearer = bearer.into();
        if bearer.is_empty() {
            return Err("clerk bearer must be non-empty".into());
        }
        self.bearer = Some(bearer);
        Ok(())
    }

    /// Bind a TCP listener. Non-loopback addresses need a non-empty bearer.
    pub fn bind(addr: SocketAddr, bearer: Option<&str>) -> Result<TcpListener, String> {
        if !addr.ip().is_loopback() {
            match bearer {
                Some(secret) if !secret.is_empty() => {}
                _ => {
                    return Err("non-loopback bind refused without a clerk bearer".into());
                }
            }
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
                    load: None,
                },
                Err(error) => fail(error),
            },
            HostRequest::Load { kind, key, deny } => {
                match load_record(&self.store, kind, key, deny) {
                    Ok(record) => HostResponse {
                        ok: true,
                        error: None,
                        evaluate: None,
                        load: Some(record),
                    },
                    Err(error) => fail(error),
                }
            }
        }
    }

    pub fn handle_line(&mut self, line: &str) -> HostResponse {
        let mut value: serde_json::Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(error) => return fail(error.to_string()),
        };
        let token = value
            .get("token")
            .and_then(|value| value.as_str())
            .map(str::to_owned);
        if let Some(object) = value.as_object_mut() {
            object.remove("token");
        }
        if let Err(error) = self.check_token(token.as_deref()) {
            return fail(error);
        }
        match serde_json::from_value::<HostRequest>(value) {
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
        load: None,
    }
}

fn fail(error: impl ToString) -> HostResponse {
    HostResponse {
        ok: false,
        error: Some(error.to_string()),
        evaluate: None,
        load: None,
    }
}

fn tokens_equal(expected: &str, presented: &str) -> bool {
    let expected = expected.as_bytes();
    let presented = presented.as_bytes();
    if expected.len() != presented.len() {
        return false;
    }
    let mut diff = 0u8;
    for (left, right) in expected.iter().zip(presented.iter()) {
        diff |= left ^ right;
    }
    diff == 0
}

impl Host {
    fn check_token(&self, presented: Option<&str>) -> Result<(), String> {
        let Some(expected) = self.bearer.as_deref() else {
            return Ok(());
        };
        match presented {
            Some(got) if tokens_equal(expected, got) => Ok(()),
            _ => Err("bearer required".into()),
        }
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
                incoming: hop.incoming,
            })
            .collect(),
        sum_kind: request.sum_kind,
        sum_property: request.sum_property,
        aggregate: Aggregate::CountAndSum,
        acl,
        filter: request.filter.map(|filter| ExactMatch {
            property: filter.property,
            value: filter.value,
        }),
    };
    ObjectSet::new(LocalCompute)
        .evaluate(store, &request)
        .map_err(|err| format!("{err:?}"))
}

fn load_record(
    store: &Store,
    kind: String,
    key: String,
    deny: Vec<WireDeny>,
) -> Result<ObjectRecord, String> {
    let acl = match deny.as_slice() {
        [] => PropertyAcl::allow_all(),
        [deny] => PropertyAcl::deny_property(&deny.kind, &deny.property),
        _ => return Err("host load accepts at most one deny pair in v1".into()),
    };
    store.load(&kind, &key, &acl)
}

#[cfg(test)]
mod tests;
