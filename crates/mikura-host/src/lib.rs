//! Single-process host over [`mikura::Store`].
//!
//! Wire `op` names are snake_case: `ingest_batch`, `ingest_stream_push`,
//! `ingest_stream_flush`, `apply_action`, `apply_overlay`, `hide`,
//! `evaluate`, `load`. JSON lines are envelope `{ v, token?, op, … }`
//! (`v` omitted or `1`). Evaluate `request.predicate` is the ADR 0015
//! tree; unknown `op` tags fail closed. `request.sort` plus `page_size`
//! returns snapshot pages; `cursor` binds restriction, query, and live writer stamp.
//! Host `restriction` is the ADR 0014 document (`deny_properties`,
//! `hide_kinds`, `hide_identities`); `deny` remains the property-only
//! shorthand. Unknown restriction keys fail closed.
//! Loopback bind is unauthenticated until
//! `require_bearer` is called. Non-loopback bind requires a clerk-owned
//! bearer (ADR 0007). The CLI applies `--bearer` on any bind, including
//! loopback.
//! This crate does not know tenants, policy, receipts, or principals.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use mikura::{
    Action, Aggregate, EvaluateRequest, EvaluateResponse, ExactMatch, Hop, LocalCompute,
    ObjectRecord, ObjectSet, OverlayPatch, Predicate, PropertyAcl, Sort, Store,
};
use mikura_ingest::{BatchIngest, StreamIngest};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireHop {
    pub far_kind: String,
    pub join_property: String,
    #[serde(default)]
    pub incoming: bool,
    #[serde(default)]
    pub predicate: Option<WirePredicate>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct WireDeny {
    pub kind: String,
    pub property: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireIdentity {
    pub kind: String,
    pub key: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireRestriction {
    #[serde(default)]
    pub deny_properties: Vec<WireDeny>,
    #[serde(default)]
    pub hide_kinds: Vec<String>,
    #[serde(default)]
    pub hide_identities: Vec<WireIdentity>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireFilter {
    pub property: String,
    pub value: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum WirePredicate {
    Eq {
        property: String,
        value: String,
    },
    Neq {
        property: String,
        value: String,
    },
    Range {
        property: String,
        #[serde(default)]
        min: Option<String>,
        #[serde(default)]
        max: Option<String>,
    },
    Missing {
        property: String,
    },
    And {
        args: Vec<WirePredicate>,
    },
    Or {
        args: Vec<WirePredicate>,
    },
    Not {
        arg: Box<WirePredicate>,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireEvaluate {
    pub root_kind: String,
    pub hops: Vec<WireHop>,
    pub sum_kind: String,
    pub sum_property: String,
    #[serde(default)]
    pub deny: Vec<WireDeny>,
    #[serde(default)]
    pub restriction: Option<WireRestriction>,
    #[serde(default)]
    pub filter: Option<WireFilter>,
    #[serde(default)]
    pub predicate: Option<WirePredicate>,
    /// Distinct result objects to return. Omit or `0` keeps count/sum only.
    #[serde(default)]
    pub object_bound: usize,
    #[serde(default)]
    pub sort: Option<WireSort>,
    #[serde(default)]
    pub page_size: usize,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireSort {
    pub property: String,
    #[serde(default)]
    pub descending: bool,
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
    ApplyAction {
        id: String,
        kind: String,
        key: String,
        #[serde(default)]
        props: HashMap<String, String>,
        #[serde(default)]
        expected_gen: Option<u64>,
        #[serde(default)]
        deny: Vec<WireDeny>,
        #[serde(default)]
        restriction: Option<WireRestriction>,
    },
    ApplyOverlay {
        id: String,
        kind: String,
        key: String,
        #[serde(default)]
        props: HashMap<String, String>,
        #[serde(default)]
        cleared: Vec<String>,
        #[serde(default)]
        expected_gen: Option<u64>,
        #[serde(default)]
        deny: Vec<WireDeny>,
        #[serde(default)]
        restriction: Option<WireRestriction>,
    },
    Hide {
        kind: String,
        key: String,
        #[serde(default)]
        deny: Vec<WireDeny>,
        #[serde(default)]
        restriction: Option<WireRestriction>,
    },
    Evaluate {
        request: WireEvaluate,
    },
    Load {
        kind: String,
        key: String,
        #[serde(default)]
        deny: Vec<WireDeny>,
        #[serde(default)]
        restriction: Option<WireRestriction>,
    },
}

pub const WIRE_V: u32 = 1;
/// Default max JSON-line bytes for one RPC. Oversized lines fail closed.
pub const DEFAULT_REQUEST_BOUND: usize = 1 << 20;
/// Default request-line wall-clock for assembling one JSON line.
pub const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 5_000;

#[derive(Clone, Debug, Deserialize)]
struct HostEnvelope {
    #[serde(default)]
    token: Option<String>,
    #[serde(flatten)]
    request: HostRequest,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostResponse {
    pub v: u32,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<ObjectRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

pub struct Host {
    store: Store,
    stream: StreamIngest,
    bearer: Option<String>,
    request_bound: usize,
    request_timeout: Duration,
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
            request_bound: DEFAULT_REQUEST_BOUND,
            request_timeout: Duration::from_millis(DEFAULT_REQUEST_TIMEOUT_MS),
        })
    }

    /// Fail closed when a JSON line exceeds `bound` bytes or assembling
    /// that line exceeds `timeout`. Bound must be greater than zero.
    /// After a complete line is accepted, evaluate and ingest run to
    /// completion.
    pub fn set_request_limits(&mut self, bound: usize, timeout: Duration) -> Result<(), String> {
        if bound == 0 {
            return Err("request bound must be greater than zero".into());
        }
        if timeout.is_zero() {
            return Err("request timeout must be greater than zero".into());
        }
        self.request_bound = bound;
        self.request_timeout = timeout;
        Ok(())
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

    /// Bind `addr` and open the store as one host.
    ///
    /// A presented bearer is stored on any bind, including loopback.
    /// Non-loopback still requires a non-empty bearer. `open` plus `handle`
    /// without `require_bearer` remains the in-process clerk path.
    pub fn listen(
        log: &Path,
        stream_bound: usize,
        addr: SocketAddr,
        bearer: Option<&str>,
    ) -> Result<(Self, TcpListener), String> {
        let listener = Self::bind(addr, bearer)?;
        let mut host = Self::open(log, stream_bound)?;
        if let Some(secret) = bearer {
            host.require_bearer(secret)?;
        }
        Ok((host, listener))
    }

    /// Bind a TCP listener. Non-loopback addresses need a non-empty bearer.
    pub fn bind(addr: SocketAddr, bearer: Option<&str>) -> Result<TcpListener, String> {
        require_bearer_if_routable(
            addr,
            bearer.is_some_and(|secret| !secret.is_empty()),
            "non-loopback bind refused without a clerk bearer",
        )?;
        TcpListener::bind(addr).map_err(|err| err.to_string())
    }

    pub fn handle(&mut self, request: HostRequest) -> HostResponse {
        match request {
            HostRequest::IngestBatch { records } => ack(BatchIngest::run(&mut self.store, records)),
            HostRequest::IngestStreamPush { record } => {
                ack(self.stream.push(&mut self.store, record))
            }
            HostRequest::IngestStreamFlush => ack(self.stream.flush(&mut self.store)),
            HostRequest::ApplyAction {
                id,
                kind,
                key,
                props,
                expected_gen,
                deny,
                restriction,
            } => {
                let acl = restriction_from_wire(&deny, restriction.as_ref(), "apply_action");
                ack(acl.and_then(|acl| {
                    self.store.apply_action_in_view(
                        Action {
                            id,
                            kind,
                            key,
                            props,
                        },
                        expected_gen,
                        &acl,
                    )
                }))
            }
            HostRequest::ApplyOverlay {
                id,
                kind,
                key,
                props,
                cleared,
                expected_gen,
                deny,
                restriction,
            } => {
                let acl = restriction_from_wire(&deny, restriction.as_ref(), "apply_overlay");
                ack(acl.and_then(|acl| {
                    self.store.apply_overlay_in_view(
                        OverlayPatch {
                            kind,
                            key,
                            props,
                            cleared,
                            action_id: None,
                        },
                        id,
                        expected_gen,
                        &acl,
                    )
                }))
            }
            HostRequest::Hide {
                kind,
                key,
                deny,
                restriction,
            } => ack(hide_identity(&mut self.store, kind, key, deny, restriction)),
            HostRequest::Evaluate { request } => match evaluate(&self.store, request) {
                Ok(response) => HostResponse {
                    v: WIRE_V,
                    ok: true,
                    error: None,
                    evaluate: Some(EvaluateWire {
                        two_hop_count: response.two_hop_count,
                        sum_amount: response.sum_amount,
                        objects: response.objects,
                        cursor: response.cursor,
                    }),
                    load: None,
                },
                Err(error) => fail(error),
            },
            HostRequest::Load {
                kind,
                key,
                deny,
                restriction,
            } => match load_record(&self.store, kind, key, deny, restriction) {
                Ok(record) => HostResponse {
                    v: WIRE_V,
                    ok: true,
                    error: None,
                    evaluate: None,
                    load: Some(record),
                },
                Err(error) => fail(error),
            },
        }
    }

    pub fn handle_line(&mut self, line: &str) -> HostResponse {
        if line.len() > self.request_bound {
            return request_bound_fail(self.request_bound, line.len());
        }
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(error) => return fail(error.to_string()),
        };
        if let Err(error) = wire_version(&value) {
            return fail(error);
        }
        let envelope: HostEnvelope = match serde_json::from_value(value) {
            Ok(envelope) => envelope,
            Err(error) => return fail(error.to_string()),
        };
        if let Err(error) = self.check_token(envelope.token.as_deref()) {
            return fail(error);
        }
        self.handle(envelope.request)
    }

    pub fn serve_one(&mut self, mut stream: TcpStream) -> Result<(), String> {
        self.refuse_unauthenticated_routable(stream.local_addr().map_err(|err| err.to_string())?)?;
        stream
            .set_write_timeout(Some(self.request_timeout))
            .map_err(|err| err.to_string())?;
        let deadline = Instant::now() + self.request_timeout;
        let line = match read_request_line(&mut stream, self.request_bound, deadline) {
            Ok(line) => line,
            Err(ServeRead::Bound { bound, bytes }) => {
                return write_fail_closed(&mut stream, request_bound_fail(bound, bytes), deadline);
            }
            Err(ServeRead::Timeout) => {
                return write_fail_closed(&mut stream, fail("RequestTimeout"), deadline);
            }
            Err(ServeRead::Disconnect) => return Err("client disconnect".into()),
            Err(ServeRead::Io(error)) => return Err(error),
        };
        write_response(&mut stream, self.handle_line(line.trim()))
    }

    /// Accept connections until the listener fails.
    ///
    /// A single client disconnect, timeout, or bound rejection does not stop
    /// the host. Listener accept errors still fail closed. The process binary
    /// stops by closing stdin; that path uses [`Self::serve_while`] and does
    /// not flush the stream buffer.
    pub fn serve(&mut self, listener: TcpListener) -> Result<(), String> {
        self.serve_while(listener, || true)
    }

    /// Accept connections while `keep_going` is true.
    ///
    /// Returning because `keep_going` is false does not flush uncommitted
    /// stream pushes. A blocked accept still needs one wakeup connect after
    /// the flag flips.
    pub fn serve_while<F>(&mut self, listener: TcpListener, keep_going: F) -> Result<(), String>
    where
        F: Fn() -> bool,
    {
        self.refuse_unauthenticated_routable(
            listener.local_addr().map_err(|err| err.to_string())?,
        )?;
        while keep_going() {
            let stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(err) => return Err(err.to_string()),
            };
            if !keep_going() {
                return Ok(());
            }
            let _ = self.serve_one(stream);
        }
        Ok(())
    }

    fn refuse_unauthenticated_routable(&self, addr: SocketAddr) -> Result<(), String> {
        require_bearer_if_routable(
            addr,
            self.bearer.is_some(),
            "non-loopback serve refused without a clerk bearer",
        )
    }
}

fn require_bearer_if_routable(
    addr: SocketAddr,
    present: bool,
    message: &str,
) -> Result<(), String> {
    if !addr.ip().is_loopback() && !present {
        return Err(message.into());
    }
    Ok(())
}

fn ack(result: Result<(), String>) -> HostResponse {
    match result {
        Ok(()) => ok(),
        Err(error) => fail(error),
    }
}

fn request_bound_fail(bound: usize, bytes: usize) -> HostResponse {
    fail(format!("RequestBound {{ bound: {bound}, bytes: {bytes} }}"))
}

fn restriction_from_wire(
    deny: &[WireDeny],
    restriction: Option<&WireRestriction>,
    op: &str,
) -> Result<PropertyAcl, String> {
    match restriction {
        None => acl_from_denies(deny, op),
        Some(restriction) => {
            if !deny.is_empty() {
                return Err(format!("host {op} accepts deny or restriction, not both"));
            }
            let mut acl = PropertyAcl::allow_all();
            for pair in &restriction.deny_properties {
                acl.insert_deny(&pair.kind, &pair.property)
                    .map_err(|err| err.to_string())?;
            }
            for kind in &restriction.hide_kinds {
                acl.insert_hide_kind(kind).map_err(|err| err.to_string())?;
            }
            for id in &restriction.hide_identities {
                acl.insert_hide_identity(&id.kind, &id.key)
                    .map_err(|err| err.to_string())?;
            }
            Ok(acl)
        }
    }
}

fn acl_from_denies(deny: &[WireDeny], op: &str) -> Result<PropertyAcl, String> {
    let mut acl = PropertyAcl::allow_all();
    for pair in deny {
        acl.insert_deny(&pair.kind, &pair.property)
            .map_err(|err| format!("host {op}: {err}"))?;
    }
    Ok(acl)
}

fn ok() -> HostResponse {
    HostResponse {
        v: WIRE_V,
        ok: true,
        error: None,
        evaluate: None,
        load: None,
    }
}

fn fail(error: impl ToString) -> HostResponse {
    HostResponse {
        v: WIRE_V,
        ok: false,
        error: Some(error.to_string()),
        evaluate: None,
        load: None,
    }
}

enum ServeRead {
    Bound { bound: usize, bytes: usize },
    Timeout,
    Disconnect,
    Io(String),
}

fn remaining_until(deadline: Instant) -> Result<Duration, ServeRead> {
    let now = Instant::now();
    if now >= deadline {
        return Err(ServeRead::Timeout);
    }
    Ok(deadline.saturating_duration_since(now))
}

fn read_request_line(
    stream: &mut TcpStream,
    bound: usize,
    deadline: Instant,
) -> Result<String, ServeRead> {
    if bound == 0 {
        return Err(ServeRead::Bound { bound: 0, bytes: 1 });
    }
    let mut buf = Vec::with_capacity(bound);
    let mut slab = [0u8; 8192];
    loop {
        let remaining = remaining_until(deadline)?;
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|err| ServeRead::Io(err.to_string()))?;
        match stream.read(&mut slab) {
            Ok(0) => return Err(ServeRead::Disconnect),
            Ok(n) => {
                for &b in &slab[..n] {
                    if b == b'\n' {
                        return String::from_utf8(buf)
                            .map_err(|err| ServeRead::Io(err.to_string()));
                    }
                    if buf.len() >= bound {
                        return Err(ServeRead::Bound {
                            bound,
                            bytes: bound + 1,
                        });
                    }
                    buf.push(b);
                }
            }
            Err(err)
                if err.kind() == std::io::ErrorKind::TimedOut
                    || err.kind() == std::io::ErrorKind::WouldBlock =>
            {
                return Err(ServeRead::Timeout);
            }
            Err(err) => return Err(ServeRead::Io(err.to_string())),
        }
    }
}

fn write_response(stream: &mut TcpStream, response: HostResponse) -> Result<(), String> {
    let body = serde_json::to_string(&response).map_err(|err| err.to_string())?;
    stream
        .write_all(body.as_bytes())
        .map_err(|err| err.to_string())?;
    stream.write_all(b"\n").map_err(|err| err.to_string())
}

/// Reply, then FIN the write side and discard leftover input so a client that
/// already sent past the bound can still read the typed error instead of RST.
/// Discard is the remaining request wall-clock budget, not a fresh idle
/// timeout on every chunk.
fn write_fail_closed(
    stream: &mut TcpStream,
    response: HostResponse,
    deadline: Instant,
) -> Result<(), String> {
    write_response(stream, response)?;
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let mut discard = [0u8; 256];
    loop {
        let Ok(remaining) = remaining_until(deadline) else {
            return Ok(());
        };
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|err| err.to_string())?;
        match stream.read(&mut discard) {
            Ok(0) => return Ok(()),
            Ok(_) => continue,
            Err(err)
                if err.kind() == std::io::ErrorKind::TimedOut
                    || err.kind() == std::io::ErrorKind::WouldBlock =>
            {
                return Ok(());
            }
            Err(_) => return Ok(()),
        }
    }
}

fn wire_version(value: &serde_json::Value) -> Result<u32, String> {
    match value.get("v") {
        None => Ok(WIRE_V),
        Some(version) => {
            let version = version
                .as_u64()
                .ok_or_else(|| "host wire v must be an integer".to_string())?;
            if version == u64::from(WIRE_V) {
                Ok(WIRE_V)
            } else {
                Err(format!("unsupported host wire v {version}"))
            }
        }
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

fn predicate_from_wire(pred: WirePredicate) -> Result<Predicate, String> {
    Ok(match pred {
        WirePredicate::Eq { property, value } => Predicate::eq(property, value),
        WirePredicate::Neq { property, value } => Predicate::neq(property, value),
        WirePredicate::Range { property, min, max } => Predicate::Range { property, min, max },
        WirePredicate::Missing { property } => Predicate::missing(property),
        WirePredicate::And { args } => Predicate::and(
            args.into_iter()
                .map(predicate_from_wire)
                .collect::<Result<_, _>>()?,
        ),
        WirePredicate::Or { args } => Predicate::or(
            args.into_iter()
                .map(predicate_from_wire)
                .collect::<Result<_, _>>()?,
        ),
        WirePredicate::Not { arg } => !predicate_from_wire(*arg)?,
    })
}

fn evaluate(store: &Store, request: WireEvaluate) -> Result<EvaluateResponse, String> {
    let acl = restriction_from_wire(&request.deny, request.restriction.as_ref(), "evaluate")?;
    if let Some(filter) = &request.filter {
        if filter.property.is_empty() || filter.value.is_empty() {
            return Err("evaluate filter requires non-empty property and value".into());
        }
    }
    if request.filter.is_some() && request.predicate.is_some() {
        return Err("evaluate accepts filter or predicate, not both".into());
    }
    let hops = request
        .hops
        .into_iter()
        .map(|hop| {
            Ok(Hop {
                far_kind: hop.far_kind,
                join_property: hop.join_property,
                incoming: hop.incoming,
                predicate: hop.predicate.map(predicate_from_wire).transpose()?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let request = EvaluateRequest {
        root_kind: request.root_kind,
        hops,
        sum_kind: request.sum_kind,
        sum_property: request.sum_property,
        aggregate: Aggregate::CountAndSum,
        acl,
        filter: request.filter.map(|filter| ExactMatch {
            property: filter.property,
            value: filter.value,
        }),
        predicate: request.predicate.map(predicate_from_wire).transpose()?,
        object_bound: request.object_bound,
        sort: request.sort.map(|sort| Sort {
            property: sort.property,
            descending: sort.descending,
        }),
        page_size: request.page_size,
        cursor: request.cursor,
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
    restriction: Option<WireRestriction>,
) -> Result<ObjectRecord, String> {
    let acl = restriction_from_wire(&deny, restriction.as_ref(), "load")?;
    store.load(&kind, &key, &acl)
}

fn hide_identity(
    store: &mut Store,
    kind: String,
    key: String,
    deny: Vec<WireDeny>,
    restriction: Option<WireRestriction>,
) -> Result<(), String> {
    let acl = restriction_from_wire(&deny, restriction.as_ref(), "hide")?;
    store.hide(&kind, &key, &acl)
}

#[cfg(test)]
mod tests;
