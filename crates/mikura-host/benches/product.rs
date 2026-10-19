//! Product benchmark suite ([#255](https://github.com/Sannrox/mikura/issues/255)).
//!
//! Latency percentiles against the product `Store` and the in-process `Host`
//! on a seeded Customer -> Order -> Shipment fixture (spike 011 ratios:
//! `n/100` customers, `n/10` orders, the rest shipments; every 100th key
//! hidden). See `docs/benchmarks.md`.
//!
//! ```text
//! cargo bench -p mikura-host --bench product                 # 10^4 objects
//! MIKURA_BENCH_SIZES=10000,1000000 cargo bench -p mikura-host --bench product
//! MIKURA_BENCH_SIZES=10000000 ...                            # opt-in 10^7
//! MIKURA_BENCH_OUT=results.json ...                          # write JSON
//! MIKURA_BENCH_BASELINE=baseline.json ...                    # regression gate
//! ```
//!
//! Every measurement is lower-is-better. With a baseline, any p50 more than
//! 20% above the baseline p50 for the same name and size fails the run.

use mikura::{
    tenant_namespace, Aggregate, ComputeBackend, EvaluateRequest, ExactMatch, Hop, KeyPurpose,
    LocalCompute, LogKeys, MemoryKeyProvider, ObjectRecord, PropertyAcl, RuntimeProfile,
    SchemaDescriptor, SchemaLink, Sort, Store, TenantId, UnwrapGrant, STORE_KEY_TENANT,
};
use mikura_host::{Host, HostRequest};
use mikura_ingest::BatchIngest;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const NS: &str = "bench";
const DEFAULT_SIZES: &[u64] = &[10_000];
/// A p50 above `baseline * REGRESSION_LIMIT` fails the gate.
const REGRESSION_LIMIT: f64 = 1.20;
/// Per-measurement sampling: at least `MIN_SAMPLES`, then more until either
/// `MAX_SAMPLES` or `SAMPLE_BUDGET` is reached.
const MIN_SAMPLES: usize = 5;
const MAX_SAMPLES: usize = 2_000;
const SAMPLE_BUDGET: Duration = Duration::from_secs(3);
const HOST_READERS: usize = 4;
/// Writes add records, so they take a fixed count: compared runs then
/// measure the same database size whatever their speed.
const FIXED_WRITES: usize = 100;

