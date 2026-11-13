//! Throwaway: after ADR 0040 split commit, do host reads still wait on
//! group-commit fsync, or only on exclusive apply? Does a second projection
//! generation buy enough to justify the copy?
//!
//! Not the mikura store of record.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mikura::{ObjectRecord, PropertyAcl, SchemaDescriptor, SchemaLink, Store};
use mikura_host::{Host, HostRequest, WireEvaluate, WireHop};
use mikura_ingest::BatchIngest;

const NS: &str = "bench";
const HOST_READERS: usize = 8;
const HOST_EVALUATORS: usize = 2;
const FIXED_WRITES: usize = 100;
const MIN_SAMPLES: usize = 5;
const MAX_SAMPLES: usize = 2_000;
const SAMPLE_BUDGET: Duration = Duration::from_secs(3);

struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("scratch")
            .join(format!("017-{}-{}-{}", std::process::id(), label, nanos));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        Self { dir }
    }

    fn log(&self) -> PathBuf {
        self.dir.join("objects.mikura")
    }
}

impl Drop for Scratch {
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

struct Percentiles {
    samples: usize,
    p50: u64,
    p95: u64,
    p99: u64,
}

impl Percentiles {
    fn from(mut values: Vec<u64>) -> Self {
        values.sort_unstable();
        let at = |q: f64| values[((values.len() - 1) as f64 * q).round() as usize];
        Self {
            samples: values.len(),
            p50: at(0.50),
            p95: at(0.95),
            p99: at(0.99),
        }
    }
}

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
    record(
        "Order",
        'o',
        id,
        vec![("customer_id", format!("c{}", id % scale.customers))],
    )
}

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

fn seed(log: &Path, objects: u64) -> Result<Scale, String> {
    let scale = Scale::new(objects);
    let mut store = Store::create(log)?;
    store.append(shipment_schema())?;
    let chunk = (objects / 20).max(100) as usize;
    let records = (0..scale.customers)
        .map(customer)
        .chain((0..scale.orders).map(|id| order(id, &scale)))
        .chain((0..scale.shipments).map(|id| shipment(id, &scale)));
    let mut batch = Vec::with_capacity(chunk);
    for record in records {
        batch.push(record);
        if batch.len() == chunk {
            BatchIngest::run(&mut store, std::mem::take(&mut batch))?;
        }
    }
    if !batch.is_empty() {
        BatchIngest::run(&mut store, batch)?;
    }
    Ok(scale)
}

fn load_req(key: String) -> HostRequest {
    HostRequest::Load {
        namespace: NS.into(),
        kind: "Shipment".into(),
        key,
        deny: Vec::new(),
        restriction: None,
    }
}

fn one_hop_eval() -> HostRequest {
    HostRequest::Evaluate {
        request: Box::new(WireEvaluate {
            namespace: NS.into(),
            root_kind: "Order".into(),
            hops: vec![WireHop {
                far_kind: "Shipment".into(),
                join_property: "order_id".into(),
                incoming: false,
                predicate: None,
            }],
            sum_kind: "Shipment".into(),
            sum_property: "amount".into(),
            deny: Vec::new(),
            restriction: None,
            filter: None,
            predicate: None,
            object_bound: 0,
            sort: None,
            then_by: Vec::new(),
            page_size: 0,
            cursor: None,
            breakdown: None,
        }),
    }
}

fn fmt_ns(ns: u64) -> String {
    if ns >= 1_000_000 {
        format!("{:.2} ms", ns as f64 / 1_000_000.0)
    } else if ns >= 1_000 {
        format!("{:.1} µs", ns as f64 / 1_000.0)
    } else {
        format!("{ns} ns")
    }
}

fn print_row(name: &str, objects: u64, p: &Percentiles) {
    println!(
        "{name} objects={objects} samples={} p50={} p95={} p99={}",
        p.samples,
        fmt_ns(p.p50),
        fmt_ns(p.p95),
        fmt_ns(p.p99)
    );
}

fn host_idle_reads(host: &Host, scale: &Scale) -> Percentiles {
    let mut id = 1;
    Percentiles::from(sample(|| {
        let key = next_visible(&mut id, scale);
        timed(|| {
            assert!(host.handle(load_req(key.clone())).ok, "idle load");
        })
    }))
}

