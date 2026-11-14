//! Throwaway: at what vector count and dimension does evaluate's
//! brute-force `Sort.nearest` miss a 500 ms synthetic bar, and does a
//! clustered projection earn its place?
//!
//! Not the store of record. The clustered index lives in this crate only.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use mikura::{
    Aggregate, ComputeBackend, EvaluateRequest, LocalCompute, ObjectRecord, Predicate, PropertyAcl,
    PropertyType, SchemaDescriptor, Sort, Store,
};

const NS: &str = "bench";
const KIND: &str = "sample";
const KMEANS_ITERS: usize = 5;
const NPROBE: usize = 8;
const SAMPLES: usize = 5;
const BUDGET_MS: f64 = 500.0;

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
    println!("budget_ms={BUDGET_MS} k={k} dim={dim} objects={objects}");

    let dir = temp_dir("run", objects);
    let log = dir.join("objects.mikura");
    let mut store = Store::create(&log)?;
    store.append(sample_schema(dim).to_record(NS)?)?;

    let mut rows = Vec::with_capacity(objects as usize);
    let chunk = (objects / 20).max(100) as usize;
    let mut batch = Vec::with_capacity(chunk);
    let t_ingest = Instant::now();
    for id in 0..objects {
        let row = sample_row(id, dim);
        if !row.hidden {
            rows.push(DecodedRow {
                key: row.record.key.clone(),
                bucket: row.bucket,
                vector: row.vector,
            });
        }
        batch.push(row.record);
        if batch.len() == chunk {
            store.append_batch(std::mem::take(&mut batch))?;
        }
    }
    if !batch.is_empty() {
        store.append_batch(batch)?;
    }
    let ingest_ms = t_ingest.elapsed().as_secs_f64() * 1000.0;
    let ingest_rss = peak_rss_bytes();

    let query_idx = rows
        .iter()
        .position(|row| row.key == "k7")
        .or_else(|| rows.iter().position(|row| row.bucket != 0))
        .ok_or_else(|| "no visible query row".to_string())?;
    let query = rows[query_idx].vector.clone();
    let query_canon = encode_vector(&query);

    let t_build = Instant::now();
    let index = ClusteredIndex::build(&rows, dim as usize, 0.01)?;
    let build_ms = t_build.elapsed().as_secs_f64() * 1000.0;

    for filter_pct in [0u8, 1] {
        let predicate = (filter_pct > 0).then(|| Predicate::eq("bucket", "0"));
        let request = nearest_request(&query_canon, rows.len().max(objects as usize), k, predicate);
        let _ = LocalCompute
            .evaluate(&store, &request)
            .map_err(|err| format!("{err:?}"))?;
        let mut eval_ns = Vec::with_capacity(SAMPLES);
        let mut decoded_ns = Vec::with_capacity(SAMPLES);
        let mut ivf_ns = Vec::with_capacity(SAMPLES);
        let mut eval_keys = Vec::new();
        for _ in 0..SAMPLES {
            let t = Instant::now();
            let response = LocalCompute
                .evaluate(&store, &request)
                .map_err(|err| format!("{err:?}"))?;
            eval_ns.push(elapsed_ns(t));
            eval_keys = response
                .objects
                .iter()
                .map(|row| row.key.clone())
                .collect();
            let t = Instant::now();
            let decoded = decoded_topk(&rows, &query, k, filter_pct);
            decoded_ns.push(elapsed_ns(t));
            let t = Instant::now();
            let _ = index.query(&query, k, filter_pct);
            ivf_ns.push(elapsed_ns(t));
            if decoded != eval_keys {
                return Err(format!(
                    "dual-read miss filter_pct={filter_pct}: evaluate={eval_keys:?} decoded={decoded:?}"
                ));
            }
        }
        let ivf_keys = index.query(&query, k, filter_pct);
        let recall = recall_at_k(&eval_keys, &ivf_keys, k);
        let eval_ms = p50(&eval_ns) as f64 / 1_000_000.0;
        let decoded_ms = p50(&decoded_ns) as f64 / 1_000_000.0;
        let ivf_ms = p50(&ivf_ns) as f64 / 1_000_000.0;
        println!(
            "filter_pct={filter_pct} evaluate_p50_ms={eval_ms:.3} evaluate={} decoded_p50_ms={decoded_ms:.3} decoded={} ivf_p50_ms={ivf_ms:.3} ivf={} recall@{k}={recall:.3} build_ms={build_ms:.1} ingest_ms={ingest_ms:.1} nlist={} nprobe={} tail={} index_bytes={} peak_rss_bytes={ingest_rss}",
            hold_or_miss(eval_ms),
            hold_or_miss(decoded_ms),
            hold_or_miss(ivf_ms),
            index.nlist,
            index.nprobe,
            index.tail.len(),
            index.bytes(),
        );
    }
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
    bucket: u8,
    vector: Vec<f64>,
}

