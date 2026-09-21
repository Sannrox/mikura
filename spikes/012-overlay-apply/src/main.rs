//! Cost of a new overlay on an existing identity (#223).
//!
//! Compares, on the product `Store` through its public API only:
//! - A: whole-record `append` (one commit per record)
//! - B: whole-record `append_uncommitted`, one commit at the end
//! - C: `apply_overlay` on an existing identity (one commit per overlay)
//! - D: batch source ingest of identities that carry an overlay vs identities
//!   that do not (the overlay merge on the write path)
//!
//! The default group-commit policy syncs every 32 pages, so B and D are
//! amortized durable costs, not CPU-only work. `SyncPolicy` is not public, so
//! this harness cannot switch that off; read B and D as comparisons.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use mikura::{ObjectRecord, OverlayPatch, PropertyAcl, Store};

fn instance(i: usize, note: &str) -> ObjectRecord {
    let mut props: HashMap<String, String> = (0..12)
        .map(|p| {
            (
                format!("prop{p:02}"),
                format!("value-{i}-{p}-xxxxxxxxxxxxxxxx"),
            )
        })
        .collect();
    props.insert("customer_id".into(), format!("c{}", i % 500));
    props.insert("note".into(), note.into());
    ObjectRecord {
        gen: 1,
        kind: "incident".into(),
        key: format!("inc-{i:07}"),
        hidden: false,
        action_id: None,
        props,
    }
}

fn arg(name: &str, default: usize) -> usize {
    let args: Vec<String> = std::env::args().collect();
    args.windows(2)
        .find(|pair| pair[0] == name)
        .and_then(|pair| pair[1].parse().ok())
        .unwrap_or(default)
}

fn per_op(elapsed: std::time::Duration, ops: usize) -> f64 {
    elapsed.as_secs_f64() * 1e6 / ops as f64
}

fn main() {
    let n = arg("--objects", 20_000);
    let k = arg("--ops", 2_000);
    assert!(n >= 4 * k, "--objects must be at least 4 * --ops");

    let dir: PathBuf =
        std::env::temp_dir().join(format!("mikura-overlay-spike-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("objects.mikura");
    let mut store = Store::create(&log).unwrap();
    store
        .append_batch((0..n).map(|i| instance(i, "seed")).collect())
        .unwrap();
    let size = |path: &PathBuf| std::fs::metadata(path).unwrap().len();

    println!("objects={n} ops={k}");

    let before = size(&log);
    let t = Instant::now();
    for j in 0..k {
        store.append(instance(j, &format!("a{j}"))).unwrap();
    }
    println!(
        "A whole-record append+commit : {:9.1} us/op  log +{} B/op",
        per_op(t.elapsed(), k),
        (size(&log) - before) / k as u64
    );

    let t = Instant::now();
    for j in 0..k {
        store
            .append_uncommitted(instance(k + j, &format!("b{j}")))
            .unwrap();
    }
    let appended = t.elapsed();
    let t = Instant::now();
    store.commit().unwrap();
    println!(
        "B append_uncommitted, group syncs included: {:7.1} us/op  (final commit: {:.1} ms)",
        per_op(appended, k),
        t.elapsed().as_secs_f64() * 1e3
    );

    let before = size(&log);
    let t = Instant::now();
    for j in 0..k {
        store
            .apply_overlay(
                OverlayPatch {
                    kind: "incident".into(),
                    key: format!("inc-{:07}", 2 * k + j),
                    props: HashMap::from([("note".into(), format!("n{j}"))]),
                    cleared: Vec::new(),
                    action_id: None,
                },
                format!("act-{j}"),
                None,
            )
            .unwrap();
    }
    println!(
        "C apply_overlay (new, existing id): {:6.1} us/op  log +{} B/op",
        per_op(t.elapsed(), k),
        (size(&log) - before) / k as u64
    );
    let probe = store
        .load(
            "incident",
            &format!("inc-{:07}", 2 * k),
            &PropertyAcl::allow_all(),
        )
        .unwrap();
    assert_eq!(probe.props.get("note").map(String::as_str), Some("n0"));

    let with_overlay: Vec<_> = (2 * k..3 * k).map(|i| instance(i, "src")).collect();
    let t = Instant::now();
    store.append_batch(with_overlay).unwrap();
    println!(
        "D1 source ingest, overlay present (batch incl. commit): {:6.1} us/record (batch of {k})",
        per_op(t.elapsed(), k)
    );
    let without_overlay: Vec<_> = (3 * k..4 * k).map(|i| instance(i, "src")).collect();
    let t = Instant::now();
    store.append_batch(without_overlay).unwrap();
    println!(
        "D2 source ingest, no overlay      (batch incl. commit): {:6.1} us/record (batch of {k})",
        per_op(t.elapsed(), k)
    );

    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}