struct MixedHost {
    idle: Percentiles,
    reads: Percentiles,
    writes: Percentiles,
    alone: Percentiles,
    evals: Percentiles,
    reads_under_eval: Percentiles,
    dual_ok: bool,
}

fn host_mixed(log: &Path, scale: &Scale) -> Result<MixedHost, String> {
    let host = Host::open(log, 1_024)?;
    let idle = host_idle_reads(&host, scale);
    let stop = AtomicBool::new(false);
    let mut next = scale.shipments * 2;
    let mut write = || {
        let records: Vec<_> = (next..next + 100).map(|id| shipment(id, scale)).collect();
        next += 100;
        timed(|| {
            assert!(
                host.handle(HostRequest::IngestBatch { records }).ok,
                "host ingest"
            );
        })
    };
    let alone = Percentiles::from((0..FIXED_WRITES).map(|_| write()).collect());
    let (reads, writes) = std::thread::scope(|scope| {
        let readers: Vec<_> = (0..HOST_READERS)
            .map(|reader| {
                let (host, stop) = (&host, &stop);
                scope.spawn(move || {
                    let mut samples = Vec::new();
                    let mut id = reader as u64 + 1;
                    while !stop.load(Ordering::Relaxed) {
                        let key = next_visible(&mut id, scale);
                        let elapsed = timed(|| {
                            assert!(host.handle(load_req(key.clone())).ok, "host load");
                        });
                        if samples.len() < MAX_SAMPLES {
                            samples.push(elapsed);
                        }
                    }
                    samples
                })
            })
            .collect();
        let writes = Percentiles::from((0..FIXED_WRITES).map(|_| write()).collect());
        stop.store(true, Ordering::Relaxed);
        let reads = Percentiles::from(
            readers
                .into_iter()
                .flat_map(|reader| reader.join().expect("reader"))
                .collect(),
        );
        (reads, writes)
    });
    stop.store(false, Ordering::Relaxed);
    let (evals, reads_under_eval) = std::thread::scope(|scope| {
        let evaluators: Vec<_> = (0..HOST_EVALUATORS)
            .map(|_| {
                let (host, stop) = (&host, &stop);
                scope.spawn(move || {
                    let mut samples = Vec::new();
                    while !stop.load(Ordering::Relaxed) {
                        let elapsed = timed(|| {
                            assert!(host.handle(one_hop_eval()).ok, "host evaluate");
                        });
                        if samples.len() < MAX_SAMPLES {
                            samples.push(elapsed);
                        }
                    }
                    samples
                })
            })
            .collect();
        let readers: Vec<_> = (0..HOST_READERS)
            .map(|reader| {
                let (host, stop) = (&host, &stop);
                scope.spawn(move || {
                    let mut samples = Vec::new();
                    let mut id = reader as u64 + 1;
                    while !stop.load(Ordering::Relaxed) {
                        let key = next_visible(&mut id, scale);
                        let elapsed = timed(|| {
                            assert!(host.handle(load_req(key.clone())).ok, "host load");
                        });
                        if samples.len() < MAX_SAMPLES {
                            samples.push(elapsed);
                        }
                    }
                    samples
                })
            })
            .collect();
        for _ in 0..FIXED_WRITES {
            let _ = write();
        }
        stop.store(true, Ordering::Relaxed);
        let evals = Percentiles::from(
            evaluators
                .into_iter()
                .flat_map(|t| t.join().expect("evaluator"))
                .collect(),
        );
        let reads_under_eval = Percentiles::from(
            readers
                .into_iter()
                .flat_map(|t| t.join().expect("reader"))
                .collect(),
        );
        (evals, reads_under_eval)
    });
    let probe_keys = [
        next_visible(&mut 1u64, scale),
        format!("s{}", next - 99),
        format!("s{}", next - 1),
    ];
    let live: Vec<_> = probe_keys
        .iter()
        .map(|key| {
            let response = host.handle(load_req(key.clone()));
            (key.clone(), response.ok, response.load)
        })
        .collect();
    drop(host);
    let replayed = Store::open(log)?;
    let acl = PropertyAcl::allow_all();
    let dual_ok = live.iter().all(|(key, ok, record)| {
        let from_log = replayed.load(NS, "Shipment", key, &acl);
        match (ok, record, from_log) {
            (true, Some(live), Ok(replay)) => {
                !live.hidden && live.gen == replay.gen && live.props == replay.props
            }
            _ => false,
        }
    });
    Ok(MixedHost {
        idle,
        reads,
        writes,
        alone,
        evals,
        reads_under_eval,
        dual_ok,
    })
}

