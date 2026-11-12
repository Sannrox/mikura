//! Seeded fault-injection harness for commit and recovery (#345).
//!
//! Generated IO faults, not named commit stages. Oracles: acknowledged
//! writes survive reopen; a multi-record in-flight write is all or nothing;
//! reopen matches log replay; the same seed replays the same trace.

use super::super::*;
use super::helpers::*;
use crate::fault::{allow_recovery, install, take_trace, FaultKind, Plan, CI_SEEDS};
use crate::log::read_records;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn unique_log(tag: &str) -> (PathBuf, PathBuf) {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mikura-fault-{tag}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("objects.mikura");
    (dir, log)
}

fn item(key: &str, n: &str) -> ObjectRecord {
    rec("Item", key, false, &[("n", n)])
}

fn wide(key: &str) -> ObjectRecord {
    // Each framed record must fit in one page (~4090 body bytes) and two
    // must not, so a generated fault can land between their page writes.
    let payload = "x".repeat(2500);
    rec("Item", key, false, &[("payload", &payload)])
}

#[derive(Default)]
struct Live {
    acked: BTreeMap<String, ObjectRecord>,
    inflight: Vec<String>,
}

fn ack(live: &mut Live, records: &[ObjectRecord]) {
    live.inflight.clear();
    for record in records {
        live.acked.insert(record.key.clone(), record.clone());
    }
}

fn load_item(store: &Store, key: &str) -> Option<ObjectRecord> {
    store.load(NS, "Item", key, &PropertyAcl::allow_all()).ok()
}

fn visible(store: &Store) -> BTreeSet<String> {
    ["a", "b", "c", "d", "e"]
        .into_iter()
        .filter(|key| load_item(store, key).is_some())
        .map(str::to_string)
        .collect()
}

fn same_body(got: &ObjectRecord, expected: &ObjectRecord) -> bool {
    got.kind == expected.kind
        && got.key == expected.key
        && got.hidden == expected.hidden
        && got.props == expected.props
}

fn honest_items(log: &Path) -> BTreeMap<String, ObjectRecord> {
    read_records(log)
        .unwrap_or_default()
        .into_iter()
        .filter(|record| record.kind == "Item")
        .map(|record| (record.key.clone(), record))
        .collect()
}

fn recovered_items(store: &Store) -> BTreeMap<String, ObjectRecord> {
    ["a", "b", "c", "d", "e"]
        .into_iter()
        .filter_map(|key| load_item(store, key).map(|record| (key.to_string(), record)))
        .collect()
}

fn bodies_match(
    got: &BTreeMap<String, ObjectRecord>,
    expected: &BTreeMap<String, ObjectRecord>,
    label: &str,
) -> Result<(), String> {
    if got.len() != expected.len()
        || got.iter().any(|(key, record)| {
            expected
                .get(key)
                .is_none_or(|wanted| !same_body(record, wanted))
        })
    {
        return Err(format!("{label} {got:?} != {expected:?}"));
    }
    Ok(())
}

fn dual_read(log: &Path, expected: &BTreeMap<String, ObjectRecord>) -> Result<(), String> {
    let _ = std::fs::remove_file(Store::join_map_path(log));
    let _ = std::fs::remove_file(Store::join_delta_path(log));
    let _ = std::fs::remove_file(Store::checkpoint_path(log));
    let _ = std::fs::remove_file(Store::checkpoint_prev_path(log));
    let rebuilt = Store::open(log).map_err(|e| format!("dual-read open: {e}"))?;
    bodies_match(&recovered_items(&rebuilt), expected, "dual-read")
}

fn check_oracles(log: &Path, live: &Live) -> Result<(), String> {
    let store = match Store::open(log) {
        Ok(store) => store,
        Err(error) => {
            return if live.acked.is_empty() {
                Ok(())
            } else {
                Err(format!("open lost acked writes: {error}"))
            };
        }
    };
    let got = recovered_items(&store);
    for (key, expected) in &live.acked {
        match got.get(key) {
            Some(record) if same_body(record, expected) => {}
            Some(record) => {
                return Err(format!("acked {key} body {record:?} != {expected:?}"));
            }
            None => return Err(format!("acked {key} missing after reopen")),
        }
    }
    if got.contains_key("d") && !live.acked.contains_key("d") {
        return Err("uncommitted tail became visible".into());
    }
    if !live.inflight.is_empty() {
        let present = live
            .inflight
            .iter()
            .filter(|key| got.contains_key(*key))
            .count();
        if present != 0 && present != live.inflight.len() {
            return Err(format!("partial in-flight write: {:?}", got.keys()));
        }
    }
    let replay = honest_items(log);
    bodies_match(&got, &replay, "reopen vs log replay")?;
    drop(store);
    dual_read(log, &got)
}

