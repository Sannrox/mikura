//! Throwaway: does the product-loop Explorer workload need prefix or
//! contains, and does a token projection earn its place?
//!
//! Not the store of record. Token lists live in this crate only.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use mikura::{
    Aggregate, ComputeBackend, EvaluateRequest, Hop, LocalCompute, ObjectRecord, Predicate,
    PropertyAcl, PropertyType, SchemaDescriptor, Store,
};

const NS: &str = "bench";
const COMPONENT: &str = "component";
const INCIDENT: &str = "incident";
const SAMPLES: usize = 5;
const BUDGET_MS: f64 = 500.0;
const PREFIX: &str = "billing";
const CONTAINS: &str = "api";
const TOKEN: &str = "api";

fn main() {
    let objects = arg_u64("--objects", 10_000);
    if objects < 32 {
        eprintln!("--objects must be at least 32");
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
    let (os, arch, cpus, cpu) = machine_class();
    println!("machine={os}/{arch} cpus={cpus} cpu={cpu}");
    println!("budget_ms={BUDGET_MS} objects={objects}");

    let dir = temp_dir("run", objects);
    let log = dir.join("objects.mikura");
    let mut store = Store::create(&log)?;
    store.append(component_schema().to_record(NS)?)?;
    store.append(incident_schema().to_record(NS)?)?;

    let mut rows = Vec::with_capacity(objects as usize);
    let chunk = (objects / 20).max(100) as usize;
    let mut batch = Vec::with_capacity(chunk);
    let t_ingest = Instant::now();
    for id in 0..objects {
        let row = sample_component(id);
        if !row.hidden {
            rows.push(DecodedName {
                key: row.record.key.clone(),
                name: row.name.clone(),
            });
        }
        batch.push(row.record);
        batch.push(sample_incident(id).record);
        if batch.len() >= chunk {
            store.append_batch(std::mem::take(&mut batch))?;
        }
    }
    if !batch.is_empty() {
        store.append_batch(batch)?;
    }
    let ingest_ms = t_ingest.elapsed().as_secs_f64() * 1000.0;
    let sidecar_bytes = file_len(&sidecar_path(&log));

    let t_build = Instant::now();
    let tokens = TokenIndex::build(&rows);
    let ordered = ordered_names(&rows);
    let build_ms = t_build.elapsed().as_secs_f64() * 1000.0;

    let eq_request = list_request(COMPONENT, 0, Some(Predicate::eq("tier", "prod")));
    let hop_request = hop_request(0);
    let _ = LocalCompute
        .evaluate(&store, &eq_request)
        .map_err(|err| format!("{err:?}"))?;
    let _ = LocalCompute
        .evaluate(&store, &hop_request)
        .map_err(|err| format!("{err:?}"))?;

    let mut eq_ns = Vec::with_capacity(SAMPLES);
    let mut hop_ns = Vec::with_capacity(SAMPLES);
    let mut prefix_scan_ns = Vec::with_capacity(SAMPLES);
    let mut prefix_btree_ns = Vec::with_capacity(SAMPLES);
    let mut contains_ns = Vec::with_capacity(SAMPLES);
    let mut token_ns = Vec::with_capacity(SAMPLES);
    let mut eq_count = 0usize;
    for _ in 0..SAMPLES {
        let t = Instant::now();
        let response = LocalCompute
            .evaluate(&store, &eq_request)
            .map_err(|err| format!("{err:?}"))?;
        eq_ns.push(elapsed_ns(t));
        eq_count = response.two_hop_count as usize;

        let t = Instant::now();
        std::hint::black_box(
            LocalCompute
                .evaluate(&store, &hop_request)
                .map_err(|err| format!("{err:?}"))?,
        );
        hop_ns.push(elapsed_ns(t));

        let t = Instant::now();
        std::hint::black_box(prefix_count(&rows, PREFIX));
        prefix_scan_ns.push(elapsed_ns(t));

        let t = Instant::now();
        std::hint::black_box(prefix_btree_count(&ordered, PREFIX));
        prefix_btree_ns.push(elapsed_ns(t));

        let t = Instant::now();
        let contains = contains_keys(&rows, CONTAINS);
        contains_ns.push(elapsed_ns(t));

        let t = Instant::now();
        let token = tokens.query(TOKEN);
        token_ns.push(elapsed_ns(t));

        if token != contains {
            return Err(format!(
                "dual-read miss token={token:?} contains={contains:?}"
            ));
        }
    }
    if eq_count == 0 {
        return Err("eq tier=prod returned no rows".into());
    }
    println!(
        "eq_p50_ms={:.3} eq={} hop_p50_ms={:.3} hop={} prefix_scan_p50_ms={:.3} prefix_scan={} prefix_btree_p50_ms={:.3} prefix_btree={} contains_p50_ms={:.3} contains={} token_p50_ms={:.3} token={} eq_hits={} prefix_hits={} contains_hits={} build_ms={build_ms:.1} ingest_ms={ingest_ms:.1} token_bytes={} sidecar_bytes={sidecar_bytes} peak_rss_bytes={}",
        p50(&eq_ns) as f64 / 1_000_000.0,
        hold_or_miss(p50(&eq_ns) as f64 / 1_000_000.0),
        p50(&hop_ns) as f64 / 1_000_000.0,
        hold_or_miss(p50(&hop_ns) as f64 / 1_000_000.0),
        p50(&prefix_scan_ns) as f64 / 1_000_000.0,
        hold_or_miss(p50(&prefix_scan_ns) as f64 / 1_000_000.0),
        p50(&prefix_btree_ns) as f64 / 1_000_000.0,
        hold_or_miss(p50(&prefix_btree_ns) as f64 / 1_000_000.0),
        p50(&contains_ns) as f64 / 1_000_000.0,
        hold_or_miss(p50(&contains_ns) as f64 / 1_000_000.0),
        p50(&token_ns) as f64 / 1_000_000.0,
        hold_or_miss(p50(&token_ns) as f64 / 1_000_000.0),
        eq_count,
        prefix_count(&rows, PREFIX),
        contains_keys(&rows, CONTAINS).len(),
        tokens.bytes(),
        peak_rss_bytes(),
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

fn hold_or_miss(ms: f64) -> &'static str {
    if ms <= BUDGET_MS {
        "HOLD"
    } else {
        "MISS"
    }
}

struct SampleRow {
    record: ObjectRecord,
    hidden: bool,
    name: String,
}

struct DecodedName {
    key: String,
    name: String,
}

fn component_schema() -> SchemaDescriptor {
    SchemaDescriptor {
        kind: COMPONENT.into(),
        properties: vec!["n".into(), "name".into(), "tier".into()],
        required: vec!["n".into(), "name".into(), "tier".into()],
        types: vec![
            ("n".into(), PropertyType::Integer),
            ("name".into(), PropertyType::String),
            ("tier".into(), PropertyType::String),
        ],
        indexes: vec!["tier".into()],
        ..SchemaDescriptor::default()
    }
}

fn incident_schema() -> SchemaDescriptor {
    SchemaDescriptor {
        kind: INCIDENT.into(),
        properties: vec!["affects".into(), "n".into(), "name".into()],
        required: vec!["affects".into(), "n".into(), "name".into()],
        types: vec![
            ("affects".into(), PropertyType::String),
            ("n".into(), PropertyType::Integer),
            ("name".into(), PropertyType::String),
        ],
        ..SchemaDescriptor::default()
    }
}

fn sample_component(id: u64) -> SampleRow {
    let hidden = id % 100 == 99;
    let key = if id == 0 {
        "svc-api".to_string()
    } else {
        format!("svc-{id}")
    };
    let name = if id == 0 {
        "billing-api".to_string()
    } else if id % 100 == 0 {
        format!("billing-api-{id}")
    } else {
        format!("svc-{id}")
    };
    let tier = if id % 10 == 0 { "prod" } else { "staging" };
    let record = ObjectRecord {
        gen: 1,
        namespace: NS.into(),
        kind: COMPONENT.into(),
        key,
        hidden,
        action_id: None,
        props: HashMap::from([
            ("n".into(), "1".into()),
            ("name".into(), name.clone()),
            ("tier".into(), tier.into()),
        ]),
        schema_version: 1,
    };
    SampleRow {
        record,
        hidden,
        name,
    }
}

fn sample_incident(id: u64) -> SampleRow {
    let hidden = id % 100 == 99;
    let key = if id == 1 {
        "inc-1".to_string()
    } else {
        format!("inc-{id}")
    };
    let name = if id == 1 {
        "elevated latency".to_string()
    } else {
        format!("event-{id}")
    };
    let affects = if id == 1 {
        "svc-api".to_string()
    } else {
        format!("svc-{id}")
    };
    let record = ObjectRecord {
        gen: 1,
        namespace: NS.into(),
        kind: INCIDENT.into(),
        key,
        hidden,
        action_id: None,
        props: HashMap::from([
            ("affects".into(), affects),
            ("n".into(), "1".into()),
            ("name".into(), name.clone()),
        ]),
        schema_version: 1,
    };
    SampleRow {
        record,
        hidden,
        name,
    }
}

fn list_request(kind: &str, object_bound: usize, predicate: Option<Predicate>) -> EvaluateRequest {
    EvaluateRequest {
        namespace: NS.into(),
        root_kind: kind.into(),
        hops: vec![],
        sum_kind: kind.into(),
        sum_property: "n".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate,
        object_bound,
        sort: None,
        page_size: 0,
        cursor: None,
        breakdown: None,
        then_by: Vec::new(),
    }
}

fn hop_request(object_bound: usize) -> EvaluateRequest {
    EvaluateRequest {
        namespace: NS.into(),
        root_kind: INCIDENT.into(),
        hops: vec![Hop {
            far_kind: COMPONENT.into(),
            join_property: "affects".into(),
            incoming: true,
            predicate: None,
        }],
        sum_kind: COMPONENT.into(),
        sum_property: "n".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: None,
        object_bound,
        sort: None,
        page_size: 0,
        cursor: None,
        breakdown: None,
        then_by: Vec::new(),
    }
}

fn tokenize(text: &str) -> Vec<&str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|token| !token.is_empty())
        .collect()
}