fn rss_bytes() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("VmRSS:").and_then(|rest| {
                    rest.trim()
                        .trim_end_matches("kB")
                        .trim()
                        .parse::<u64>()
                        .ok()
                })
            })
        })
        .map_or(0, |kib| kib * 1024)
}

struct Toy {
    clone: Percentiles,
    clone_rss_before: u64,
    clone_rss_after: u64,
    exclusive_reads: Percentiles,
    snapshot_reads: Percentiles,
}

fn fill_map(n: u64) -> HashMap<u64, u64> {
    let mut map = HashMap::with_capacity(n as usize);
    for i in 0..n {
        map.insert(i, i);
    }
    map
}

fn toy(n: u64) -> Toy {
    let base = fill_map(n);
    let before = rss_bytes();
    let held = base.clone();
    let after = rss_bytes();
    let clone_samples: Vec<u64> = (0..8)
        .map(|_| {
            let src = &base;
            timed(|| {
                let _ = src.clone();
            })
        })
        .collect();
    drop(held);
    let clone = Percentiles::from(clone_samples);

    let exclusive = RwLock::new(fill_map(n));
    let stop = AtomicBool::new(false);
    let next = AtomicU64::new(n);
    let exclusive_reads = std::thread::scope(|scope| {
        let readers: Vec<_> = (0..HOST_READERS)
            .map(|reader| {
                let (exclusive, stop) = (&exclusive, &stop);
                scope.spawn(move || {
                    let mut samples = Vec::new();
                    let mut id = reader as u64;
                    while !stop.load(Ordering::Relaxed) {
                        let elapsed = timed(|| {
                            let map = exclusive.read().expect("exclusive read");
                            let _ = map.get(&(id % n));
                        });
                        id = id.wrapping_add(7_919);
                        if samples.len() < MAX_SAMPLES {
                            samples.push(elapsed);
                        }
                    }
                    samples
                })
            })
            .collect();
        for _ in 0..FIXED_WRITES {
            let start = next.fetch_add(100, Ordering::Relaxed);
            let mut map = exclusive.write().expect("exclusive write");
            for i in 0..100 {
                map.insert(start + i, start + i);
            }
        }
        stop.store(true, Ordering::Relaxed);
        Percentiles::from(
            readers
                .into_iter()
                .flat_map(|reader| reader.join().expect("reader"))
                .collect(),
        )
    });

    let snapshot = RwLock::new(Arc::new(fill_map(n)));
    stop.store(false, Ordering::Relaxed);
    let next = AtomicU64::new(n);
    let snapshot_reads = std::thread::scope(|scope| {
        let readers: Vec<_> = (0..HOST_READERS)
            .map(|reader| {
                let (snapshot, stop) = (&snapshot, &stop);
                scope.spawn(move || {
                    let mut samples = Vec::new();
                    let mut id = reader as u64;
                    while !stop.load(Ordering::Relaxed) {
                        let elapsed = timed(|| {
                            let pin = snapshot.read().expect("snapshot pin").clone();
                            let _ = pin.get(&(id % n));
                        });
                        id = id.wrapping_add(7_919);
                        if samples.len() < MAX_SAMPLES {
                            samples.push(elapsed);
                        }
                    }
                    samples
                })
            })
            .collect();
        for _ in 0..FIXED_WRITES {
            let start = next.fetch_add(100, Ordering::Relaxed);
            let current = snapshot.read().expect("snapshot base").clone();
            let mut next_map = (*current).clone();
            for i in 0..100 {
                next_map.insert(start + i, start + i);
            }
            *snapshot.write().expect("snapshot swap") = Arc::new(next_map);
        }
        stop.store(true, Ordering::Relaxed);
        Percentiles::from(
            readers
                .into_iter()
                .flat_map(|reader| reader.join().expect("reader"))
                .collect(),
        )
    });

    Toy {
        clone,
        clone_rss_before: before,
        clone_rss_after: after,
        exclusive_reads,
        snapshot_reads,
    }
}

