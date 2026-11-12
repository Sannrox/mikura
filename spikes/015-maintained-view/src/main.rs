//! Throwaway: does a sabun view kept current at commit beat evaluate-time
//! `Breakdown` on the product `Store` enough to justify a second path?
//!
//! Fixture is the product bench: Customer → Order → Shipment, spike 011
//! ratios, every 100th key hidden. The measured breakdown is zero-hop
//! `Shipment` grouped by `weight` with sum `amount` (ADR 0038).
//!
//! Not the store of record. sabun is a spike dependency only.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use mikura::{
    Aggregate, Breakdown, ComputeBackend, EvaluateRequest, LocalCompute, ObjectRecord, PropertyAcl,
    SchemaDescriptor, SchemaLink, Store,
};
use sabun::aggregate::{Count, Sum};
use sabun::zset::ZSet;

const NS: &str = "bench";

fn main() {
    let objects = arg_u64("--objects", 10_000);
    if objects < 1_000 {
        eprintln!("--objects must be at least 1000");
        std::process::exit(2);
    }
    match run(objects) {
        Ok(()) => {}
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

fn run(objects: u64) -> Result<(), String> {
    let scale = Scale::new(objects);
    let dir = temp_dir("run", objects);
    let log = dir.join("objects.mikura");
    let mut store = Store::create(&log)?;
    store.append(shipment_schema(1, &["amount"]))?;

    let mut view = View::new();
    let chunk = (objects / 20).max(100) as usize;
    let records = (0..scale.customers)
        .map(customer)
        .chain((0..scale.orders).map(|id| order(id, &scale)))
        .chain((0..scale.shipments).map(|id| shipment(id, &scale)));
    let mut batch = Vec::with_capacity(chunk);
    let mut after_commit_eval = Vec::new();
    let mut after_commit_view = Vec::new();
    let mut commit_ns = Vec::new();
    let mut view_step_ns = Vec::new();
    for record in records {
        batch.push(record);
        if batch.len() == chunk {
            time_commit(
                &mut store,
                &mut view,
                &mut batch,
                &mut commit_ns,
                &mut view_step_ns,
                &mut after_commit_eval,
                &mut after_commit_view,
            )?;
        }
    }
    if !batch.is_empty() {
        time_commit(
            &mut store,
            &mut view,
            &mut batch,
            &mut commit_ns,
            &mut view_step_ns,
            &mut after_commit_eval,
            &mut after_commit_view,
        )?;
    }

    let request = breakdown_request();
    let cold = Instant::now();
    let cold_groups = evaluate(&store, &request)?;
    let cold_eval_ns = elapsed_ns(cold);
    assert_groups_match(&cold_groups, &view)?;
    let warm_eval: Vec<u64> = (0..8)
        .map(|_| {
            let start = Instant::now();
            let _ = evaluate(&store, &request).expect("warm evaluate");
            elapsed_ns(start)
        })
        .collect();
    let warm_view: Vec<u64> = (0..8)
        .map(|_| {
            let start = Instant::now();
            let _ = view.answer();
            elapsed_ns(start)
        })
        .collect();

    let mut next = scale.shipments;
    let mut append_only = Vec::new();
    let mut view_only = Vec::new();
    let mut append_and_view = Vec::new();
    for _ in 0..100 {
        let record = shipment(next, &scale);
        next += 1;
        let change = shipment_change(std::slice::from_ref(&record));
        let t = Instant::now();
        let step_change = change.clone();
        view.ingest(step_change)?;
        view_only.push(elapsed_ns(t));
        let t = Instant::now();
        store.append(record)?;
        append_only.push(elapsed_ns(t));
        let record = shipment(next, &scale);
        next += 1;
        let change = shipment_change(std::slice::from_ref(&record));
        let t = Instant::now();
        store.append(record)?;
        view.ingest(change)?;
        append_and_view.push(elapsed_ns(t));
    }
    let extra_groups = evaluate(&store, &request)?;
    assert_groups_match(&extra_groups, &view)?;

    let rebuild = Instant::now();
    store.append(shipment_schema(2, &["amount", "weight"]))?;
    let rebuild_leaf_rollups_ns = elapsed_ns(rebuild);

    let ingest_rss = peak_rss_bytes();
    drop(store);
    let open = Instant::now();
    let store = Store::open(&log)?;
    let open_with_sidecar_ns = elapsed_ns(open);
    let replay = Instant::now();
    let mut replayed = View::new();
    let acl = PropertyAcl::allow_all();
    for id in 0..next {
        if id.is_multiple_of(100) {
            continue;
        }
        let key = format!("s{id}");
        let loaded = store.load(NS, "Shipment", &key, &acl)?;
        replayed.ingest(shipment_change(std::slice::from_ref(&loaded)))?;
    }
    let replay_from_log_ns = elapsed_ns(replay);
    let replay_groups = evaluate(&store, &request)?;
    assert_groups_match(&replay_groups, &replayed)?;

    let machine = machine_class();
    println!("machine_os={}", machine.0);
    println!("machine_arch={}", machine.1);
    println!("machine_cpus={}", machine.2);
    println!("machine_cpu={}", machine.3);
    println!("objects={objects}");
    println!(
        "customers={} orders={} shipments={}",
        scale.customers, scale.orders, scale.shipments
    );
    println!("groups={}", view.counts.len());
    println!("cold_evaluate_ns={cold_eval_ns}");
    println!("warm_evaluate_p50_ns={}", p50(&warm_eval));
    println!("warm_view_p50_ns={}", p50(&warm_view));
    println!("after_commit_evaluate_p50_ns={}", p50(&after_commit_eval));
    println!("after_commit_view_p50_ns={}", p50(&after_commit_view));
    println!("commit_p50_ns={}", p50(&commit_ns));
    println!("view_step_p50_ns={}", p50(&view_step_ns));
    println!("append_only_p50_ns={}", p50(&append_only));
    println!("view_only_p50_ns={}", p50(&view_only));
    println!("append_and_view_p50_ns={}", p50(&append_and_view));
    println!("rebuild_leaf_rollups_ns={rebuild_leaf_rollups_ns}");
    println!("open_with_sidecar_ns={open_with_sidecar_ns}");
    println!("replay_view_from_log_ns={replay_from_log_ns}");
    println!("peak_rss_bytes={ingest_rss}");
    println!("view_state_bytes={}", view.state_bytes());
    let added = percent(p50(&view_only), p50(&append_only));
    println!("view_step_pct_of_append={added:.2}");
    let speedup = p50(&warm_eval) as f64 / p50(&warm_view).max(1) as f64;
    println!("warm_speedup={speedup:.1}");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

fn time_commit(
    store: &mut Store,
    view: &mut View,
    batch: &mut Vec<ObjectRecord>,
    commit_ns: &mut Vec<u64>,
    view_step_ns: &mut Vec<u64>,
    after_commit_eval: &mut Vec<u64>,
    after_commit_view: &mut Vec<u64>,
) -> Result<(), String> {
    let change = shipment_change(batch);
    let t = Instant::now();
    view.ingest(change)?;
    view_step_ns.push(elapsed_ns(t));
    let t = Instant::now();
    store.append_batch(std::mem::take(batch))?;
    commit_ns.push(elapsed_ns(t));
    let request = breakdown_request();
    let t = Instant::now();
    let groups = evaluate(store, &request)?;
    after_commit_eval.push(elapsed_ns(t));
    let t = Instant::now();
    let _ = view.answer();
    after_commit_view.push(elapsed_ns(t));
    assert_groups_match(&groups, view)
}

struct View {
    count: Count<String, i64>,
    sum: Sum<String, i64>,
    counts: BTreeMap<String, i64>,
    sums: BTreeMap<String, i64>,
}

impl View {
    fn new() -> Self {
        Self {
            count: Count::new(),
            sum: Sum::new(),
            counts: BTreeMap::new(),
            sums: BTreeMap::new(),
        }
    }

    fn ingest(&mut self, change: ZSet<(String, i64)>) -> Result<(), String> {
        if change.is_empty() {
            return Ok(());
        }
        let counts = self
            .count
            .step(change.clone())
            .map_err(|err| err.to_string())?;
        let sums = self.sum.step(change).map_err(|err| err.to_string())?;
        fold_answer(&mut self.counts, &counts);
        fold_answer(&mut self.sums, &sums);
        Ok(())
    }

    fn answer(&self) -> Vec<(String, i64, i64)> {
        self.counts
            .iter()
            .map(|(key, count)| {
                let sum = self.sums.get(key).copied().unwrap_or(0);
                (key.clone(), *count, sum)
            })
            .collect()
    }

    fn state_bytes(&self) -> usize {
        self.counts
            .iter()
            .map(|(key, _)| key.len() + 16)
            .sum::<usize>()
            + self
                .sums
                .iter()
                .map(|(key, _)| key.len() + 16)
                .sum::<usize>()
    }
}

fn fold_answer(map: &mut BTreeMap<String, i64>, delta: &ZSet<(String, i64)>) {
    for ((key, value), weight) in delta.iter() {
        if weight < 0 {
            if map.get(key) == Some(value) {
                map.remove(key);
            }
        } else if weight > 0 {
            map.insert(key.clone(), *value);
        }
    }
}

fn shipment_change(records: &[ObjectRecord]) -> ZSet<(String, i64)> {
    let mut change = ZSet::new();
    for record in records {
        if record.kind != "Shipment" || record.hidden {
            continue;
        }
        let Some(weight) = record.props.get("weight").filter(|value| !value.is_empty()) else {
            continue;
        };
        let Ok(amount) = record
            .props
            .get("amount")
            .ok_or(())
            .and_then(|value| value.parse::<i64>().map_err(|_| ()))
        else {
            continue;
        };
        change.add_row((weight.clone(), amount), 1);
    }
    change
}

fn evaluate(store: &Store, request: &EvaluateRequest) -> Result<Vec<mikura::Group>, String> {
    LocalCompute
        .evaluate(store, request)
        .map(|response| response.groups)
        .map_err(|err| format!("{err:?}"))
}

fn breakdown_request() -> EvaluateRequest {
    EvaluateRequest {
        namespace: NS.into(),
        root_kind: "Shipment".into(),
        hops: Vec::new(),
        sum_kind: "Shipment".into(),
        sum_property: "amount".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        breakdown: Some(Breakdown {
            group_by: vec!["weight".into()],
            max_groups: 20,
            distinct: None,
        }),
        ..EvaluateRequest::default()
    }
}

fn assert_groups_match(groups: &[mikura::Group], view: &View) -> Result<(), String> {
    let answer = view.answer();
    if groups.len() != answer.len() {
        return Err(format!(
            "group count {} != view {}",
            groups.len(),
            answer.len()
        ));
    }
    for (group, (key, count, sum)) in groups.iter().zip(answer) {
        let got = group.key.first().and_then(|part| part.as_deref());
        if got != Some(key.as_str()) {
            return Err(format!("group key {got:?} != view {key}"));
        }
        if group.count as i64 != count {
            return Err(format!("group {key} count {} != view {count}", group.count));
        }
        if group.sum != sum {
            return Err(format!("group {key} sum {} != view {sum}", group.sum));
        }
    }
    Ok(())
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

fn shipment_schema(version: u32, sums: &[&str]) -> ObjectRecord {
    SchemaDescriptor {
        kind: "Shipment".into(),
        properties: vec!["order_id".into(), "amount".into(), "weight".into()],
        links: vec![SchemaLink {
            name: "order_id".into(),
            far_kind: "Order".into(),
            outgoing: true,
        }],
        sums: sums.iter().map(|name| (*name).to_string()).collect(),
        version,
        ..SchemaDescriptor::default()
    }
    .to_record(NS)
    .expect("shipment schema")
}

fn temp_dir(label: &str, objects: u64) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "mikura-maintained-view-{label}-{objects}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn arg_u64(name: &str, default: u64) -> u64 {
    let args: Vec<String> = std::env::args().collect();
    args.windows(2)
        .find(|pair| pair[0] == name)
        .and_then(|pair| pair[1].parse().ok())
        .unwrap_or(default)
}

fn elapsed_ns(start: Instant) -> u64 {
    start.elapsed().as_nanos() as u64
}

fn p50(values: &[u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted[(sorted.len() - 1) / 2]
}

fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    (part as f64) * 100.0 / (whole as f64)
}

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
    0
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
                .map(|rest| rest.trim_start_matches(':').trim().to_string())
        })
        .unwrap_or_else(|| "unknown".into());
    (os, arch, cpus, cpu)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_matches_evaluate_on_product_fixture() {
        let objects = 1_000;
        let scale = Scale::new(objects);
        let dir = temp_dir("test", objects);
        let log = dir.join("objects.mikura");
        let mut store = Store::create(&log).unwrap();
        store.append(shipment_schema(1, &["amount"])).unwrap();
        let mut view = View::new();
        let mut batch: Vec<_> = (0..scale.customers)
            .map(customer)
            .chain((0..scale.orders).map(|id| order(id, &scale)))
            .chain((0..scale.shipments).map(|id| shipment(id, &scale)))
            .collect();
        view.ingest(shipment_change(&batch)).unwrap();
        store.append_batch(std::mem::take(&mut batch)).unwrap();
        let groups = LocalCompute
            .evaluate(&store, &breakdown_request())
            .unwrap()
            .groups;
        assert_groups_match(&groups, &view).unwrap();
        assert_eq!(groups.len(), 7);
        let extra = shipment(scale.shipments, &scale);
        view.ingest(shipment_change(std::slice::from_ref(&extra)))
            .unwrap();
        store.append(extra).unwrap();
        let groups = LocalCompute
            .evaluate(&store, &breakdown_request())
            .unwrap()
            .groups;
        assert_groups_match(&groups, &view).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
