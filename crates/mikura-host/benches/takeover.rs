//! Takeover and fencing drill under write load (#364). See
//! `docs/benchmarks.md#takeover-drill`.
//!
//! ```text
//! cargo bench -p mikura-host --bench takeover
//! MIKURA_DRILL_ROUNDS=5 ...                    # rounds per scenario (default 3)
//! MIKURA_DRILL_OUT=drill.json ...              # write this run's results
//! MIKURA_DRILL_BASELINE=baseline.json ...      # time-to-accept regression gate
//! ```
//!
//! Each round starts a loopback object namespace and `mikura-host`
//! processes on it: a writer and a read-only standby, each reaching storage
//! through its own fault proxy, the way two machines have their own route.
//! Clients write continuously. One fault is injected, the operator step
//! that follows it runs at once (this repository has no failure detector),
//! and the round records how long until writes are accepted again.
//!
//! Oracles, every round: no acknowledged write is lost; a write that was
//! not acknowledged ends committed or not committed after readback and is
//! retried only when absent (blindly at the same source version under
//! `VersionedKillWriter`), so no key has two generations; a displaced
//! writer that is still running refuses writes. Any failed oracle fails
//! the run. With a baseline, a scenario whose median time-to-accept is
//! more than `REGRESSION_LIMIT` times and `REGRESSION_FLOOR` above the
//! baseline median fails the run.

mod common;

use common::{host_rpc, machine, spawn_host, Machine};
use mikura::{LocalObjectServer, ObjectBackend, PropertyAcl, Store};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitCode};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const NS: &str = "drill";
const KIND: &str = "Item";
const DEFAULT_ROUNDS: usize = 3;
/// Concurrent clients against the one writer.
const CLIENTS: usize = 4;
/// Scenario load, all clients together. The object client opens a
/// connection per request, about ten per one-record write, and each
/// passes the proxy, so unpaced load runs out of ephemeral ports within a
/// few rounds on macOS. This rate keeps a full run inside the port range.
const PACED_WRITES_PER_SEC: u32 = 25;
/// Unpaced load once per run, after the scenarios, to measure
/// single-writer throughput.
const BURST: Duration = Duration::from_secs(2);
/// Load before the fault.
const STEADY: Duration = Duration::from_secs(2);
/// Load after writes are accepted again, before the round stops.
const AFTER: Duration = Duration::from_secs(1);
/// A fault that leaves writes refused this long fails the round.
const ACCEPT_BOUND: Duration = Duration::from_secs(20);
/// Gate: fail when the median exceeds both `baseline * LIMIT` and
/// `baseline + FLOOR`. The floor keeps sub-second jitter from failing.
const REGRESSION_LIMIT: f64 = 1.5;
const REGRESSION_FLOOR: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Scenario {
    /// SIGKILL the writer, promote the standby.
    KillWriter,
    /// Hold the writer's manifest swap and all its later storage traffic
    /// (segments are already written), promote the standby, then heal:
    /// the held swap reaches storage late.
    PartitionMidCommit,
    /// The writer's manifest swap lands but its response is lost. The
    /// writer resolves it by readback and keeps the role.
    DropSwapResponse,
    /// A second process starts as a writer on the same namespace.
    SecondWriter,
    /// `KillWriter` with `ingest_versioned` load (ADR 0056). Unacknowledged
    /// writes are retried blindly at the same source version, with no
    /// readback, and must still leave one generation per key.
    VersionedKillWriter,
}

const SCENARIOS: [Scenario; 5] = [
    Scenario::KillWriter,
    Scenario::PartitionMidCommit,
    Scenario::DropSwapResponse,
    Scenario::SecondWriter,
    Scenario::VersionedKillWriter,
];