fn run_size(objects: u64, mode: &str) -> Result<(), String> {
    println!("--- objects={objects} ---");
    if mode == "all" || mode == "toy" {
        let toy = toy(objects);
        print_row("hashmap_clone", objects, &toy.clone);
        println!(
            "hashmap_clone_rss_before={} hashmap_clone_rss_after={}",
            toy.clone_rss_before, toy.clone_rss_after
        );
        print_row("exclusive_rwlock_read", objects, &toy.exclusive_reads);
        print_row("arc_swap_read", objects, &toy.snapshot_reads);
    }
    if mode == "all" || mode == "host" {
        let scratch = Scratch::new(&format!("n{objects}"));
        let log = scratch.log();
        let scale = seed(&log, objects)?;
        let mixed = host_mixed(&log, &scale)?;
        print_row("host_idle_load", objects, &mixed.idle);
        print_row("host_read_under_writes", objects, &mixed.reads);
        print_row("host_write_under_reads", objects, &mixed.writes);
        print_row("host_write_alone", objects, &mixed.alone);
        print_row("host_eval_under_writes", objects, &mixed.evals);
        print_row(
            "host_read_under_eval_and_writes",
            objects,
            &mixed.reads_under_eval,
        );
        println!("dual_read_ok={}", mixed.dual_ok);
        if !mixed.dual_ok {
            return Err("dual-read mismatch".into());
        }
    }
    Ok(())
}

fn arg_str(name: &str, default: &str) -> String {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == name {
            return args.next().unwrap_or_else(|| default.to_string());
        }
        if let Some(value) = arg.strip_prefix(&format!("{name}=")) {
            return value.to_string();
        }
    }
    default.to_string()
}

fn machine_class() -> (String, String, usize, String) {
    let os = std::env::consts::OS.to_string();
    let arch = std::env::consts::ARCH.to_string();
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("model name"))
                .map(|rest| rest.trim().trim_start_matches(':').trim().to_string())
        })
        .unwrap_or_else(|| "unknown".into());
    (os, arch, cpus, cpu)
}

