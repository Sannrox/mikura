//! Throwaway: do published Explorer answers need set union, intersection,
//! or difference, or do `and`/`or`/`not` on one kind already cover them?
//!
//! Not the store of record. Combinators are not product operators.

use std::collections::HashMap;
use std::path::PathBuf;
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
    println!("budget_ms={BUDGET_MS} objects={objects} published_queries=3 combinator_queries=0");

    let dir = temp_dir("run", objects);
    let log = dir.join("objects.mikura");
    let mut store = Store::create(&log)?;
    store.append(component_schema().to_record(NS)?)?;
    store.append(incident_schema().to_record(NS)?)?;

    let chunk = (objects / 20).max(100) as usize;
    let mut batch = Vec::with_capacity(chunk);
    let t_ingest = Instant::now();
    for id in 0..objects {
        batch.push(sample_component(id));
        batch.push(sample_incident(id));
        if batch.len() >= chunk {
            store.append_batch(std::mem::take(&mut batch))?;
        }
    }
    if !batch.is_empty() {
        store.append_batch(batch)?;
    }
    let ingest_ms = t_ingest.elapsed().as_secs_f64() * 1000.0;

    let prod = Predicate::eq("tier", "prod");
    let staging = Predicate::eq("tier", "staging");
    let billing = Predicate::eq("name", "billing-api");
    let and_req = list_request(
        0,
        Some(Predicate::and(vec![prod.clone(), !billing.clone()])),
    );
    let or_req = list_request(0, Some(Predicate::or(vec![prod.clone(), staging.clone()])));
    let hop_req = hop_request(0);
    let eq_req = list_request(0, Some(prod.clone()));

    let _ = LocalCompute
        .evaluate(&store, &eq_req)
        .map_err(|err| format!("{err:?}"))?;

    let mut eq_ns = Vec::with_capacity(SAMPLES);
    let mut and_ns = Vec::with_capacity(SAMPLES);
    let mut or_ns = Vec::with_capacity(SAMPLES);
    let mut hop_ns = Vec::with_capacity(SAMPLES);
    let mut inter_ns = Vec::with_capacity(SAMPLES);
    let mut and_count = 0usize;
    let mut or_count = 0usize;
    for _ in 0..SAMPLES {
        let t = Instant::now();
        std::hint::black_box(
            LocalCompute
                .evaluate(&store, &eq_req)
                .map_err(|err| format!("{err:?}"))?,
        );
        eq_ns.push(elapsed_ns(t));

        let t = Instant::now();
        let and_resp = LocalCompute
            .evaluate(&store, &and_req)
            .map_err(|err| format!("{err:?}"))?;
        and_ns.push(elapsed_ns(t));
        and_count = and_resp.two_hop_count;

        let t = Instant::now();
        let or_resp = LocalCompute
            .evaluate(&store, &or_req)
            .map_err(|err| format!("{err:?}"))?;
        or_ns.push(elapsed_ns(t));
        or_count = or_resp.two_hop_count;

        let t = Instant::now();
        std::hint::black_box(
            LocalCompute
                .evaluate(&store, &hop_req)
                .map_err(|err| format!("{err:?}"))?,
        );
        hop_ns.push(elapsed_ns(t));

        let t = Instant::now();
        let prod_keys = eval_keys(&store, &list_request(objects as usize, Some(prod.clone())))?;
        let billing_keys = eval_keys(
            &store,
            &list_request(objects as usize, Some(billing.clone())),
        )?;
        let decoded_diff = difference(&prod_keys, &billing_keys);
        inter_ns.push(elapsed_ns(t));
        let and_keys = eval_keys(
            &store,
            &list_request(
                objects as usize,
                Some(Predicate::and(vec![prod.clone(), !billing.clone()])),
            ),
        )?;
        if and_keys != decoded_diff {
            return Err(format!(
                "dual-read miss and={and_keys:?} difference={decoded_diff:?}"
            ));
        }
    }
    if and_count == 0 || or_count == 0 {
        return Err("and/or returned no rows".into());
    }
    println!(
        "eq_p50_ms={:.3} eq={} and_p50_ms={:.3} and={} or_p50_ms={:.3} or={} hop_p50_ms={:.3} hop={} decoded_diff_p50_ms={:.3} decoded_diff={} and_hits={and_count} or_hits={or_count} ingest_ms={ingest_ms:.1} peak_rss_bytes={}",
        p50(&eq_ns) as f64 / 1_000_000.0,
        hold_or_miss(p50(&eq_ns) as f64 / 1_000_000.0),
        p50(&and_ns) as f64 / 1_000_000.0,
        hold_or_miss(p50(&and_ns) as f64 / 1_000_000.0),
        p50(&or_ns) as f64 / 1_000_000.0,
        hold_or_miss(p50(&or_ns) as f64 / 1_000_000.0),
        p50(&hop_ns) as f64 / 1_000_000.0,
        hold_or_miss(p50(&hop_ns) as f64 / 1_000_000.0),
        p50(&inter_ns) as f64 / 1_000_000.0,
        hold_or_miss(p50(&inter_ns) as f64 / 1_000_000.0),
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

fn sample_component(id: u64) -> ObjectRecord {
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
    ObjectRecord {
        gen: 1,
        namespace: NS.into(),
        kind: COMPONENT.into(),
        key,
        hidden,
        action_id: None,
        props: HashMap::from([
            ("n".into(), "1".into()),
            ("name".into(), name),
            ("tier".into(), tier.into()),
        ]),
        schema_version: 1,
    }
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

fn difference(left: &[String], right: &[String]) -> Vec<String> {
    left.iter()
        .filter(|key| !right.contains(key))
        .cloned()
        .collect()
}

#[cfg(test)]
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
    store.append(sample_component(0)).unwrap();
    store.append(sample_incident(1)).unwrap();
    (store, dir)
}

fn temp_dir(label: &str, objects: u64) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "mikura-set-combinators-{label}-{objects}-{}-{nanos}",
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
        let (store, dir) = seed_store();
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
    fn same_kind_difference_is_and_not() {
        let (store, dir) = seed_store();
        let prod = eval_keys(
            &store,
            &list_request(8, Some(Predicate::eq("tier", "prod"))),
        )
        .unwrap();
        let billing = eval_keys(
            &store,
            &list_request(8, Some(Predicate::eq("name", "billing-api"))),
        )
        .unwrap();
        let decoded = difference(&prod, &billing);
        let composed = eval_keys(
            &store,
            &list_request(
                8,
                Some(Predicate::and(vec![
                    Predicate::eq("tier", "prod"),
                    !Predicate::eq("name", "billing-api"),
                ])),
            ),
        )
        .unwrap();
        assert_eq!(composed, decoded);
        assert!(decoded.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_kind_union_is_or() {
        let n = 64;
        let dir = temp_dir("or", n);
        let log = dir.join("objects.mikura");
        let mut store = Store::create(&log).unwrap();
        store
            .append(component_schema().to_record(NS).unwrap())
            .unwrap();
        store
            .append_batch((0..n).map(sample_component).collect())
            .unwrap();
        let prod = eval_keys(
            &store,
            &list_request(n as usize, Some(Predicate::eq("tier", "prod"))),
        )
        .unwrap();
        let staging = eval_keys(
            &store,
            &list_request(n as usize, Some(Predicate::eq("tier", "staging"))),
        )
        .unwrap();
        let mut decoded = prod.clone();
        decoded.extend(staging);
        decoded.sort();
        decoded.dedup();
        let composed = eval_keys(
            &store,
            &list_request(
                n as usize,
                Some(Predicate::or(vec![
                    Predicate::eq("tier", "prod"),
                    Predicate::eq("tier", "staging"),
                ])),
            ),
        )
        .unwrap();
        assert_eq!(composed, decoded);
        assert!(!composed.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mixed_kind_union_is_not_a_product_operator() {
        let (store, dir) = seed_store();
        let components = eval_keys(&store, &list_request(8, None)).unwrap();
        let incidents = LocalCompute
            .evaluate(
                &store,
                &EvaluateRequest {
                    namespace: NS.into(),
                    root_kind: INCIDENT.into(),
                    hops: vec![],
                    sum_kind: INCIDENT.into(),
                    sum_property: "n".into(),
                    aggregate: Aggregate::CountAndSum,
                    acl: PropertyAcl::allow_all(),
                    filter: None,
                    predicate: None,
                    object_bound: 8,
                    sort: None,
                    page_size: 0,
                    cursor: None,
                    breakdown: None,
                    then_by: Vec::new(),
                },
            )
            .unwrap()
            .objects
            .into_iter()
            .map(|row| row.key)
            .collect::<Vec<_>>();
        assert_eq!(components, vec!["svc-api".to_string()]);
        assert_eq!(incidents, vec!["inc-1".to_string()]);
        assert_ne!(components, incidents);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
