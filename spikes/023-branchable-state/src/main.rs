//! Throwaway: does a consumer name isolated branches with merge, or do
//! overlay, change sets, and history already cover propose-review-commit?
//!
//! Not the store of record. Writable manifest forks are unpublished.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[cfg(test)]
use mikura::Predicate;
use mikura::{
    Aggregate, AsOf, ChangeSet, ComputeBackend, Edit, EvaluateRequest, Hop, LocalCompute,
    ObjectRecord, OverlayPatch, PropertyAcl, PropertyType, SchemaDescriptor, Store,
};

const NS: &str = "bench";
const COMPONENT: &str = "component";
const INCIDENT: &str = "incident";
const SAMPLES: usize = 5;
const BUDGET_MS: f64 = 500.0;
const PUBLISHED_QUERIES: usize = 3;
const PUBLISHED_WRITES: usize = 1;
const PUBLISHED_BRANCH_OPS: usize = 0;

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
    println!(
        "budget_ms={BUDGET_MS} objects={objects} published_queries={PUBLISHED_QUERIES} published_writes={PUBLISHED_WRITES} branch_ops={PUBLISHED_BRANCH_OPS}"
    );

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
    let fork_pages = store.committed_pages();
    let fork_version = store.manifest_version();
    let obj_dir = objects_dir(&log);
    let manifest = latest_manifest(&obj_dir)?;
    let manifest_bytes = fs::metadata(&manifest).map_err(|e| e.to_string())?.len();
    let data_bytes = dir_bytes(&obj_dir)?;

    let mut overlay_ns = Vec::with_capacity(SAMPLES);
    let mut change_ns = Vec::with_capacity(SAMPLES);
    let mut history_ns = Vec::with_capacity(SAMPLES);
    let mut fork_ns = Vec::with_capacity(SAMPLES);
    let mut copy_ns = Vec::with_capacity(SAMPLES);
    let mut read_ns = Vec::with_capacity(SAMPLES);
    let mut merge_ns = Vec::with_capacity(SAMPLES);
    let allow = PropertyAcl::allow_all();
    for sample in 0..SAMPLES {
        let t = Instant::now();
        store.apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: INCIDENT.into(),
                key: "inc-1".into(),
                props: HashMap::from([("note".into(), format!("acked-{sample}"))]),
                cleared: Vec::new(),
                action_id: None,
                at: None,
            },
            format!("act-inc-1-note-{sample}"),
            None,
        )?;
        overlay_ns.push(elapsed_ns(t));

        let current = store.load(NS, INCIDENT, "inc-1", &allow)?;
        let t = Instant::now();
        store.apply_change_set(ChangeSet {
            id: format!("act-set-{sample}"),
            namespace: NS.into(),
            edits: vec![Edit::Put {
                kind: INCIDENT.into(),
                key: "inc-1".into(),
                props: current.props.clone(),
                expected_gen: Some(current.gen),
            }],
        })?;
        change_ns.push(elapsed_ns(t));

        let t = Instant::now();
        let versions = store.history(NS, INCIDENT, "inc-1", &allow)?;
        history_ns.push(elapsed_ns(t));
        if versions.is_empty() {
            return Err("history of inc-1 is empty".into());
        }

        let dest = dir.join(format!("fork-{sample}"));
        let t = Instant::now();
        fs::copy(&manifest, &dest).map_err(|e| e.to_string())?;
        fork_ns.push(elapsed_ns(t));

        let copy_dest = dir.join(format!("copy-{sample}"));
        let t = Instant::now();
        copy_dir(&obj_dir, &copy_dest)?;
        copy_ns.push(elapsed_ns(t));

        let t = Instant::now();
        let as_of =
            store.load_as_of(NS, INCIDENT, "inc-1", &allow, AsOf::CommitPoint(fork_pages))?;
        read_ns.push(elapsed_ns(t));
        if as_of.record.props.contains_key("note") {
            return Err("fork-point load_as_of saw a later overlay".into());
        }

        let live = store.load(NS, INCIDENT, "inc-1", &allow)?;
        let t = Instant::now();
        let base =
            store.load_as_of(NS, INCIDENT, "inc-1", &allow, AsOf::CommitPoint(fork_pages))?;
        let mut theirs = live.clone();
        theirs
            .props
            .insert("note".into(), format!("other-{sample}"));
        let conflict = three_way(&base.record, &live, &theirs);
        merge_ns.push(elapsed_ns(t));
        if conflict != ConflictKind::DivergentUpdate {
            return Err(format!("expected divergent update, got {conflict:?}"));
        }
    }

    let seed = store.load(NS, COMPONENT, "svc-api", &allow)?;
    if seed.props.get("tier").map(String::as_str) != Some("prod") {
        return Err("seed component svc-api drifted".into());
    }
    let hop = LocalCompute
        .evaluate(&store, &hop_request(0))
        .map_err(|err| format!("{err:?}"))?;
    if hop.two_hop_count == 0 {
        return Err("seed hop count is 0".into());
    }

    println!(
        "ingest_ms={ingest_ms:.1} overlay_p50_ms={:.3} change_set_p50_ms={:.3} history_p50_ms={:.3} fork_p50_ms={:.3} data_copy_p50_ms={:.3} read_at_fork_p50_ms={:.3} merge_walk_p50_ms={:.3} manifest_bytes={manifest_bytes} data_bytes={data_bytes} fork_manifest={} fork_pages={fork_pages} peak_rss_bytes={}",
        p50(&overlay_ns) as f64 / 1_000_000.0,
        p50(&change_ns) as f64 / 1_000_000.0,
        p50(&history_ns) as f64 / 1_000_000.0,
        p50(&fork_ns) as f64 / 1_000_000.0,
        p50(&copy_ns) as f64 / 1_000_000.0,
        p50(&read_ns) as f64 / 1_000_000.0,
        p50(&merge_ns) as f64 / 1_000_000.0,
        fork_version,
        peak_rss_bytes(),
    );
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConflictKind {
    None,
    DivergentUpdate,
    DeleteVersusUpdate,
}