#[derive(Debug, Serialize, Deserialize)]
struct Report {
    machine: Machine,
    results: Vec<Measurement>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Machine {
    os: String,
    arch: String,
    cpus: usize,
    cpu: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Measurement {
    name: String,
    objects: u64,
    unit: String,
    samples: usize,
    p50: u64,
    p95: u64,
    p99: u64,
}

impl Measurement {
    fn new(name: &str, objects: u64, unit: &str, mut values: Vec<u64>) -> Self {
        values.sort_unstable();
        let at = |q: f64| values[((values.len() - 1) as f64 * q).round() as usize];
        Self {
            name: name.into(),
            objects,
            unit: unit.into(),
            samples: values.len(),
            p50: at(0.50),
            p95: at(0.95),
            p99: at(0.99),
        }
    }

    fn single(name: &str, objects: u64, unit: &str, value: u64) -> Self {
        Self::new(name, objects, unit, vec![value])
    }
}

/// Temp directory for one size; removed on drop. Unique per process and run.
struct Fixture {
    dir: PathBuf,
    log: PathBuf,
}

impl Fixture {
    fn new(objects: u64) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "mikura-bench-{objects}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("bench temp dir");
        let log = dir.join("objects.mikura");
        Self { dir, log }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Scale {
    customers: u64,
    orders: u64,
    shipments: u64,
}

impl Scale {
    fn new(objects: u64) -> Self {
        let customers = (objects / 100).max(1);
        let orders = (objects / 10).max(1);
        Self {
            customers,
            orders,
            shipments: objects.saturating_sub(customers + orders).max(1),
        }
    }
}

/// Every 100th id is hidden, so visibility stays on the measured paths.
fn record(kind: &str, prefix: char, id: u64, props: Vec<(&str, String)>) -> ObjectRecord {
    ObjectRecord {
        gen: 1,
        namespace: NS.into(),
        kind: kind.into(),
        key: format!("{prefix}{id}"),
        hidden: id.is_multiple_of(100),
        action_id: None,
        props: props
            .into_iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect(),
        schema_version: 1,
    }
}

fn customer(id: u64) -> ObjectRecord {
    let region = ["eu", "us", "ap"][(id % 3) as usize];
    record("Customer", 'c', id, vec![("region", region.into())])
}

fn order(id: u64, scale: &Scale) -> ObjectRecord {
    let customer = format!("c{}", id % scale.customers);
    record("Order", 'o', id, vec![("customer_id", customer)])
}

/// `amount` is a declared sum; `weight` is not, so it takes the scan path.
fn shipment(id: u64, scale: &Scale) -> ObjectRecord {
    record(
        "Shipment",
        's',
        id,
        vec![
            ("order_id", format!("o{}", id % scale.orders)),
            ("amount", format!("{}", id % 50 + 1)),
            ("weight", format!("{}", id % 7 + 1)),
        ],
    )
}

/// The next visible fixture shipment key, striding so reads do not hit one
/// cache line. Hidden ids are skipped, so the key always exists.
fn next_visible(id: &mut u64, scale: &Scale) -> String {
    loop {
        *id = (*id + 7_919) % scale.shipments;
        if !id.is_multiple_of(100) {
            return format!("s{id}");
        }
    }
}

fn shipment_schema() -> ObjectRecord {
    SchemaDescriptor {
        kind: "Shipment".into(),
        properties: vec!["order_id".into(), "amount".into(), "weight".into()],
        links: vec![SchemaLink {
            name: "order_id".into(),
            far_kind: "Order".into(),
            outgoing: true,
        }],
        sums: vec!["amount".into()],
        ..SchemaDescriptor::default()
    }
    .to_record(NS)
    .expect("shipment schema")
}

fn timed(f: impl FnOnce()) -> u64 {
    let start = Instant::now();
    f();
    start.elapsed().as_nanos() as u64
}

/// Collect exactly `count` nanosecond samples from `once`.
fn fixed(count: usize, once: impl FnMut() -> u64) -> Vec<u64> {
    std::iter::repeat_with(once).take(count).collect()
}

/// Collect nanosecond samples from `once` within the sampling budget.
fn sample(mut once: impl FnMut() -> u64) -> Vec<u64> {
    let start = Instant::now();
    let mut values = Vec::new();
    while values.len() < MIN_SAMPLES
        || (values.len() < MAX_SAMPLES && start.elapsed() < SAMPLE_BUDGET)
    {
        values.push(once());
    }
    values
}

/// Peak resident set (Linux `VmHWM`), else current RSS from `ps`. Bytes.
fn peak_rss_bytes() -> u64 {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        if let Some(kib) = status
            .lines()
            .find_map(|line| line.strip_prefix("VmHWM:"))
            .and_then(|rest| {
                rest.trim()
                    .trim_end_matches("kB")
                    .trim()
                    .parse::<u64>()
                    .ok()
            })
        {
            return kib * 1024;
        }
    }
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|out| {
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse::<u64>()
                .ok()
        })
        .map_or(0, |kib| kib * 1024)
}

fn hop_request(root: &str, hops: Vec<Hop>, sum_property: &str) -> EvaluateRequest {
    EvaluateRequest {
        namespace: NS.into(),
        root_kind: root.into(),
        hops,
        sum_kind: "Shipment".into(),
        sum_property: sum_property.into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        ..EvaluateRequest::default()
    }
}

fn hop(far_kind: &str, join_property: &str) -> Hop {
    Hop {
        far_kind: far_kind.into(),
        join_property: join_property.into(),
        incoming: false,
        predicate: None,
    }
}

/// Run every measurement for one fixture size.
fn bench_size(objects: u64, results: &mut Vec<Measurement>) -> Result<(), String> {
    let fixture = Fixture::new(objects);
    let scale = Scale::new(objects);
    let mut store = Store::create(&fixture.log)?;
    store.append(shipment_schema())?;

    // Ingest the fixture in about twenty batches; each batch is a sample.
    let chunk = (objects / 20).max(100) as usize;
    let records = (0..scale.customers)
        .map(customer)
        .chain((0..scale.orders).map(|id| order(id, &scale)))
        .chain((0..scale.shipments).map(|id| shipment(id, &scale)));
    let mut batch = Vec::with_capacity(chunk);
    let mut per_record = Vec::new();
    for record in records {
        batch.push(record);
        if batch.len() == chunk {
            let start = Instant::now();
            BatchIngest::run(&mut store, std::mem::take(&mut batch))?;
            per_record.push(start.elapsed().as_nanos() as u64 / chunk as u64);
        }
    }
    if !batch.is_empty() {
        BatchIngest::run(&mut store, batch)?;
    }
    results.push(Measurement::new(
        "append_batch",
        objects,
        "ns/record",
        per_record,
    ));
    results.push(Measurement::single(
        "peak_rss",
        objects,
        "bytes",
        peak_rss_bytes(),
    ));
    // Before the time-budgeted appends below, so the size is deterministic.
    let log_bytes = std::fs::metadata(&fixture.log).map_or(0, |meta| meta.len());
    results.push(Measurement::single("log_size", objects, "bytes", log_bytes));

    let mut next = scale.shipments;
    results.push(Measurement::new(
        "append_commit",
        objects,
        "ns/op",
        fixed(FIXED_WRITES, || {
            let record = shipment(next, &scale);
            next += 1;
            timed(|| store.append(record).expect("append + commit"))
        }),
    ));

    let allow = PropertyAcl::allow_all();
    let deny = PropertyAcl::deny_property("Shipment", "amount");
    for (name, acl) in [("point_read", &allow), ("point_read_acl", &deny)] {
        let mut id = 1;
        results.push(Measurement::new(
            name,
            objects,
            "ns/op",
            sample(|| {
                let key = next_visible(&mut id, &scale);
                timed(|| {
                    store.load(NS, "Shipment", &key, acl).expect("load");
                })
            }),
        ));
    }

    // First sorted page of one seventh of the shipments.
    let list = EvaluateRequest {
        filter: Some(ExactMatch {
            property: "weight".into(),
            value: "3".into(),
        }),
        object_bound: scale.shipments as usize,
        sort: Some(Sort::by("amount")),
        page_size: 100,
        ..hop_request("Shipment", Vec::new(), "amount")
    };
    let list_open = EvaluateRequest {
        // append_commit has already added FIXED_WRITES extra shipments.
        object_bound: (scale.shipments as usize)
            .saturating_add(FIXED_WRITES)
            .saturating_add(1),
        sort: Some(Sort::by("amount")),
        page_size: 100,
        ..hop_request("Shipment", Vec::new(), "amount")
    };
    let list_acl = EvaluateRequest {
        acl: PropertyAcl::deny_property("Shipment", "weight"),
        ..list_open.clone()
    };
    let one_hop = vec![hop("Shipment", "order_id")];
    let two_hop = vec![hop("Order", "customer_id"), hop("Shipment", "order_id")];
    let queries = [
        ("list", list_open),
        ("list_acl", list_acl),
        ("list_filter", list),
        (
            "one_hop_sum_declared",
            hop_request("Order", one_hop.clone(), "amount"),
        ),
        (
            "one_hop_sum_undeclared",
            hop_request("Order", one_hop, "weight"),
        ),
        (
            "two_hop_sum_declared",
            hop_request("Customer", two_hop.clone(), "amount"),
        ),
        (
            "two_hop_sum_undeclared",
            hop_request("Customer", two_hop, "weight"),
        ),
    ];
    for (name, request) in &queries {
        // Undeclared sums build last-hop rollups on first use (#258).
        // Warm that path so samples are the cached evaluate, matching the
        // published "after first use" budget.
        LocalCompute.evaluate(&store, request).expect(name);
        results.push(Measurement::new(
            name,
            objects,
            "ns/op",
            sample(|| {
                timed(|| {
                    LocalCompute.evaluate(&store, request).expect(name);
                })
            }),
        ));
    }

    drop(store);
    results.push(Measurement::new(
        "open_with_sidecar",
        objects,
        "ns/op",
        sample(|| timed(|| drop(Store::open(&fixture.log).expect("open")))),
    ));
    results.push(Measurement::new(
        "open_without_sidecar",
        objects,
        "ns/op",
        sample(|| {
            remove_projections(&fixture.log);
            timed(|| drop(Store::open(&fixture.log).expect("open")))
        }),
    ));

    let (reads, writes) = host_mixed(&fixture.log, &scale)?;
    results.push(Measurement::new(
        "host_read_under_writes",
        objects,
        "ns/op",
        reads,
    ));
    results.push(Measurement::new(
        "host_write_under_reads",
        objects,
        "ns/op",
        writes,
    ));
    Ok(())
}

/// Delete every rebuildable projection so `open` replays the log.
fn remove_projections(log: &Path) {
    for path in [
        Store::join_map_path(log),
        Store::join_delta_path(log),
        Store::checkpoint_path(log),
        Store::checkpoint_prev_path(log),
    ] {
        let _ = std::fs::remove_file(path);
    }
}

/// Reader threads load shipments while the caller ingests 100-record batches
/// through the same `Host`. Returns (read samples, write samples).
fn host_mixed(log: &Path, scale: &Scale) -> Result<(Vec<u64>, Vec<u64>), String> {
    let host = Host::open(log, 1_024)?;
    let stop = AtomicBool::new(false);
    let mut next = scale.shipments * 2;
    std::thread::scope(|scope| {
        let readers: Vec<_> = (0..HOST_READERS)
            .map(|reader| {
                let (host, stop) = (&host, &stop);
                scope.spawn(move || {
                    let mut samples = Vec::new();
                    let mut id = reader as u64 + 1;
                    // Keep loading until the writer stops; only recording is capped.
                    while !stop.load(Ordering::Relaxed) {
                        let request = HostRequest::Load {
                            namespace: NS.into(),
                            kind: "Shipment".into(),
                            key: next_visible(&mut id, scale),
                            deny: Vec::new(),
                            restriction: None,
                        };
                        let elapsed = timed(|| assert!(host.handle(request).ok, "host load"));
                        if samples.len() < MAX_SAMPLES {
                            samples.push(elapsed);
                        }
                    }
                    samples
                })
            })
            .collect();
        let writes = fixed(FIXED_WRITES, || {
            let records: Vec<_> = (next..next + 100).map(|id| shipment(id, scale)).collect();
            next += 100;
            timed(|| {
                assert!(
                    host.handle(HostRequest::IngestBatch { records }).ok,
                    "host ingest"
                )
            })
        });
        stop.store(true, Ordering::Relaxed);
        let reads = readers
            .into_iter()
            .flat_map(|reader| reader.join().expect("reader thread"))
            .collect();
        Ok((reads, writes))
    })
}

fn bench_encrypted_append(results: &mut Vec<Measurement>) -> Result<(), String> {
    let provider: std::sync::Arc<dyn mikura::KeyProvider + Send + Sync> = std::sync::Arc::new(
        MemoryKeyProvider::new(RuntimeProfile::Development).map_err(|e| e.to_string())?,
    );
    let grant = |tenant: &str| -> Result<UnwrapGrant, String> {
        Ok(UnwrapGrant {
            tenant_id: TenantId::new(tenant).map_err(|e| e.to_string())?,
            purpose: KeyPurpose::Store,
            expires_unix_secs: i64::MAX,
        })
    };
    let keys = LogKeys::new(provider, vec![grant(STORE_KEY_TENANT)?, grant("acme")?])?;
    let fixture = Fixture::new(FIXED_WRITES as u64);
    let ns = tenant_namespace("acme", NS)?;
    let mut store = Store::create_with_keys(&fixture.log, keys)?;
    let mut values = Vec::with_capacity(FIXED_WRITES);
    for id in 0..FIXED_WRITES as u64 {
        let mut record = shipment(id, &Scale::new(FIXED_WRITES as u64));
        record.namespace = ns.clone();
        values.push(timed(|| store.append(record).expect("encrypted append")));
    }
    results.push(Measurement::new(
        "encrypted_append",
        FIXED_WRITES as u64,
        "ns/op",
        values,
    ));
    Ok(())
}

fn machine() -> Machine {
    let cpu = std::process::Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|name| !name.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/proc/cpuinfo")
                .ok()
                .and_then(|info| {
                    info.lines()
                        .find_map(|line| line.strip_prefix("model name"))
                        .map(|rest| rest.trim_start_matches([' ', '\t', ':']).to_string())
                })
        })
        .unwrap_or_else(|| "unknown".into());
    Machine {
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        cpus: std::thread::available_parallelism().map_or(1, usize::from),
        cpu,
    }
}