fn main() {
    let objects = arg_str("--objects", "10000");
    let mode = arg_str("--mode", "all");
    if !matches!(mode.as_str(), "all" | "host" | "toy") {
        eprintln!("--mode must be all, host, or toy");
        std::process::exit(1);
    }
    let machine = machine_class();
    println!("machine_os={}", machine.0);
    println!("machine_arch={}", machine.1);
    println!("machine_cpus={}", machine.2);
    println!("machine_cpu={}", machine.3);
    for part in objects.split(',') {
        let n: u64 = match part.trim().parse() {
            Ok(n) if n >= 1_000 => n,
            _ => {
                eprintln!("--objects entries must be >= 1000");
                std::process::exit(1);
            }
        };
        if let Err(error) = run_size(n, &mode) {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikura_host::{WireEvaluate, WireFilter};

    fn rec(kind: &str, key: &str) -> ObjectRecord {
        ObjectRecord {
            gen: 1,
            namespace: NS.into(),
            kind: kind.into(),
            key: key.into(),
            hidden: false,
            action_id: None,
            props: HashMap::from([("n".into(), "1".into())]),
            schema_version: 1,
        }
    }

    #[test]
    fn stream_push_is_hidden_until_flush() {
        let scratch = Scratch::new("stream");
        let host = Host::open(&scratch.log(), 8).unwrap();
        let seed = host.handle(HostRequest::IngestBatch {
            records: vec![rec("Shipment", "seed")],
        });
        assert!(seed.ok, "{seed:?}");
        let push = host.handle(HostRequest::IngestStreamPush {
            record: rec("Shipment", "buffered"),
        });
        assert!(push.ok, "{push:?}");
        let before = host.handle(load_req("buffered".into()));
        assert!(!before.ok, "uncommitted push was visible: {before:?}");
        let flush = host.handle(HostRequest::IngestStreamFlush);
        assert!(flush.ok, "{flush:?}");
        let after = host.handle(load_req("buffered".into()));
        assert!(after.ok, "{after:?}");
        assert_eq!(
            after.load.as_ref().map(|r| r.key.as_str()),
            Some("buffered")
        );
    }

    #[test]
    fn host_load_matches_log_replay_after_batch() {
        let scratch = Scratch::new("dual");
        let log = scratch.log();
        let host = Host::open(&log, 8).unwrap();
        let ingest = host.handle(HostRequest::IngestBatch {
            records: (0..8).map(|i| rec("Shipment", &format!("s{i}"))).collect(),
        });
        assert!(ingest.ok, "{ingest:?}");
        let live = host.handle(load_req("s3".into()));
        assert!(live.ok, "{live:?}");
        let live_rec = live.load.expect("live load");
        drop(host);
        let replayed = Store::open(&log).unwrap();
        let from_log = replayed
            .load(NS, "Shipment", "s3", &PropertyAcl::allow_all())
            .unwrap();
        assert_eq!(live_rec.gen, from_log.gen);
        assert_eq!(live_rec.props, from_log.props);
    }

    fn batch_count(host: &Host) -> usize {
        let response = host.handle(HostRequest::Evaluate {
            request: Box::new(WireEvaluate {
                namespace: NS.into(),
                root_kind: "Shipment".into(),
                hops: Vec::new(),
                sum_kind: "Shipment".into(),
                sum_property: "n".into(),
                deny: Vec::new(),
                restriction: None,
                filter: Some(WireFilter {
                    property: "batch".into(),
                    value: "yes".into(),
                }),
                predicate: None,
                object_bound: 0,
                sort: None,
                then_by: Vec::new(),
                page_size: 0,
                cursor: None,
                breakdown: None,
            }),
        });
        assert!(response.ok, "{response:?}");
        response.evaluate.expect("evaluate").two_hop_count
    }

    #[test]
    fn concurrent_readers_see_batch_whole_or_none() {
        let scratch = Scratch::new("whole");
        let host = Host::open(&scratch.log(), 8).unwrap();
        let seed = host.handle(HostRequest::IngestBatch {
            records: vec![rec("Shipment", "seed")],
        });
        assert!(seed.ok, "{seed:?}");
        let stop = AtomicBool::new(false);
        let torn = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let readers: Vec<_> = (0..4)
                .map(|_| {
                    let (host, stop, torn) = (&host, &stop, &torn);
                    scope.spawn(move || {
                        while !stop.load(Ordering::Relaxed) {
                            let count = batch_count(host);
                            if count != 0 && count != 100 {
                                torn.store(true, Ordering::Relaxed);
                            }
                        }
                    })
                })
                .collect();
            let records: Vec<_> = (0..100)
                .map(|i| {
                    let mut row = rec("Shipment", &format!("b{i}"));
                    row.props.insert("batch".into(), "yes".into());
                    row
                })
                .collect();
            let ingest = host.handle(HostRequest::IngestBatch { records });
            assert!(ingest.ok, "{ingest:?}");
            stop.store(true, Ordering::Relaxed);
            for reader in readers {
                reader.join().expect("reader");
            }
        });
        assert!(!torn.load(Ordering::Relaxed), "reader saw a torn batch");
        assert_eq!(batch_count(&host), 100);
    }

    #[test]
    fn arc_swap_readers_keep_previous_generation_during_clone() {
        let map = RwLock::new(Arc::new(HashMap::from([(0u64, 0u64)])));
        let current = map.read().unwrap().clone();
        let mut next = (*current).clone();
        next.insert(1, 1);
        let pinned = map.read().unwrap().clone();
        assert_eq!(pinned.get(&0), Some(&0));
        assert_eq!(pinned.get(&1), None);
        *map.write().unwrap() = Arc::new(next);
        let swapped = map.read().unwrap().clone();
        assert_eq!(swapped.get(&1), Some(&1));
        assert_eq!(pinned.get(&1), None);
    }
}