fn prefix_count(rows: &[DecodedName], prefix: &str) -> usize {
    rows.iter()
        .filter(|row| row.name.starts_with(prefix))
        .count()
}

fn prefix_btree_count(ordered: &BTreeMap<String, Vec<String>>, prefix: &str) -> usize {
    ordered
        .range(prefix.to_string()..)
        .take_while(|(name, _)| name.starts_with(prefix))
        .map(|(_, keys)| keys.len())
        .sum()
}

fn contains_keys(rows: &[DecodedName], needle: &str) -> Vec<String> {
    let mut keys: Vec<String> = rows
        .iter()
        .filter(|row| row.name.contains(needle))
        .map(|row| row.key.clone())
        .collect();
    keys.sort();
    keys
}

fn ordered_names(rows: &[DecodedName]) -> BTreeMap<String, Vec<String>> {
    let mut map = BTreeMap::new();
    for row in rows {
        map.entry(row.name.clone())
            .or_insert_with(Vec::new)
            .push(row.key.clone());
    }
    map
}

struct TokenIndex {
    postings: HashMap<String, Vec<String>>,
}

impl TokenIndex {
    fn build(rows: &[DecodedName]) -> Self {
        let mut postings: HashMap<String, Vec<String>> = HashMap::new();
        for row in rows {
            let mut seen = HashMap::new();
            for token in tokenize(&row.name) {
                if seen.insert(token, ()).is_none() {
                    postings
                        .entry(token.to_string())
                        .or_default()
                        .push(row.key.clone());
                }
            }
        }
        for list in postings.values_mut() {
            list.sort();
        }
        Self { postings }
    }