fn sizes() -> Result<Vec<u64>, String> {
    let Ok(raw) = std::env::var("MIKURA_BENCH_SIZES") else {
        return Ok(DEFAULT_SIZES.to_vec());
    };
    let mut sizes = raw
        .split(',')
        .map(|part| {
            part.trim()
                .parse::<u64>()
                .ok()
                .filter(|n| *n >= 1_000)
                .ok_or_else(|| format!("MIKURA_BENCH_SIZES entry `{part}` must be >= 1000"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    // Ascending, so the process-wide peak RSS after each size is that size's.
    sizes.sort_unstable();
    Ok(sizes)
}

/// Regressions of `current` p50 against `baseline` beyond the limit.
fn regressions(current: &[Measurement], baseline: &[Measurement]) -> Vec<String> {
    current
        .iter()
        .filter_map(|now| {
            let then = baseline
                .iter()
                .find(|then| then.name == now.name && then.objects == now.objects)?;
            (now.p50 as f64 > then.p50 as f64 * REGRESSION_LIMIT).then(|| {
                format!(
                    "{} @ {}: p50 {} {} vs baseline {} (+{:.0}%)",
                    now.name,
                    now.objects,
                    now.p50,
                    now.unit,
                    then.p50,
                    (now.p50 as f64 / then.p50.max(1) as f64 - 1.0) * 100.0
                )
            })
        })
        .collect()
}

fn run() -> Result<bool, String> {
    let machine = machine();
    println!(
        "machine: {} {} {} cpus, {}",
        machine.os, machine.arch, machine.cpus, machine.cpu
    );
    let mut results = Vec::new();
    for objects in sizes()? {
        bench_size(objects, &mut results)?;
    }
    bench_encrypted_append(&mut results)?;
    println!(
        "{:<26} {:>10} {:>10} {:>8} {:>14} {:>14} {:>14}",
        "measurement", "objects", "unit", "samples", "p50", "p95", "p99"
    );
    for m in &results {
        println!(
            "{:<26} {:>10} {:>10} {:>8} {:>14} {:>14} {:>14}",
            m.name, m.objects, m.unit, m.samples, m.p50, m.p95, m.p99
        );
    }
    for m in &results {
        if matches!(m.name.as_str(), "point_read" | "point_read_acl") && m.p99 > 1_000_000 {
            return Err(format!(
                "{} @ {} p99 {} ns exceeds 1 ms",
                m.name, m.objects, m.p99
            ));
        }
    }
    let mut sizes: Vec<u64> = results.iter().map(|m| m.objects).collect();
    sizes.sort_unstable();
    sizes.dedup();
    for objects in sizes {
        let open = results
            .iter()
            .find(|m| m.name == "list" && m.objects == objects);
        let acl = results
            .iter()
            .find(|m| m.name == "list_acl" && m.objects == objects);
        if let (Some(open), Some(acl)) = (open, acl) {
            let ratio = acl.p50 as f64 / open.p50.max(1) as f64;
            println!(
                "list_acl p50 overhead @ {objects}: {:+.1}%",
                (ratio - 1.0) * 100.0
            );
        }
    }
    let report = Report { machine, results };
    if let Some(path) = std::env::var_os("MIKURA_BENCH_OUT") {
        let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
        std::fs::write(&path, json + "\n").map_err(|e| format!("write results: {e}"))?;
    }
    let Some(path) = std::env::var_os("MIKURA_BENCH_BASELINE") else {
        return Ok(true);
    };
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("read baseline: {e}"))?;
    let baseline: Report =
        serde_json::from_str(&raw).map_err(|e| format!("parse baseline: {e}"))?;
    if baseline.machine != report.machine {
        return Err(format!(
            "baseline machine {:?} differs from this machine {:?}; compare on the same class",
            baseline.machine, report.machine
        ));
    }
    let failed = regressions(&report.results, &baseline.results);
    for line in &failed {
        eprintln!("regression: {line}");
    }
    Ok(failed.is_empty())
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("bench: {error}");
            ExitCode::FAILURE
        }
    }
}