fn drive(log: &Path) -> Live {
    let mut live = Live::default();
    let mut store = match Store::create(log) {
        Ok(store) => store,
        Err(_) => return live,
    };
    if crate::fault::crashed() {
        return live;
    }
    for (key, n) in [("a", "1"), ("b", "2")] {
        let record = item(key, n);
        live.inflight = vec![key.into()];
        if store.append(record.clone()).is_err() || crate::fault::crashed() {
            return live;
        }
        ack(&mut live, &[record]);
    }
    let batch = vec![wide("c"), wide("e")];
    live.inflight = vec!["c".into(), "e".into()];
    // Payloads large enough that the two records occupy separate pages, so
    // a generated fault can land between their writes.
    if store.append_batch(batch.clone()).is_err() || crate::fault::crashed() {
        return live;
    }
    ack(&mut live, &batch);
    live.inflight.clear();
    if store.append_uncommitted(item("d", "4")).is_err() || crate::fault::crashed() {
        return live;
    }
    live.inflight = vec!["d".into()];
    live
}

fn count_ops() -> u64 {
    let (dir, log) = unique_log("count");
    let _guard = install(None, false);
    let live = drive(&log);
    let n = take_trace().len() as u64;
    drop(_guard);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        live.acked.contains_key("c") && live.acked.contains_key("e"),
        "fault-free workload must commit the multi-page batch"
    );
    assert_eq!(
        live.inflight,
        vec!["d".to_string()],
        "fault-free workload must leave an uncommitted tail"
    );
    n
}

fn run_plan(
    tag: &str,
    plan: Option<Plan>,
    orphans: bool,
) -> Result<Vec<(crate::fault::Op, Option<&'static str>)>, String> {
    let (dir, log) = unique_log(tag);
    let _guard = install(plan, orphans);
    let live = drive(&log);
    let trace = take_trace();
    allow_recovery();
    let result = check_oracles(&log, &live);
    drop(_guard);
    let _ = std::fs::remove_dir_all(&dir);
    result.map(|()| trace)
}

fn run_seed(
    seed: u64,
    ops: u64,
    orphans: bool,
) -> Result<Vec<(crate::fault::Op, Option<&'static str>)>, String> {
    run_plan(&format!("seed-{seed}"), Plan::from_seed(seed, ops), orphans)
}

#[test]
fn bounded_seed_batch_holds_oracles() {
    let ops = count_ops();
    assert!(ops > 0, "workload issued no IO");
    for seed in 0..CI_SEEDS {
        run_seed(seed, ops, false).unwrap_or_else(|error| {
            panic!("seed {seed} plan={:?}: {error}", Plan::from_seed(seed, ops));
        });
    }
}

#[test]
fn crash_at_every_step_holds_oracles() {
    let ops = count_ops();
    assert!(ops > 0, "workload issued no IO");
    for step in 0..ops {
        let plan = Plan {
            step,
            kind: FaultKind::Crash,
        };
        run_plan(&format!("crash-{step}"), Some(plan), false).unwrap_or_else(|error| {
            panic!("crash at step {step}: {error}");
        });
    }
}

#[test]
fn same_seed_replays_the_same_trace() {
    let ops = count_ops();
    let seed = 7;
    let first = run_seed(seed, ops, false).expect("first run");
    let second = run_seed(seed, ops, false).expect("second run");
    assert_eq!(first, second, "seed {seed} must replay");
}

#[test]
fn re_finds_orphan_pages_as_authority() {
    let ops = count_ops();
    assert!(ops > 0);
    let mut found = None;
    for step in 0..ops {
        for kind in [FaultKind::Crash, FaultKind::FailAfter] {
            let plan = Plan { step, kind };
            let (dir, log) = unique_log(&format!("orphan-{step}-{}", kind.label()));
            let _guard = install(Some(plan), true);
            let live = drive(&log);
            allow_recovery();
            drop(_guard);
            let honest: BTreeSet<_> = honest_items(&log).into_keys().collect();
            let _guard = install(None, true);
            let got = Store::open(&log)
                .map(|store| visible(&store))
                .unwrap_or_default();
            drop(_guard);
            let extra: BTreeSet<_> = got.difference(&honest).cloned().collect();
            let _ = std::fs::remove_dir_all(&dir);
            if !extra.is_empty() && extra.iter().any(|key| !live.acked.contains_key(key)) {
                found = Some((step, kind, extra));
                break;
            }
        }
        if found.is_some() {
            break;
        }
    }
    assert!(
        found.is_some(),
        "harness did not re-find orphan pages as authority across {ops} steps"
    );
}

#[test]
fn displaced_writer_fails_closed() {
    let (dir, log) = unique_log("fence");
    let _store = Store::create(&log).unwrap();
    let error = match Store::open(&log) {
        Ok(_) => panic!("second writer opened the log"),
        Err(error) => error,
    };
    assert!(error.starts_with(LOG_IN_USE), "second writer: {error}");
    let _ = std::fs::remove_dir_all(&dir);
}