fn three_way(base: &ObjectRecord, ours: &ObjectRecord, theirs: &ObjectRecord) -> ConflictKind {
    if ours.hidden != theirs.hidden && (ours.hidden || theirs.hidden) {
        return ConflictKind::DeleteVersusUpdate;
    }
    if ours.gen != base.gen && theirs.gen != base.gen && ours.props != theirs.props {
        return ConflictKind::DivergentUpdate;
    }
    ConflictKind::None
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
        properties: vec!["affects".into(), "n".into(), "name".into(), "note".into()],
        required: vec!["affects".into(), "n".into(), "name".into()],
        types: vec![
            ("affects".into(), PropertyType::String),
            ("n".into(), PropertyType::Integer),
            ("name".into(), PropertyType::String),
            ("note".into(), PropertyType::String),
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
    let tier = if id.is_multiple_of(10) {
        "prod"
    } else {
        "staging"
    };
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

#[cfg(test)]
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

fn objects_dir(log: &Path) -> PathBuf {
    let mut path = log.as_os_str().to_os_string();
    path.push(".obj");
    PathBuf::from(path)
}

fn latest_manifest(obj_dir: &Path) -> Result<PathBuf, String> {
    let mut latest: Option<(u64, PathBuf)> = None;
    let entries = fs::read_dir(obj_dir).map_err(|e| e.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(rest) = name.strip_prefix("manifest.") else {
            continue;
        };
        let Ok(version) = rest.parse::<u64>() else {
            continue;
        };
        if latest.as_ref().is_none_or(|(seen, _)| version > *seen) {
            latest = Some((version, entry.path()));
        }
    }
    latest
        .map(|(_, path)| path)
        .ok_or_else(|| "missing manifest object".into())
}

fn dir_bytes(root: &Path) -> Result<u64, String> {
    let mut total = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let meta = entry.metadata().map_err(|e| e.to_string())?;
            if meta.is_dir() {
                stack.push(path);
            } else {
                total += meta.len();
            }
        }
    }
    Ok(total)
}