struct DecodedRow {
    key: String,
    bucket: u8,
    vector: Vec<f64>,
}

fn sample_schema(dim: u16) -> SchemaDescriptor {
    SchemaDescriptor {
        kind: KIND.into(),
        properties: vec!["bucket".into(), "embedding".into(), "n".into()],
        required: vec!["embedding".into(), "n".into()],
        types: vec![
            ("bucket".into(), PropertyType::Integer),
            ("embedding".into(), PropertyType::Vector { dim }),
            ("n".into(), PropertyType::Integer),
        ],
        ..SchemaDescriptor::default()
    }
}

fn sample_row(id: u64, dim: u16) -> SampleRow {
    let hidden = id % 100 == 99;
    let bucket = (id % 100) as u8;
    let vector = seeded_vector(id, dim as usize);
    let record = ObjectRecord {
        gen: 1,
        namespace: NS.into(),
        kind: KIND.into(),
        key: format!("k{id}"),
        hidden,
        action_id: None,
        props: HashMap::from([
            ("bucket".into(), bucket.to_string()),
            ("embedding".into(), encode_vector(&vector)),
            ("n".into(), "1".into()),
        ]),
        schema_version: 1,
    };
    SampleRow {
        record,
        hidden,
        bucket,
        vector,
    }
}