#[derive(Debug, Serialize, Deserialize)]
struct Report {
    machine: Machine,
    backend: String,
    clients: usize,
    rounds: usize,
    paced_writes_per_sec: u32,
    /// Acknowledged writes per second of one writer under unpaced load
    /// from `clients` clients, one record per write.
    single_writer_writes_per_sec: f64,
    scenarios: Vec<ScenarioReport>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ScenarioReport {
    scenario: Scenario,
    time_to_accept_ms_median: u64,
    runs: Vec<Round>,
}

/// One round of one scenario.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Round {
    time_to_accept_ms: u64,
    acked_before: usize,
    acked_after: usize,
    /// Not acknowledged, then found committed by readback.
    committed_after_readback: usize,
    /// Not acknowledged, absent on readback, retried once.
    not_committed_after_readback: usize,
    /// Not acknowledged, retried at the same source version without
    /// readback (`VersionedKillWriter`).
    #[serde(default)]
    retried_blind: usize,
    lost: usize,
    duplicates: usize,
    /// `killed`, `fenced`, or `not displaced`.
    displaced_writer: String,
    errors: Vec<String>,
}

impl Round {
    fn passed(&self) -> bool {
        self.errors.is_empty() && self.lost == 0 && self.duplicates == 0
    }
}

// ---------------------------------------------------------------- fault proxy

#[derive(Default)]
struct ProxyState {
    partitioned: bool,
    partition_at_next_swap: bool,
    drop_next_swap_response: bool,
    held_at: Option<Instant>,
    dropped_at: Option<Instant>,
    stopped: bool,
}

/// Loopback TCP proxy in front of the object namespace for one node. The
/// object client sends one request per connection, so each connection is
/// one request: it can be held (partition) or have its response dropped.
struct FaultProxy {
    addr: SocketAddr,
    state: Arc<(Mutex<ProxyState>, Condvar)>,
    join: Option<JoinHandle<()>>,
}

impl FaultProxy {
    fn start(upstream: SocketAddr) -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("bind proxy: {e}"))?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        let state = Arc::new((Mutex::new(ProxyState::default()), Condvar::new()));
        let shared = Arc::clone(&state);
        let join = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if shared.0.lock().map(|s| s.stopped).unwrap_or(true) {
                    break;
                }
                let Ok(client) = stream else { break };
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || {
                    let _ = relay(client, upstream, &shared);
                });
            }
        });
        Ok(Self {
            addr,
            state,
            join: Some(join),
        })
    }

    fn endpoint(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn update(&self, change: impl FnOnce(&mut ProxyState)) {
        let (lock, cvar) = &*self.state;
        if let Ok(mut state) = lock.lock() {
            change(&mut state);
        }
        cvar.notify_all();
    }

    fn read<T>(&self, get: impl FnOnce(&ProxyState) -> T) -> Option<T> {
        self.state.0.lock().ok().map(|state| get(&state))
    }

    fn heal(&self) {
        self.update(|s| s.partitioned = false);
    }
}

impl Drop for FaultProxy {
    fn drop(&mut self) {
        self.update(|s| {
            s.stopped = true;
            s.partitioned = false;
        });
        // Same bounded wake-up as `LocalObjectServer`: never hang a round.
        let Some(join) = self.join.take() else {
            return;
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !join.is_finished() && Instant::now() < deadline {
            let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(50));
            std::thread::sleep(Duration::from_millis(10));
        }
        if join.is_finished() {
            let _ = join.join();
        }
    }
}