    fn query(&self, token: &str) -> Vec<String> {
        self.postings.get(token).cloned().unwrap_or_default()
    }

    fn bytes(&self) -> usize {
        self.postings
            .iter()
            .map(|(term, list)| term.len() + list.iter().map(|key| key.len()).sum::<usize>())
            .sum()
    }
}

fn temp_dir(label: &str, objects: u64) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "mikura-explorer-text-{label}-{objects}-{}-{nanos}",
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

fn sidecar_path(log: &Path) -> PathBuf {
    let mut name = log.as_os_str().to_os_string();
    name.push(".joins");
    PathBuf::from(name)
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
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

    fn seed_store() -> (Store, PathBuf) {
        let dir = temp_dir("seed", 2);
        let log = dir.join("objects.mikura");
        let mut store = Store::create(&log).unwrap();
        store
            .append(component_schema().to_record(NS).unwrap())
            .unwrap();
        store
            .append(incident_schema().to_record(NS).unwrap())
            .unwrap();
        store.append(sample_component(0).record).unwrap();
        store.append(sample_incident(1).record).unwrap();
        (store, dir)
    }

    #[test]
    fn product_loop_eq_tier_matches_seed() {
        let (store, dir) = seed_store();
        let request = list_request(COMPONENT, 8, Some(Predicate::eq("tier", "prod")));
        let keys: Vec<String> = LocalCompute
            .evaluate(&store, &request)
            .unwrap()
            .objects
            .into_iter()
            .map(|row| row.key)
            .collect();
        assert_eq!(keys, vec!["svc-api".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn product_loop_hop_matches_seed() {
        let (store, dir) = seed_store();
        let keys: Vec<String> = LocalCompute
            .evaluate(&store, &hop_request(8))
            .unwrap()
            .objects
            .into_iter()
            .map(|row| row.key)
            .collect();
        assert_eq!(keys, vec!["svc-api".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn seed_prefix_and_contains_are_unrequired_answers() {
        let rows = vec![DecodedName {
            key: "svc-api".into(),
            name: "billing-api".into(),
        }];
        assert_eq!(prefix_count(&rows, PREFIX), 1);
        assert_eq!(contains_keys(&rows, CONTAINS), vec!["svc-api".to_string()]);
    }

    #[test]
    fn token_index_matches_contains_scan() {
        let n = 200;
        let rows: Vec<DecodedName> = (0..n)
            .filter_map(|id| {
                let row = sample_component(id);
                (!row.hidden).then_some(DecodedName {
                    key: row.record.key,
                    name: row.name,
                })
            })
            .collect();
        let index = TokenIndex::build(&rows);
        assert_eq!(index.query(TOKEN), contains_keys(&rows, CONTAINS));
        assert!(!index.query(TOKEN).is_empty());
    }
}