fn nearest_request(
    query: &str,
    object_bound: usize,
    page_size: usize,
    predicate: Option<Predicate>,
) -> EvaluateRequest {
    EvaluateRequest {
        namespace: NS.into(),
        root_kind: KIND.into(),
        hops: vec![],
        sum_kind: KIND.into(),
        sum_property: "n".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate,
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
    let mut centroid_state = cluster
        .wrapping_add(1)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15);
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

fn decoded_topk(rows: &[DecodedRow], query: &[f64], k: usize, filter_pct: u8) -> Vec<String> {
    let mut scored: Vec<(f64, &str)> = rows
        .iter()
        .filter(|row| filter_pct == 0 || row.bucket == 0)
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

fn recall_at_k(exact: &[String], approx: &[String], k: usize) -> f64 {
    if k == 0 {
        return 1.0;
    }
    let hits = exact
        .iter()
        .take(k)
        .filter(|key| approx.iter().take(k).any(|got| got == *key))
        .count();
    hits as f64 / k as f64
}

struct ClusteredIndex {
    dim: usize,
    nlist: usize,
    nprobe: usize,
    keys: Vec<String>,
    buckets: Vec<u8>,
    vectors: Vec<f64>,
    centroids: Vec<f64>,
    lists: Vec<Vec<u32>>,
    tail: Vec<u32>,
}

impl ClusteredIndex {
    fn build(rows: &[DecodedRow], dim: usize, tail_frac: f32) -> Result<Self, String> {
        if rows.is_empty() {
            return Err("no visible vectors".into());
        }
        let n = rows.len();
        let tail_n = ((n as f32) * tail_frac).ceil() as usize;
        let tail_n = tail_n.clamp(1, n.saturating_sub(16).max(1));
        let indexed = n - tail_n;
        let nlist = ((indexed as f64).sqrt() as usize / 2).clamp(16, 128);
        let nlist = nlist.min(indexed);
        let mut keys = Vec::with_capacity(n);
        let mut buckets = Vec::with_capacity(n);
        let mut vectors = Vec::with_capacity(n * dim);
        for row in rows {
            if row.vector.len() != dim {
                return Err("vector dim mismatch".into());
            }
            keys.push(row.key.clone());
            buckets.push(row.bucket);
            vectors.extend_from_slice(&row.vector);
        }
        let mut centroids = Vec::with_capacity(nlist * dim);
        let stride = (indexed / nlist).max(1);
        for list in 0..nlist {
            let src = (list * stride).min(indexed - 1) * dim;
            centroids.extend_from_slice(&vectors[src..src + dim]);
        }
        let mut lists = vec![Vec::new(); nlist];
        for _ in 0..KMEANS_ITERS {
            for list in lists.iter_mut() {
                list.clear();
            }
            for id in 0..indexed {
                let best = nearest_centroid(&vectors[id * dim..id * dim + dim], &centroids, dim);
                lists[best].push(id as u32);
            }
            for (list_id, members) in lists.iter().enumerate() {
                if members.is_empty() {
                    continue;
                }
                let dest = list_id * dim;
                for slot in centroids[dest..dest + dim].iter_mut() {
                    *slot = 0.0;
                }
                let inv = 1.0 / members.len() as f64;
                for id in members {
                    let src = *id as usize * dim;
                    for (slot, value) in centroids[dest..dest + dim]
                        .iter_mut()
                        .zip(&vectors[src..src + dim])
                    {
                        *slot += *value * inv;
                    }
                }
            }
        }
        let tail = ((indexed as u32)..(n as u32)).collect();
        Ok(Self {
            dim,
            nlist,
            nprobe: NPROBE.min(nlist),
            keys,
            buckets,
            vectors,
            centroids,
            lists,
            tail,
        })
    }

    fn query(&self, query: &[f64], k: usize, filter_pct: u8) -> Vec<String> {
        let mut scored: Vec<(f64, String)> = Vec::new();
        let mut seen = vec![false; self.keys.len()];
        let take = |id: u32, scored: &mut Vec<(f64, String)>, seen: &mut [bool]| {
            let id = id as usize;
            if seen[id] {
                return;
            }
            seen[id] = true;
            if filter_pct > 0 && self.buckets[id] != 0 {
                return;
            }
            let start = id * self.dim;
            scored.push((
                squared_l2(query, &self.vectors[start..start + self.dim]),
                self.keys[id].clone(),
            ));
        };
        if filter_pct > 0 {
            for id in 0..self.keys.len() as u32 {
                take(id, &mut scored, &mut seen);
            }
        } else {
            let mut centroid_hits: Vec<(f64, usize)> = (0..self.nlist)
                .map(|list| {
                    let start = list * self.dim;
                    (
                        squared_l2(query, &self.centroids[start..start + self.dim]),
                        list,
                    )
                })
                .collect();
            centroid_hits.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
            for (_, list) in centroid_hits.iter().take(self.nprobe) {
                for id in &self.lists[*list] {
                    take(*id, &mut scored, &mut seen);
                }
            }
            for id in &self.tail {
                take(*id, &mut scored, &mut seen);
            }
        }
        scored.sort_by(|a, b| cmp_score(a.0, a.1.as_str(), b.0, b.1.as_str()));
        scored.truncate(k);
        scored.into_iter().map(|(_, key)| key).collect()
    }

    fn bytes(&self) -> usize {
        self.vectors.len() * 8
            + self.centroids.len() * 8
            + self.keys.iter().map(|key| key.len()).sum::<usize>()
            + self.lists.iter().map(|list| list.len() * 4).sum::<usize>()
            + self.tail.len() * 4
            + self.buckets.len()
    }
}

fn nearest_centroid(vector: &[f64], centroids: &[f64], dim: usize) -> usize {
    let nlist = centroids.len() / dim;
    let mut best = 0;
    let mut best_d = f64::MAX;
    for list in 0..nlist {
        let d = squared_l2(vector, &centroids[list * dim..list * dim + dim]);
        if d < best_d {
            best_d = d;
            best = list;
        }
    }
    best
}

fn temp_dir(label: &str, objects: u64) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "mikura-ann-index-{label}-{objects}-{}-{nanos}",
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

    fn visible_rows(n: u64, dim: u16) -> (Store, Vec<DecodedRow>, PathBuf) {
        let dir = temp_dir("test", n);
        let log = dir.join("objects.mikura");
        let mut store = Store::create(&log).unwrap();
        store
            .append(sample_schema(dim).to_record(NS).unwrap())
            .unwrap();
        let mut rows = Vec::new();
        let mut batch = Vec::new();
        for id in 0..n {
            let row = sample_row(id, dim);
            if !row.hidden {
                rows.push(DecodedRow {
                    key: row.record.key.clone(),
                    bucket: row.bucket,
                    vector: row.vector,
                });
            }
            batch.push(row.record);
        }
        store.append_batch(batch).unwrap();
        (store, rows, dir)
    }

    #[test]
    fn decoded_matches_evaluate() {
        let n = 64;
        let dim = 8;
        let k = 5;
        let (store, rows, dir) = visible_rows(n, dim);
        let query = &rows[1].vector;
        let request = nearest_request(&encode_vector(query), n as usize, k, None);
        let eval: Vec<String> = LocalCompute
            .evaluate(&store, &request)
            .unwrap()
            .objects
            .into_iter()
            .map(|row| row.key)
            .collect();
        let decoded = decoded_topk(&rows, query, k, 0);
        assert_eq!(eval, decoded);
        let filtered = nearest_request(
            &encode_vector(query),
            n as usize,
            k,
            Some(Predicate::eq("bucket", "0")),
        );
        let eval: Vec<String> = LocalCompute
            .evaluate(&store, &filtered)
            .unwrap()
            .objects
            .into_iter()
            .map(|row| row.key)
            .collect();
        let decoded = decoded_topk(&rows, query, k, 1);
        assert_eq!(eval, decoded);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clustered_index_scans_unindexed_tail() {
        let mut rows = Vec::new();
        for id in 0..32u64 {
            let mut vector = vec![1.0; 4];
            vector[0] = id as f64;
            rows.push(DecodedRow {
                key: format!("k{id}"),
                bucket: 1,
                vector,
            });
        }
        rows[31].vector = vec![0.0; 4];
        let index = ClusteredIndex::build(&rows, 4, 0.05).unwrap();
        assert!(!index.tail.is_empty());
        let hits = index.query(&[0.0; 4], 1, 0);
        assert_eq!(hits, vec!["k31".to_string()]);
    }

    #[test]
    fn clustered_index_recall_on_separated_groups() {
        let mut rows = Vec::new();
        for id in 0..48u64 {
            let group = (id / 16) as f64 * 10.0;
            rows.push(DecodedRow {
                key: format!("k{id}"),
                bucket: 0,
                vector: vec![group, (id % 16) as f64 * 0.01],
            });
        }
        let index = ClusteredIndex::build(&rows, 2, 0.02).unwrap();
        let query = vec![0.0, 0.0];
        let exact = decoded_topk(&rows, &query, 4, 0);
        let approx = index.query(&query, 4, 0);
        assert!(
            recall_at_k(&exact, &approx, 4) >= 0.75,
            "exact={exact:?} approx={approx:?}"
        );
    }
}