fn copy_dir(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let meta = entry.metadata().map_err(|e| e.to_string())?;
        if meta.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            fs::copy(&from, &to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn temp_dir(label: &str, objects: u64) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "mikura-branchable-state-{label}-{objects}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("temp dir");
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
    if let Ok(status) = fs::read_to_string("/proc/self/status") {
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
    let cpu = fs::read_to_string("/proc/cpuinfo")
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
    use mikura::{Action, VersionOrigin};

    const STAGE_NS: &str = "bench-stage";

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
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn overlay_and_change_set_cover_propose_commit() {
        let (mut store, dir) = seed_store();
        let allow = PropertyAcl::allow_all();
        store
            .apply_overlay(
                OverlayPatch {
                    namespace: NS.into(),
                    kind: INCIDENT.into(),
                    key: "inc-1".into(),
                    props: HashMap::from([("note".into(), "acked".into())]),
                    cleared: Vec::new(),
                    action_id: None,
                    at: None,
                },
                "act-inc-1-note".into(),
                None,
            )
            .unwrap();
        let live = store.load(NS, INCIDENT, "inc-1", &allow).unwrap();
        assert_eq!(live.props.get("note").map(String::as_str), Some("acked"));
        store
            .apply_change_set(ChangeSet {
                id: "act-reassign".into(),
                namespace: NS.into(),
                edits: vec![
                    Edit::Put {
                        kind: INCIDENT.into(),
                        key: "inc-1".into(),
                        props: HashMap::from([
                            ("affects".into(), "svc-api".into()),
                            ("n".into(), "1".into()),
                            ("name".into(), "elevated latency".into()),
                            ("note".into(), "acked".into()),
                        ]),
                        expected_gen: Some(live.gen),
                    },
                    Edit::Put {
                        kind: COMPONENT.into(),
                        key: "svc-api".into(),
                        props: HashMap::from([
                            ("n".into(), "1".into()),
                            ("name".into(), "billing-api".into()),
                            ("tier".into(), "prod".into()),
                        ]),
                        expected_gen: Some(1),
                    },
                ],
            })
            .unwrap();
        let versions = store.history(NS, INCIDENT, "inc-1", &allow).unwrap();
        assert!(versions.iter().any(|row| row.origin == VersionOrigin::Edit
            && row.record.action_id.as_deref() == Some("act-reassign")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn expected_gen_rejects_divergent_update() {
        let (mut store, dir) = seed_store();
        store
            .apply_action(
                Action {
                    id: "act-first".into(),
                    namespace: NS.into(),
                    kind: INCIDENT.into(),
                    key: "inc-1".into(),
                    props: HashMap::from([
                        ("affects".into(), "svc-api".into()),
                        ("n".into(), "1".into()),
                        ("name".into(), "elevated latency".into()),
                        ("note".into(), "first".into()),
                    ]),
                },
                Some(1),
            )
            .unwrap();
        let err = store
            .apply_change_set(ChangeSet {
                id: "act-stale".into(),
                namespace: NS.into(),
                edits: vec![Edit::Put {
                    kind: INCIDENT.into(),
                    key: "inc-1".into(),
                    props: HashMap::from([
                        ("affects".into(), "svc-api".into()),
                        ("n".into(), "1".into()),
                        ("name".into(), "elevated latency".into()),
                        ("note".into(), "branch".into()),
                    ]),
                    expected_gen: Some(1),
                }],
            })
            .unwrap_err();
        assert!(err.contains("stale generation"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hide_versus_put_is_delete_versus_update() {
        let base = sample_incident(1);
        let mut hidden = base.clone();
        hidden.hidden = true;
        hidden.gen = 2;
        let mut updated = base.clone();
        updated.gen = 2;
        updated.props.insert("note".into(), "acked".into());
        assert_eq!(
            three_way(&base, &hidden, &updated),
            ConflictKind::DeleteVersusUpdate
        );
    }

    #[test]
    fn staging_namespace_does_not_hop() {
        let (mut store, dir) = seed_store();
        store
            .append(component_schema().to_record(STAGE_NS).unwrap())
            .unwrap();
        store
            .append(incident_schema().to_record(STAGE_NS).unwrap())
            .unwrap();
        let mut staged = sample_incident(1);
        staged.namespace = STAGE_NS.into();
        store.append(staged).unwrap();
        let mut request = hop_request(8);
        request.namespace = STAGE_NS.into();
        let hop = eval_keys(&store, &request).unwrap();
        assert!(hop.is_empty(), "{hop:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_fork_is_smaller_than_data_copy() {
        let (store, dir) = seed_store();
        let log = dir.join("objects.mikura");
        let obj_dir = objects_dir(&log);
        let manifest = latest_manifest(&obj_dir).unwrap();
        let manifest_bytes = fs::metadata(&manifest).unwrap().len();
        let data_bytes = dir_bytes(&obj_dir).unwrap();
        assert!(manifest_bytes > 0);
        assert!(
            manifest_bytes < data_bytes,
            "manifest {manifest_bytes} data {data_bytes}"
        );
        assert!(store.manifest_version() >= 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn published_branch_op_count_is_zero() {
        assert_eq!(PUBLISHED_BRANCH_OPS, 0);
        assert_eq!(PUBLISHED_QUERIES, 3);
        assert_eq!(PUBLISHED_WRITES, 1);
    }
}
