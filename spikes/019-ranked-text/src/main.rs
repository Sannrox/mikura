//! Throwaway: do consumer string properties need ranked full-text, and does
//! an inverted projection beat a substring scan enough to earn its place?
//!
//! Not the store of record. The inverted index lives in this crate only.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use mikura::{
    Aggregate, ComputeBackend, EvaluateRequest, LocalCompute, ObjectRecord, Predicate, PropertyAcl,
    PropertyType, SchemaDescriptor, Store,
};

const NS: &str = "bench";
const KIND: &str = "doc";
const VOCAB: usize = 256;
const BODY_TERMS: usize = 40;
const SAMPLES: usize = 5;
const BUDGET_MS: f64 = 500.0;
const NEEDLE: &str = "needle";

fn main() {
    let objects = arg_u64("--objects", 10_000);
    let k = arg_u64("--k", 10) as usize;
    if objects < 32 {
        eprintln!("--objects must be at least 32");
        std::process::exit(2);
    }
    if k == 0 || k as u64 > objects {
        eprintln!("--k must be in 1..=objects");
        std::process::exit(2);
    }
    match run(objects, k) {
        Ok(()) => {}
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

fn run(objects: u64, k: usize) -> Result<(), String> {
    let (os, arch, cpus, cpu) = machine_class();
    println!("machine={os}/{arch} cpus={cpus} cpu={cpu}");
    println!("budget_ms={BUDGET_MS} k={k} objects={objects}");

    let dir = temp_dir("run", objects);
    let log = dir.join("objects.mikura");
    let mut store = Store::create(&log)?;
    store.append(doc_schema().to_record(NS)?)?;

    let mut rows = Vec::with_capacity(objects as usize);
    let chunk = (objects / 20).max(100) as usize;
    let mut batch = Vec::with_capacity(chunk);
    let t_ingest = Instant::now();
    for id in 0..objects {
        let row = sample_row(id);
        if !row.hidden {
            rows.push(DecodedRow {
                key: row.record.key.clone(),
                title: row.title.clone(),
                body: row.body.clone(),
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

    let t_build = Instant::now();
    let index = InvertedIndex::build(&rows);
    let build_ms = t_build.elapsed().as_secs_f64() * 1000.0;

    let eq_title = rows
        .iter()
        .find(|row| row.key == "k7")
        .map(|row| row.title.clone())
        .ok_or_else(|| "no visible lookup row".to_string())?;
    let eq_request = list_request(
        rows.len().max(objects as usize),
        0,
        Some(Predicate::eq("title", eq_title.as_str())),
    );
    let _ = LocalCompute
        .evaluate(&store, &eq_request)
        .map_err(|err| format!("{err:?}"))?;

    let mut eq_ns = Vec::with_capacity(SAMPLES);
    let mut substr_page_ns = Vec::with_capacity(SAMPLES);
    let mut substr_all_ns = Vec::with_capacity(SAMPLES);
    let mut prefix_all_ns = Vec::with_capacity(SAMPLES);
    let mut inverted_ns = Vec::with_capacity(SAMPLES);
    let mut exact_ns = Vec::with_capacity(SAMPLES);
    let mut eq_keys = Vec::new();
    for _ in 0..SAMPLES {
        let t = Instant::now();
        let response = LocalCompute
            .evaluate(&store, &eq_request)
            .map_err(|err| format!("{err:?}"))?;
        eq_ns.push(elapsed_ns(t));
        eq_keys = response.objects.iter().map(|row| row.key.clone()).collect();

        let t = Instant::now();
        std::hint::black_box(substring_keys(&rows, NEEDLE, k));
        substr_page_ns.push(elapsed_ns(t));

        let t = Instant::now();
        std::hint::black_box(substring_count(&rows, NEEDLE));
        substr_all_ns.push(elapsed_ns(t));

        let t = Instant::now();
        std::hint::black_box(prefix_count(&rows, "doc-1"));
        prefix_all_ns.push(elapsed_ns(t));

        let t = Instant::now();
        let inverted = index.query(&[NEEDLE], k);
        inverted_ns.push(elapsed_ns(t));

        let t = Instant::now();
        let exact = exact_rank(&rows, &[NEEDLE], k);
        exact_ns.push(elapsed_ns(t));

        if inverted != exact {
            return Err(format!(
                "dual-read miss inverted={inverted:?} exact={exact:?}"
            ));
        }
    }
    if eq_keys != vec!["k7".to_string()] {
        return Err(format!("eq lookup expected k7, got {eq_keys:?}"));
    }
    let inverted = index.query(&[NEEDLE], k);
    let exact = exact_rank(&rows, &[NEEDLE], k);
    let recall = recall_at_k(&exact, &inverted, k);
    println!(
        "eq_p50_ms={:.3} eq={} substr_page_p50_ms={:.3} substr_page={} substr_all_p50_ms={:.3} substr_all={} prefix_all_p50_ms={:.3} prefix_all={} inverted_p50_ms={:.3} inverted={} exact_p50_ms={:.3} exact={} recall@{k}={recall:.3} build_ms={build_ms:.1} ingest_ms={ingest_ms:.1} postings={} index_bytes={} peak_rss_bytes={ingest_rss}",
        p50(&eq_ns) as f64 / 1_000_000.0,
        hold_or_miss(p50(&eq_ns) as f64 / 1_000_000.0),
        p50(&substr_page_ns) as f64 / 1_000_000.0,
        hold_or_miss(p50(&substr_page_ns) as f64 / 1_000_000.0),
        p50(&substr_all_ns) as f64 / 1_000_000.0,
        hold_or_miss(p50(&substr_all_ns) as f64 / 1_000_000.0),
        p50(&prefix_all_ns) as f64 / 1_000_000.0,
        hold_or_miss(p50(&prefix_all_ns) as f64 / 1_000_000.0),
        p50(&inverted_ns) as f64 / 1_000_000.0,
        hold_or_miss(p50(&inverted_ns) as f64 / 1_000_000.0),
        p50(&exact_ns) as f64 / 1_000_000.0,
        hold_or_miss(p50(&exact_ns) as f64 / 1_000_000.0),
        index.posting_count(),
        index.bytes(),
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
    title: String,
    body: String,
}

struct DecodedRow {
    key: String,
    title: String,
    body: String,
}

fn doc_schema() -> SchemaDescriptor {
    SchemaDescriptor {
        kind: KIND.into(),
        properties: vec!["body".into(), "n".into(), "title".into()],
        required: vec!["body".into(), "n".into(), "title".into()],
        types: vec![
            ("body".into(), PropertyType::String),
            ("n".into(), PropertyType::Integer),
            ("title".into(), PropertyType::String),
        ],
        indexes: vec!["title".into()],
        ..SchemaDescriptor::default()
    }
}

fn sample_row(id: u64) -> SampleRow {
    let hidden = id % 100 == 99;
    let title = format!("doc-{id}");
    let body = sample_body(id);
    let record = ObjectRecord {
        gen: 1,
        namespace: NS.into(),
        kind: KIND.into(),
        key: format!("k{id}"),
        hidden,
        action_id: None,
        props: HashMap::from([
            ("body".into(), body.clone()),
            ("n".into(), "1".into()),
            ("title".into(), title.clone()),
        ]),
        schema_version: 1,
    };
    SampleRow {
        record,
        hidden,
        title,
        body,
    }
}

fn sample_body(id: u64) -> String {
    let mut state = id.wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut terms = Vec::with_capacity(BODY_TERMS + 6);
    for _ in 0..BODY_TERMS {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let word = (state >> 33) as usize % VOCAB;
        terms.push(format!("w{word}"));
    }
    if id % 100 == 0 {
        let copies = 1 + (id % 5) as usize;
        for _ in 0..copies {
            terms.push(NEEDLE.to_string());
        }
    }
    terms.join(" ")
}

fn list_request(
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
        sort: None,
        page_size,
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

fn substring_keys(rows: &[DecodedRow], needle: &str, k: usize) -> Vec<String> {
    rows.iter()
        .filter(|row| row.body.contains(needle))
        .map(|row| row.key.clone())
        .take(k)
        .collect()
}

fn substring_count(rows: &[DecodedRow], needle: &str) -> usize {
    rows.iter().filter(|row| row.body.contains(needle)).count()
}

fn prefix_count(rows: &[DecodedRow], prefix: &str) -> usize {
    rows.iter()
        .filter(|row| row.title.starts_with(prefix))
        .count()
}

fn term_tf(body: &str, term: &str) -> u32 {
    tokenize(body)
        .into_iter()
        .filter(|token| *token == term)
        .count() as u32
}

fn exact_rank(rows: &[DecodedRow], terms: &[&str], k: usize) -> Vec<String> {
    let n = rows.len() as f64;
    let df: Vec<f64> = terms
        .iter()
        .map(|term| {
            rows.iter()
                .filter(|row| term_tf(&row.body, term) > 0)
                .count() as f64
        })
        .collect();
    let mut scored: Vec<(f64, &str)> = rows
        .iter()
        .filter_map(|row| {
            let mut score = 0.0;
            let mut hit = false;
            for (term, df) in terms.iter().zip(&df) {
                let tf = term_tf(&row.body, term);
                if tf == 0 {
                    continue;
                }
                hit = true;
                let idf = ((n + 1.0) / (df + 1.0)).ln() + 1.0;
                score += tf as f64 * idf;
            }
            hit.then_some((score, row.key.as_str()))
        })
        .collect();
    scored.sort_by(|a, b| cmp_score(a.0, a.1, b.0, b.1));
    scored.truncate(k);
    scored.into_iter().map(|(_, key)| key.to_string()).collect()
}

fn cmp_score(ls: f64, lk: &str, rs: f64, rk: &str) -> Ordering {
    rs.partial_cmp(&ls)
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

struct InvertedIndex {
    n: usize,
    keys: Vec<String>,
    postings: HashMap<String, Vec<(u32, u32)>>,
}

impl InvertedIndex {
    fn build(rows: &[DecodedRow]) -> Self {
        let mut postings: HashMap<String, Vec<(u32, u32)>> = HashMap::new();
        let mut keys = Vec::with_capacity(rows.len());
        for (id, row) in rows.iter().enumerate() {
            keys.push(row.key.clone());
            let mut tf: HashMap<&str, u32> = HashMap::new();
            for token in tokenize(&row.body) {
                *tf.entry(token).or_insert(0) += 1;
            }
            for (term, count) in tf {
                postings
                    .entry(term.to_string())
                    .or_default()
                    .push((id as u32, count));
            }
        }
        Self {
            n: rows.len(),
            keys,
            postings,
        }
    }

    fn query(&self, terms: &[&str], k: usize) -> Vec<String> {
        let mut scores: HashMap<u32, f64> = HashMap::new();
        let n = self.n as f64;
        for term in terms {
            let Some(list) = self.postings.get(*term) else {
                continue;
            };
            let idf = ((n + 1.0) / (list.len() as f64 + 1.0)).ln() + 1.0;
            for (id, tf) in list {
                *scores.entry(*id).or_insert(0.0) += *tf as f64 * idf;
            }
        }
        let mut scored: Vec<(f64, &str)> = scores
            .into_iter()
            .map(|(id, score)| (score, self.keys[id as usize].as_str()))
            .collect();
        scored.sort_by(|a, b| cmp_score(a.0, a.1, b.0, b.1));
        scored.truncate(k);
        scored.into_iter().map(|(_, key)| key.to_string()).collect()
    }

    fn posting_count(&self) -> usize {
        self.postings.values().map(|list| list.len()).sum()
    }

    fn bytes(&self) -> usize {
        self.keys.iter().map(|key| key.len()).sum::<usize>()
            + self
                .postings
                .iter()
                .map(|(term, list)| term.len() + list.len() * 8)
                .sum::<usize>()
    }
}

fn temp_dir(label: &str, objects: u64) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "mikura-ranked-text-{label}-{objects}-{}-{nanos}",
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

    fn visible_rows(n: u64) -> (Store, Vec<DecodedRow>, PathBuf) {
        let dir = temp_dir("test", n);
        let log = dir.join("objects.mikura");
        let mut store = Store::create(&log).unwrap();
        store.append(doc_schema().to_record(NS).unwrap()).unwrap();
        let mut rows = Vec::new();
        let mut batch = Vec::new();
        for id in 0..n {
            let row = sample_row(id);
            if !row.hidden {
                rows.push(DecodedRow {
                    key: row.record.key.clone(),
                    title: row.title.clone(),
                    body: row.body.clone(),
                });
            }
            batch.push(row.record);
        }
        store.append_batch(batch).unwrap();
        (store, rows, dir)
    }

    #[test]
    fn eq_lookup_matches_evaluate() {
        let n = 64;
        let (store, rows, dir) = visible_rows(n);
        let title = rows
            .iter()
            .find(|row| row.key == "k7")
            .unwrap()
            .title
            .clone();
        let request = list_request(n as usize, 0, Some(Predicate::eq("title", title)));
        let keys: Vec<String> = LocalCompute
            .evaluate(&store, &request)
            .unwrap()
            .objects
            .into_iter()
            .map(|row| row.key)
            .collect();
        assert_eq!(keys, vec!["k7".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inverted_index_matches_exact_rank() {
        let n = 200;
        let (_store, rows, dir) = visible_rows(n);
        let index = InvertedIndex::build(&rows);
        let exact = exact_rank(&rows, &[NEEDLE], 10);
        let inverted = index.query(&[NEEDLE], 10);
        assert_eq!(inverted, exact);
        assert!(!exact.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn substring_scan_finds_needle_docs() {
        let n = 200;
        let (_store, rows, dir) = visible_rows(n);
        let hits = substring_keys(&rows, NEEDLE, 20);
        assert!(!hits.is_empty());
        for key in &hits {
            let row = rows.iter().find(|row| row.key == *key).unwrap();
            assert!(row.body.contains(NEEDLE));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