/// Read one request, apply the proxy state, forward it, and relay (or drop)
/// the response.
fn relay(
    client: TcpStream,
    upstream: SocketAddr,
    shared: &(Mutex<ProxyState>, Condvar),
) -> Result<(), String> {
    let mut reader = BufReader::new(client.try_clone().map_err(|e| e.to_string())?);
    let mut request = Vec::new();
    let mut content_length = 0usize;
    let mut first = true;
    let mut swap = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Ok(());
        }
        if first {
            let mut parts = line.split_whitespace();
            swap = parts.next() == Some("PUT")
                && parts.next().is_some_and(|p| p.starts_with("/manifest."));
            first = false;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
        request.extend_from_slice(line.as_bytes());
        if line == "\r\n" || line == "\n" {
            break;
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).map_err(|e| e.to_string())?;
    request.extend_from_slice(&body);

    let (lock, cvar) = shared;
    let mut state = lock.lock().map_err(|e| e.to_string())?;
    if swap && state.partition_at_next_swap {
        state.partition_at_next_swap = false;
        state.partitioned = true;
        state.held_at = Some(Instant::now());
    }
    while state.partitioned && !state.stopped {
        state = cvar.wait(state).map_err(|e| e.to_string())?;
    }
    let drop_response = swap && state.drop_next_swap_response;
    state.drop_next_swap_response &= !drop_response;
    drop(state);

    let mut server = TcpStream::connect(upstream).map_err(|e| e.to_string())?;
    server.write_all(&request).map_err(|e| e.to_string())?;
    let mut response = Vec::new();
    server
        .read_to_end(&mut response)
        .map_err(|e| e.to_string())?;
    if drop_response {
        if let Ok(mut state) = lock.lock() {
            state.dropped_at = Some(Instant::now());
        }
        return Ok(());
    }
    let mut client = client;
    client.write_all(&response).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- nodes

/// One `mikura-host` process with its own stub and its own route to the
/// shared object namespace.
struct Node {
    name: &'static str,
    log: PathBuf,
    proxy: FaultProxy,
    child: Option<Child>,
    addr: SocketAddr,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Writer,
    Standby,
}

impl Node {
    /// First node: creates the log on the namespace and serves as writer.
    fn create(name: &'static str, dir: &Path, upstream: SocketAddr) -> Result<Self, String> {
        let proxy = FaultProxy::start(upstream)?;
        let log = dir.join(format!("{name}.mikura"));
        Self::spawn(name, log, proxy, Role::Writer)
    }

    /// A node on another machine: a copy of the stub whose locator names
    /// this node's own route to the same namespace.
    fn attach(
        name: &'static str,
        dir: &Path,
        primary: &Path,
        upstream: SocketAddr,
        role: Role,
    ) -> Result<Self, String> {
        let proxy = FaultProxy::start(upstream)?;
        let log = attach_stub(dir, name, primary, &proxy.endpoint())?;
        Self::spawn(name, log, proxy, role)
    }

    fn spawn(
        name: &'static str,
        log: PathBuf,
        proxy: FaultProxy,
        role: Role,
    ) -> Result<Self, String> {
        let url = proxy.endpoint();
        let mut args = vec!["--backend", "object", "--object-url", url.as_str()];
        args.extend(["--min-version-timeout-ms", "5000"]);
        if role == Role::Standby {
            args.push("--standby");
        }
        let (child, addr) = spawn_host(&log, &args).map_err(|e| format!("{name}: {e}"))?;
        Ok(Self {
            name,
            log,
            proxy,
            child: Some(child),
            addr,
        })
    }

    fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn role(&self) -> Option<String> {
        host_rpc(self.addr, &serde_json::json!({"op": "health"}))
            .ok()?
            .health
            .map(|h| h.role)
    }

    fn promote(&self) -> Result<(), String> {
        let reply = host_rpc(self.addr, &serde_json::json!({"op": "promote"}))?;
        reply
            .ok
            .then_some(())
            .ok_or_else(|| format!("{} promote: {:?}", self.name, reply.error))
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Copy `primary`'s stub to `dir/{name}.mikura` with a locator for `endpoint`.
fn attach_stub(dir: &Path, name: &str, primary: &Path, endpoint: &str) -> Result<PathBuf, String> {
    let log = dir.join(format!("{name}.mikura"));
    std::fs::copy(primary, &log).map_err(|e| format!("copy stub: {e}"))?;
    let locator =
        std::fs::read_to_string(locator_path(primary)).map_err(|e| format!("read locator: {e}"))?;
    let magic = locator.lines().next().unwrap_or_default();
    std::fs::write(locator_path(&log), format!("{magic}\n{endpoint}\n"))
        .map_err(|e| format!("write locator: {e}"))?;
    Ok(log)
}

fn locator_path(log: &Path) -> PathBuf {
    let mut path = log.as_os_str().to_os_string();
    path.push(".obj");
    PathBuf::from(path)
}

// ---------------------------------------------------------------- load

struct Attempt {
    key: String,
    attempt: u64,
    node: &'static str,
    acked: bool,
    ended: Instant,
}

type Ledger = Arc<Mutex<Vec<Attempt>>>;

/// `CLIENTS` threads writing one new key per request to one node, together
/// at `rate` writes per second or unpaced, through `ingest_versioned` when
/// `versioned`. A client stops at its first
/// unacknowledged write, as a client that gives up on that node would;
/// readback settles the write afterwards. Dropping a load stops it.
struct Load {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Load {
    fn start(
        node: &Node,
        next: &Arc<AtomicU64>,
        ledger: &Ledger,
        rate: Option<u32>,
        versioned: bool,
    ) -> Self {
        let every = rate.map(|rate| Duration::from_secs(CLIENTS as u64) / rate.max(1));
        let stop = Arc::new(AtomicBool::new(false));
        let threads = (0..CLIENTS)
            .map(|_| {
                let (stop, next, ledger) =
                    (Arc::clone(&stop), Arc::clone(next), Arc::clone(ledger));
                let (name, addr) = (node.name, node.addr);
                std::thread::spawn(move || {
                    let mut due = Instant::now();
                    while !stop.load(Ordering::Relaxed) {
                        if let Some(every) = every {
                            due += every;
                            std::thread::sleep(due.saturating_duration_since(Instant::now()));
                        }
                        let id = next.fetch_add(1, Ordering::Relaxed);
                        let key = format!("k{id}");
                        let acked = write(addr, &key, id, versioned);
                        if let Ok(mut ledger) = ledger.lock() {
                            ledger.push(Attempt {
                                key,
                                attempt: id,
                                node: name,
                                acked,
                                ended: Instant::now(),
                            });
                        }
                        if !acked {
                            break;
                        }
                    }
                })
            })
            .collect();
        Self { stop, threads }
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    fn join(mut self) {
        self.stop();
        for thread in std::mem::take(&mut self.threads) {
            let _ = thread.join();
        }
    }
}

impl Drop for Load {
    fn drop(&mut self) {
        self.stop();
    }
}

/// One write of `attempt` to `key`. A versioned write uses `attempt + 1` as
/// its source version, so a retry of the same attempt is a duplicate.
fn write(addr: SocketAddr, key: &str, attempt: u64, versioned: bool) -> bool {
    let record = serde_json::json!({
        "gen": 1,
        "namespace": NS,
        "kind": KIND,
        "key": key,
        "hidden": false,
        "props": {"attempt": attempt.to_string()},
    });
    let request = if versioned {
        serde_json::json!({
            "op": "ingest_versioned",
            "record": record,
            "source_version": attempt + 1,
        })
    } else {
        serde_json::json!({"op": "ingest_batch", "records": [record]})
    };
    host_rpc(addr, &request).is_ok_and(|reply| reply.ok)
}

fn loaded(addr: SocketAddr, key: &str) -> Result<bool, String> {
    let reply = host_rpc(
        addr,
        &serde_json::json!({"op": "load", "namespace": NS, "kind": KIND, "key": key}),
    )?;
    Ok(reply.ok && reply.load.is_some())
}

/// Poll `ready` every 5 ms until it yields or `bound` passes.
fn wait_for<T>(bound: Duration, mut ready: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + bound;
    loop {
        if let Some(value) = ready() {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ---------------------------------------------------------------- one round

/// Temp directory unique to the process and round; removed on drop.
struct RoundDir(PathBuf);

impl RoundDir {
    fn new(label: impl std::fmt::Debug, round: usize) -> Result<Self, String> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mikura-drill-{label:?}-{round}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        Ok(Self(dir))
    }
}

impl Drop for RoundDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_round(scenario: Scenario, round: usize) -> Result<Round, String> {
    let dir = RoundDir::new(scenario, round)?;
    let server = LocalObjectServer::start()?;
    let upstream = server_addr(&server)?;
    let mut a = Node::create("a", &dir.0, upstream)?;
    let b = Node::attach("b", &dir.0, &a.log, upstream, Role::Standby)?;
    let next = Arc::new(AtomicU64::new(0));
    let ledger: Ledger = Arc::default();
    let mut out = Round::default();
    let versioned = scenario == Scenario::VersionedKillWriter;
    let load =
        |node: &Node| Load::start(node, &next, &ledger, Some(PACED_WRITES_PER_SEC), versioned);

    let mut loads = vec![load(&a)];
    std::thread::sleep(STEADY);

    // Inject the fault and run the operator step. `final_writer` is the node
    // expected to hold the writer role afterwards; `displaced` the one that
    // lost it.
    let mut second = None;
    let (t0, final_writer, displaced): (Instant, &Node, Option<&Node>) = match scenario {
        Scenario::KillWriter | Scenario::VersionedKillWriter => {
            a.kill();
            let t0 = Instant::now();
            b.promote()?;
            loads.push(load(&b));
            (t0, &b, None)
        }
        Scenario::PartitionMidCommit => {
            a.proxy.update(|s| s.partition_at_next_swap = true);
            let t0 = wait_for(ACCEPT_BOUND, || a.proxy.read(|s| s.held_at).flatten())
                .ok_or("writer made no manifest swap to partition")?;
            b.promote()?;
            loads.push(load(&b));
            (t0, &b, Some(&a))
        }
        Scenario::DropSwapResponse => {
            a.proxy.update(|s| s.drop_next_swap_response = true);
            let t0 = wait_for(ACCEPT_BOUND, || a.proxy.read(|s| s.dropped_at).flatten())
                .ok_or("writer made no manifest swap to drop")?;
            (t0, &a, None)
        }
        Scenario::SecondWriter => {
            let t0 = Instant::now();
            let c = Node::attach("c", &dir.0, &a.log, upstream, Role::Writer)?;
            loads.push(load(&c));
            second = Some(c);
            let c = second.as_ref().ok_or("second writer")?;
            // Both serve as writers until one swap fences the other.
            let winner = wait_for(ACCEPT_BOUND, || match (a.role()?, c.role()?) {
                (ra, rc) if ra == "fenced" && rc == "writer" => Some(true),
                (ra, rc) if ra == "writer" && rc == "fenced" => Some(false),
                _ => None,
            })
            .ok_or("neither writer was fenced")?;
            if winner {
                (t0, c, Some(&a))
            } else {
                (t0, &a, Some(c))
            }
        }
    };

    let accepted = wait_for(ACCEPT_BOUND, || {
        ledger.lock().ok()?.iter().find_map(|w| {
            (w.acked && w.node == final_writer.name && w.ended > t0).then_some(w.ended)
        })
    });
    match accepted {
        Some(at) => out.time_to_accept_ms = (at - t0).as_millis() as u64,
        None => out
            .errors
            .push(format!("no write accepted within {ACCEPT_BOUND:?}")),
    }
    std::thread::sleep(AFTER);
    for load in &loads {
        load.stop();
    }
    a.proxy.heal();
    for load in loads {
        load.join();
    }

    out.displaced_writer = match (scenario, displaced) {
        (Scenario::KillWriter | Scenario::VersionedKillWriter, _) => "killed".into(),
        (_, None) => "not displaced".into(),
        (_, Some(node)) => {
            let probe = format!("displaced-{}", node.name);
            let id = next.fetch_add(1, Ordering::Relaxed);
            if write(node.addr, &probe, id, versioned) {
                out.errors
                    .push(format!("displaced writer {} accepted a write", node.name));
            }
            match node.role().as_deref() {
                Some("fenced") => "fenced".into(),
                other => {
                    out.errors.push(format!(
                        "displaced writer {} reports role {other:?}",
                        node.name
                    ));
                    format!("{other:?}")
                }
            }
        }
    };

    let attempts = std::mem::take(&mut *ledger.lock().map_err(|e| e.to_string())?);
    out.acked_before = attempts.iter().filter(|w| w.acked && w.ended <= t0).count();
    out.acked_after = attempts.iter().filter(|w| w.acked && w.ended > t0).count();
    let mut expected: BTreeMap<String, u64> = attempts
        .iter()
        .filter(|w| w.acked)
        .map(|w| (w.key.clone(), w.attempt))
        .collect();
    let mut retried = Vec::new();
    for w in attempts.iter().filter(|w| !w.acked) {
        if versioned {
            // The same source version is a duplicate if the write landed.
            if write(final_writer.addr, &w.key, w.attempt, true) {
                out.retried_blind += 1;
                expected.insert(w.key.clone(), w.attempt);
                retried.push(w.key.clone());
            } else {
                out.errors
                    .push(format!("blind retry of {} was refused", w.key));
            }
            continue;
        }
        if loaded(final_writer.addr, &w.key)? {
            out.committed_after_readback += 1;
            expected.insert(w.key.clone(), w.attempt);
            continue;
        }
        out.not_committed_after_readback += 1;
        let retry = next.fetch_add(1, Ordering::Relaxed);
        if write(final_writer.addr, &w.key, retry, false) {
            expected.insert(w.key.clone(), retry);
            retried.push(w.key.clone());
        } else {
            out.errors.push(format!("retry of {} was refused", w.key));
        }
    }
    drop(second);
    drop(b);
    drop(a);
    verify_history(&dir.0, &server, &expected, &retried, &mut out)?;
    Ok(out)
}

/// One writer under unpaced load for `BURST`: acknowledged writes per second.
fn measure_throughput() -> Result<f64, String> {
    let dir = RoundDir::new("throughput", 0)?;
    let server = LocalObjectServer::start()?;
    let upstream = server_addr(&server)?;
    let writer = Node::create("a", &dir.0, upstream)?;
    let ledger: Ledger = Arc::default();
    let load = Load::start(&writer, &Arc::default(), &ledger, None, false);
    std::thread::sleep(BURST);
    load.join();
    let acked = ledger
        .lock()
        .map_err(|e| e.to_string())?
        .iter()
        .filter(|w| w.acked)
        .count();
    Ok(acked as f64 / BURST.as_secs_f64())
}

fn server_addr(server: &LocalObjectServer) -> Result<SocketAddr, String> {
    server
        .endpoint()
        .trim_start_matches("http://")
        .parse()
        .map_err(|_| "object server address".to_string())
}

/// Open the namespace fresh, read-only. Every key must hold the attempt
/// that was acknowledged or retried. Only a retried key can be written
/// twice, so only those are checked for a second generation (`history`
/// scans the whole log, too slow to run per key on a remote namespace).
fn verify_history(
    dir: &Path,
    server: &LocalObjectServer,
    expected: &BTreeMap<String, u64>,
    retried: &[String],
    out: &mut Round,
) -> Result<(), String> {
    let log = attach_stub(dir, "verify", &dir.join("a.mikura"), &server.endpoint())?;
    let http = ObjectBackend::http(server.endpoint(), None).map_err(|e| e.to_string())?;
    let store = Store::open_read_only_with(&log, http)?;
    let acl = PropertyAcl::allow_all();
    for (key, attempt) in expected {
        let Ok(record) = store.load(NS, KIND, key, &acl) else {
            out.lost += 1;
            continue;
        };
        let held = record.props.get("attempt");
        if held != Some(&attempt.to_string()) {
            out.errors
                .push(format!("{key}: holds attempt {held:?}, expected {attempt}"));
        }
    }
    for key in retried {
        if store.history(NS, KIND, key, &acl)?.len() > 1 {
            out.duplicates += 1;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- report

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values.get(values.len() / 2).copied().unwrap_or(0.0)
}

fn rounds() -> Result<usize, String> {
    match std::env::var("MIKURA_DRILL_ROUNDS") {
        Err(_) => Ok(DEFAULT_ROUNDS),
        Ok(raw) => raw
            .trim()
            .parse()
            .ok()
            .filter(|n| *n >= 1)
            .ok_or_else(|| format!("MIKURA_DRILL_ROUNDS `{raw}` must be >= 1")),
    }
}

/// Scenarios whose median time-to-accept regressed against `baseline`.
fn regressions(current: &[ScenarioReport], baseline: &[ScenarioReport]) -> Vec<String> {
    current
        .iter()
        .filter_map(|now| {
            let then = baseline.iter().find(|then| then.scenario == now.scenario)?;
            let (now_ms, then_ms) = (now.time_to_accept_ms_median, then.time_to_accept_ms_median);
            let limit = (then_ms as f64 * REGRESSION_LIMIT)
                .max((then_ms + REGRESSION_FLOOR.as_millis() as u64) as f64);
            (now_ms as f64 > limit).then(|| {
                format!(
                    "{:?}: time to accept {now_ms} ms vs baseline {then_ms} ms",
                    now.scenario
                )
            })
        })
        .collect()
}

fn run() -> Result<bool, String> {
    let machine = machine();
    let rounds = rounds()?;
    println!(
        "machine: {} {} {} cpus, {}; {CLIENTS} clients, {rounds} rounds",
        machine.os, machine.arch, machine.cpus, machine.cpu
    );
    println!(
        "{:<22} {:>5} {:>10} {:>8} {:>8} {:>9} {:>11} {:>6} {:>5} {:>4}  displaced",
        "scenario",
        "round",
        "accept ms",
        "before",
        "after",
        "readback",
        "not commit",
        "blind",
        "lost",
        "dup"
    );
    let mut scenarios = Vec::new();
    let mut failed = false;
    for scenario in SCENARIOS {
        let mut runs = Vec::new();
        for round in 0..rounds {
            let run = run_round(scenario, round).unwrap_or_else(|err| Round {
                errors: vec![err],
                ..Round::default()
            });
            println!(
                "{:<22} {:>5} {:>10} {:>8} {:>8} {:>9} {:>11} {:>6} {:>5} {:>4}  {}",
                format!("{scenario:?}"),
                round,
                run.time_to_accept_ms,
                run.acked_before,
                run.acked_after,
                run.committed_after_readback,
                run.not_committed_after_readback,
                run.retried_blind,
                run.lost,
                run.duplicates,
                run.displaced_writer
            );
            for err in &run.errors {
                eprintln!("drill {scenario:?} round {round}: {err}");
            }
            failed |= !run.passed();
            runs.push(run);
        }
        let accept = median(runs.iter().map(|r| r.time_to_accept_ms as f64).collect());
        scenarios.push(ScenarioReport {
            scenario,
            time_to_accept_ms_median: accept as u64,
            runs,
        });
    }
    let single_writer = measure_throughput()?;
    println!(
        "single writer: {single_writer:.0} acknowledged writes/s ({CLIENTS} unpaced clients, one record each)"
    );
    let report = Report {
        machine,
        backend: "object".into(),
        clients: CLIENTS,
        rounds,
        paced_writes_per_sec: PACED_WRITES_PER_SEC,
        single_writer_writes_per_sec: single_writer,
        scenarios,
    };
    if let Some(path) = std::env::var_os("MIKURA_DRILL_OUT") {
        let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
        std::fs::write(&path, json + "\n").map_err(|e| format!("write results: {e}"))?;
    }
    if failed {
        return Err("an oracle failed".into());
    }
    let Some(path) = std::env::var_os("MIKURA_DRILL_BASELINE") else {
        return Ok(true);
    };
    if !Path::new(&path).exists() {
        println!(
            "no baseline at {}; time-to-accept is not gated",
            Path::new(&path).display()
        );
        return Ok(true);
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("read baseline: {e}"))?;
    let baseline: Report =
        serde_json::from_str(&raw).map_err(|e| format!("parse baseline: {e}"))?;
    if baseline.machine != report.machine {
        return Err(format!(
            "baseline machine {:?} differs from this machine {:?}; compare on the same class",
            baseline.machine, report.machine
        ));
    }
    let regressed = regressions(&report.scenarios, &baseline.scenarios);
    for line in &regressed {
        eprintln!("regression: {line}");
    }
    Ok(regressed.is_empty())
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("drill: {error}");
            ExitCode::FAILURE
        }
    }
}
