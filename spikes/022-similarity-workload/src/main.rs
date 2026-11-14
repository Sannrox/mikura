//! Throwaway: does the product-loop name a similarity-retrieval workload
//! over stored vectors, and what exact-scan envelope would serve one?
//!
//! Not the store of record. Embeddings are unpublished study properties.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use mikura::{
    Aggregate, ComputeBackend, EvaluateRequest, Hop, LocalCompute, ObjectRecord, Predicate,
    PropertyAcl, PropertyType, SchemaDescriptor, Sort, Store,
};

const NS: &str = "bench";
const COMPONENT: &str = "component";
const INCIDENT: &str = "incident";
const SAMPLES: usize = 5;
const BUDGET_MS: f64 = 500.0;
const PUBLISHED_QUERIES: usize = 3;
const PUBLISHED_SIMILARITY_QUERIES: usize = 0;

fn main() {
    let objects = arg_u64("--objects", 10_000);
    let dim = arg_u64("--dim", 64) as u16;
    let k = arg_u64("--k", 10) as usize;
    if objects < 32 {
        eprintln!("--objects must be at least 32");
        std::process::exit(2);
    }
    if !(1..=mikura::VECTOR_DIM_MAX).contains(&dim) {
        eprintln!("--dim must be in 1..={}", mikura::VECTOR_DIM_MAX);
        std::process::exit(2);
    }
    if k == 0 || k as u64 > objects {
        eprintln!("--k must be in 1..=objects");
        std::process::exit(2);
    }
    match run(objects, dim, k) {
        Ok(()) => {}
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

fn run(objects: u64, dim: u16, k: usize) -> Result<(), String> {
    let (os, arch, cpus, cpu) = machine_class();
    println!("machine={os}/{arch} cpus={cpus} cpu={cpu}");
    println!(
        "budget_ms={BUDGET_MS} objects={objects} dim={dim} k={k} published_queries={PUBLISHED_QUERIES} similarity_queries={PUBLISHED_SIMILARITY_QUERIES}"
    );

    let dir = temp_dir("run", objects);
    let log = dir.join("objects.mikura");
    let mut store = Store::create(&log)?;
    store.append(component_schema(dim).to_record(NS)?)?;
    store.append(incident_schema().to_record(NS)?)?;

    let mut rows = Vec::with_capacity(objects as usize);
    let chunk = (objects / 20).max(100) as usize;
    let mut batch = Vec::with_capacity(chunk);
    let t_ingest = Instant::now();
    for id in 0..objects {
        let (record, vector) = sample_component(id, dim);
        if !record.hidden {
            rows.push(DecodedRow {
                key: record.key.clone(),
                vector,
            });
        }
        batch.push(record);
        batch.push(sample_incident(id));
        if batch.len() >= chunk {
            store.append_batch(std::mem::take(&mut batch))?;
        }
    }
    if !batch.is_empty() {
        store.append_batch(batch)?;
    }
    let ingest_ms = t_ingest.elapsed().as_secs_f64() * 1000.0;

    let query_idx = rows
        .iter()
        .position(|row| row.key == "svc-api")
        .ok_or_else(|| "missing seed component svc-api".to_string())?;
    let query = rows[query_idx].vector.clone();
    let query_canon = encode_vector(&query);

    let eq_req = list_request(0, Some(Predicate::eq("tier", "prod")));
    let hop_req = hop_request(0);
    let nearest_req = nearest_request(&query_canon, rows.len().max(objects as usize), k);

    let _ = LocalCompute
        .evaluate(&store, &eq_req)
        .map_err(|err| format!("{err:?}"))?;
    let _ = LocalCompute
        .evaluate(&store, &nearest_req)
        .map_err(|err| format!("{err:?}"))?;

    let mut eq_ns = Vec::with_capacity(SAMPLES);
    let mut hop_ns = Vec::with_capacity(SAMPLES);
    let mut eval_ns = Vec::with_capacity(SAMPLES);
    let mut decoded_ns = Vec::with_capacity(SAMPLES);
    let mut eval_keys = Vec::new();
    for _ in 0..SAMPLES {
        let t = Instant::now();
        std::hint::black_box(
            LocalCompute
                .evaluate(&store, &eq_req)
                .map_err(|err| format!("{err:?}"))?,
        );
        eq_ns.push(elapsed_ns(t));

        let t = Instant::now();
        std::hint::black_box(
            LocalCompute
                .evaluate(&store, &hop_req)
                .map_err(|err| format!("{err:?}"))?,
        );
        hop_ns.push(elapsed_ns(t));

        let t = Instant::now();
        let response = LocalCompute
            .evaluate(&store, &nearest_req)
            .map_err(|err| format!("{err:?}"))?;
        eval_ns.push(elapsed_ns(t));
        eval_keys = response.objects.iter().map(|row| row.key.clone()).collect();

        let t = Instant::now();
        let decoded = decoded_topk(&rows, &query, k);
        decoded_ns.push(elapsed_ns(t));
        if decoded != eval_keys {
            return Err(format!(
                "dual-read miss evaluate={eval_keys:?} decoded={decoded:?}"
            ));
        }
    }
    if eval_keys.len() != k {
        return Err(format!("nearest page {} != k {k}", eval_keys.len()));
    }
    println!(
        "eq_p50_ms={:.3} eq={} hop_p50_ms={:.3} hop={} evaluate_p50_ms={:.3} evaluate={} decoded_p50_ms={:.3} decoded={} ingest_ms={ingest_ms:.1} peak_rss_bytes={}",
        p50(&eq_ns) as f64 / 1_000_000.0,
        hold_or_miss(p50(&eq_ns) as f64 / 1_000_000.0),
        p50(&hop_ns) as f64 / 1_000_000.0,
        hold_or_miss(p50(&hop_ns) as f64 / 1_000_000.0),
        p50(&eval_ns) as f64 / 1_000_000.0,
        hold_or_miss(p50(&eval_ns) as f64 / 1_000_000.0),
        p50(&decoded_ns) as f64 / 1_000_000.0,
        hold_or_miss(p50(&decoded_ns) as f64 / 1_000_000.0),
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

struct DecodedRow {
    key: String,
    vector: Vec<f64>,
}

fn component_schema(dim: u16) -> SchemaDescriptor {
    SchemaDescriptor {
        kind: COMPONENT.into(),
        properties: vec!["embedding".into(), "n".into(), "name".into(), "tier".into()],
        required: vec!["embedding".into(), "n".into(), "name".into(), "tier".into()],
        types: vec![
            ("embedding".into(), PropertyType::Vector { dim }),
            ("n".into(), PropertyType::Integer),
            ("name".into(), PropertyType::String),
            ("tier".into(), PropertyType::String),
        ],
        indexes: vec!["tier".into(), "name".into()],
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

fn sample_component(id: u64, dim: u16) -> (ObjectRecord, Vec<f64>) {
    let hidden = id % 100 == 99;
    let key = if id == 0 {
        "svc-api".to_string()
    } else {
        format!("svc-{id}")
    };
    let name = if id == 0 {
        "billing-api".to_string()
    } else {
        format!("svc-{id}")
    };
    let tier = if id % 10 == 0 { "prod" } else { "staging" };
    let vector = seeded_vector(id, dim as usize);
    let record = ObjectRecord {
        gen: 1,
        namespace: NS.into(),
        kind: COMPONENT.into(),
        key,
        hidden,
        action_id: None,
        props: HashMap::from([
            ("embedding".into(), encode_vector(&vector)),
            ("n".into(), "1".into()),
            ("name".into(), name),
            ("tier".into(), tier.into()),
        ]),
        schema_version: 1,
    };
    (record, vector)
}

fn sample_incident(id: u64) -> ObjectRecord {
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
    ObjectRecord {
        gen: 1,
        namespace: NS.into(),
        kind: INCIDENT.into(),
        key,
        hidden,
        action_id: None,
        props: HashMap::from([
            ("affects".into(), affects),
            ("n".into(), "1".into()),
            ("name".into(), name),
        ]),
        schema_version: 1,
    }
}

fn list_request(object_bound: usize, predicate: Option<Predicate>) -> EvaluateRequest {
    EvaluateRequest {
        namespace: NS.into(),
        root_kind: COMPONENT.into(),
        hops: vec![],
        sum_kind: COMPONENT.into(),
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

fn nearest_request(query: &str, object_bound: usize, page_size: usize) -> EvaluateRequest {
    EvaluateRequest {
        namespace: NS.into(),
        root_kind: COMPONENT.into(),
        hops: vec![],
        sum_kind: COMPONENT.into(),
        sum_property: "n".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: None,
        object_bound,
        sort: Some(Sort::nearest("embedding", query)),
        page_size,
        cursor: None,
        breakdown: None,
        then_by: Vec::new(),
    }
}

fn seeded_vector(id: u64, dim: usize) -> Vec<f64> {
    const CLUSTERS: u64 = 32;
    let cluster = id % CLUSTERS;
    let mut centroid_state = cluster.wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut noise_state = id.wrapping_add(1).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let mut vector = Vec::with_capacity(dim);
    for _ in 0..dim {
        centroid_state = centroid_state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1);
        noise_state = noise_state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1);
        let unit = ((centroid_state >> 33) as f64) / (u32::MAX as f64);
        let noise = ((noise_state >> 33) as f64) / (u32::MAX as f64);
        let raw = (unit * 2.0 - 1.0) + (noise * 0.08 - 0.04);
        let canonical = canonical_number(raw.clamp(-1.0, 1.0));
        vector.push(canonical.parse().expect("canonical number"));
    }
    vector
}

fn encode_vector(vector: &[f64]) -> String {
    let mut out = String::with_capacity(vector.len() * 6);
    for (i, component) in vector.iter().copied().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&canonical_number(component));
    }
    out
}

fn canonical_number(value: f64) -> String {
    if !value.is_finite() {
        return "0".into();
    }
    let scaled = (value * 100.0).round() / 100.0;
    if scaled == 0.0 {
        return "0".into();
    }
    let sign = if scaled < 0.0 { "-" } else { "" };
    let abs = scaled.abs();
    let int = abs.trunc() as i64;
    let frac = ((abs - abs.trunc()) * 100.0).round() as i64;
    if frac == 0 {
        format!("{sign}{int}")
    } else if frac % 10 == 0 {
        format!("{sign}{int}.{}", frac / 10)
    } else {
        format!("{sign}{int}.{frac:02}")
    }
}

fn squared_l2(left: &[f64], right: &[f64]) -> f64 {
    left.iter()
        .zip(right)
        .map(|(a, b)| {
            let d = a - b;
            d * d
        })
        .sum()
}

fn decoded_topk(rows: &[DecodedRow], query: &[f64], k: usize) -> Vec<String> {
    let mut scored: Vec<(f64, &str)> = rows
        .iter()
        .map(|row| (squared_l2(&row.vector, query), row.key.as_str()))
        .collect();
    scored.sort_by(|a, b| cmp_score(a.0, a.1, b.0, b.1));
    scored.truncate(k);
    scored.into_iter().map(|(_, key)| key.to_string()).collect()
}

fn cmp_score(ld: f64, lk: &str, rd: f64, rk: &str) -> Ordering {
    ld.partial_cmp(&rd)
        .unwrap_or(Ordering::Equal)
        .then_with(|| lk.cmp(rk))
}

#[cfg(test)]
fn eval_keys(store: &Store, request: &EvaluateRequest) -> Result<Vec<String>, String> {
    let mut keys: Vec<String> = LocalCompute
        .evaluate(store, request)
        .map_err(|err| format!("{err:?}"))?
        .objects
        .into_iter()
        .map(|row| row.key)
        .collect();
    keys.sort();
    Ok(keys)
}

#[cfg(test)]
fn seed_store(dim: u16) -> (Store, PathBuf) {
    let dir = temp_dir("seed", 2);
    let log = dir.join("objects.mikura");
    let mut store = Store::create(&log).unwrap();
    store
        .append(component_schema(dim).to_record(NS).unwrap())
        .unwrap();
    store
        .append(incident_schema().to_record(NS).unwrap())
        .unwrap();
    store.append(sample_component(0, dim).0).unwrap();
    store.append(sample_incident(1)).unwrap();
    (store, dir)
}

fn temp_dir(label: &str, objects: u64) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "mikura-similarity-workload-{label}-{objects}-{}-{nanos}",
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
    fn published_eq_and_hop_match_seed() {
        let (store, dir) = seed_store(2);
        let eq = eval_keys(
            &store,
            &list_request(8, Some(Predicate::eq("tier", "prod"))),
        )
        .unwrap();
        assert_eq!(eq, vec!["svc-api".to_string()]);
        let hop = eval_keys(&store, &hop_request(8)).unwrap();
        assert_eq!(hop, vec!["svc-api".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nearest_keys_match_decoded() {
        let n = 64u64;
        let dim = 2u16;
        let k = 8usize;
        let dir = temp_dir("nearest", n);
        let log = dir.join("objects.mikura");
        let mut store = Store::create(&log).unwrap();
        store
            .append(component_schema(dim).to_record(NS).unwrap())
            .unwrap();
        let mut rows = Vec::new();
        let mut batch = Vec::new();
        for id in 0..n {
            let (record, vector) = sample_component(id, dim);
            if !record.hidden {
                rows.push(DecodedRow {
                    key: record.key.clone(),
                    vector,
                });
            }
            batch.push(record);
        }
        store.append_batch(batch).unwrap();
        let query = rows
            .iter()
            .find(|row| row.key == "svc-api")
            .unwrap()
            .vector
            .clone();
        let got = LocalCompute
            .evaluate(
                &store,
                &nearest_request(&encode_vector(&query), rows.len(), k),
            )
            .unwrap()
            .objects
            .into_iter()
            .map(|row| row.key)
            .collect::<Vec<_>>();
        assert_eq!(got, decoded_topk(&rows, &query, k));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn published_similarity_query_count_is_zero() {
        assert_eq!(PUBLISHED_SIMILARITY_QUERIES, 0);
        assert_eq!(PUBLISHED_QUERIES, 3);
    }
}
