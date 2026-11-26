//! Per-identity source version (ADR 0056): the stored version survives
//! reopen through every projection, writes that carry no version copy it
//! forward (#412), and `ingest_versioned` orders source writes by it (#413).

use super::super::*;
use super::helpers::*;
use crate::log::{LogWriter, SyncPolicy};

const STORED: u64 = 7;

/// Unversioned neighbours, so one dirty row commits as a delta frame
/// rather than a full sidecar rewrite.
fn others() -> Vec<ObjectRecord> {
    (0..8)
        .map(|i| rec("Other", &format!("o{i}"), false, &[("v", "1")]))
        .collect()
}

/// A log where `Thing/k` carries a source version, written below `Store` so
/// the fixture does not depend on `ingest_versioned`.
fn versioned_log(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let (dir, log) = temp_log(name);
    let mut writer = LogWriter::create(&log, SyncPolicy::None).unwrap();
    for record in others() {
        writer.append_record(&record).unwrap();
    }
    let mut record = rec("Thing", "k", false, &[("v", "1")]);
    record.source_version = STORED;
    writer.append_record(&record).unwrap();
    writer.flush().unwrap();
    drop(writer);
    (dir, log)
}

fn stored(store: &Store) -> (u64, u64) {
    let record = store
        .load(NS, "Thing", "k", &PropertyAcl::allow_all())
        .unwrap();
    (record.gen, record.source_version)
}

#[test]
fn unversioned_writes_copy_the_stored_version() {
    let (dir, log) = versioned_log("source-copy");
    let mut store = Store::open(&log).unwrap();
    assert_eq!(stored(&store), (1, STORED));

    store
        .append(rec("Thing", "k", false, &[("v", "2")]))
        .unwrap();
    assert_eq!(stored(&store), (2, STORED));
    store
        .append_batch(vec![rec("Thing", "k", false, &[("v", "3")])])
        .unwrap();
    assert_eq!(stored(&store), (3, STORED));
    let mut fenced = rec("Thing", "k", false, &[("v", "4")]);
    fenced.action_id = Some("act-4".into());
    store.ingest_fenced(fenced, 3).unwrap();
    assert_eq!(stored(&store), (4, STORED));
    store
        .apply_action(
            Action {
                id: "act-5".into(),
                namespace: NS.into(),
                kind: "Thing".into(),
                key: "k".into(),
                props: HashMap::from([("v".into(), "5".into())]),
            },
            Some(4),
        )
        .unwrap();
    assert_eq!(stored(&store), (5, STORED));
    store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "Thing".into(),
                key: "k".into(),
                props: HashMap::from([("note".into(), "kept".into())]),
                ..Default::default()
            },
            "act-6".into(),
            Some(5),
        )
        .unwrap();
    assert_eq!(stored(&store).1, STORED);
    store
        .hide(NS, "Thing", "k", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(stored(&store).1, STORED);

    // Only `ingest_versioned` raises the stored version.
    let mut above = rec("Thing", "k", false, &[("v", "9")]);
    above.source_version = STORED + 1;
    assert!(store.append(above).is_err());
    assert_eq!(stored(&store).1, STORED);

    let history = store
        .history(NS, "Thing", "k", &PropertyAcl::allow_all())
        .unwrap();
    assert!(history.iter().all(|v| v.record.source_version == STORED));
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Reopen from `log` and expect the version without decoding log pages, so
/// the value came from the projection under test.
fn reopen_from_projection(log: &std::path::Path, open: impl Fn(&std::path::Path) -> Store) {
    crate::log::take_pages_decoded();
    let store = open(log);
    assert_eq!(
        crate::log::take_pages_decoded(),
        0,
        "reopen replayed the log"
    );
    assert_eq!(stored(&store).1, STORED);
}

#[test]
fn stored_version_survives_checkpoint_and_join_sidecar() {
    let (dir, log) = versioned_log("source-projections");
    let mut store = Store::open(&log).unwrap();
    store.checkpoint().unwrap();
    drop(store);
    reopen_from_projection(&log, |log| Store::open(log).unwrap());

    std::fs::remove_file(Store::checkpoint_path(&log)).unwrap();
    let _ = std::fs::remove_file(Store::checkpoint_prev_path(&log));
    assert!(Store::join_map_path(&log).is_file());
    reopen_from_projection(&log, |log| Store::open(log).unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stored_version_survives_delta_frames() {
    let (dir, log) = versioned_log("source-delta");
    let mut store = Store::open(&log).unwrap();
    // An unversioned frame, then a sourced one: both kinds read in sequence.
    store
        .append(rec("Other", "o0", false, &[("v", "2")]))
        .unwrap();
    store
        .append(rec("Thing", "k", false, &[("v", "2")]))
        .unwrap();
    let delta = std::fs::read(Store::join_delta_path(&log)).unwrap();
    assert_eq!(&delta[..8], JOIN_DELTA_FORMAT);
    assert!(delta
        .windows(8)
        .any(|magic| magic == JOIN_DELTA_FORMAT_SOURCED));
    drop(store);
    let _ = std::fs::remove_file(Store::checkpoint_path(&log));
    let _ = std::fs::remove_file(Store::checkpoint_prev_path(&log));
    assert_eq!(std::fs::read(Store::join_delta_path(&log)).unwrap(), delta);
    reopen_from_projection(&log, |log| Store::open(log).unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stored_version_survives_paged_sidecar() {
    let (dir, log) = versioned_log("source-paged");
    let budget = || ServingBudget::from_bytes(1024 * 1024).expect("nonzero");
    drop(Store::open_with_serving_budget(&log, budget()).unwrap());
    let paged = std::fs::read(Store::join_map_path(&log)).unwrap();
    assert_eq!(&paged[..8], JOIN_FORMAT_PAGED_SOURCED);
    reopen_from_projection(&log, |log| {
        Store::open_with_serving_budget(log, budget()).unwrap()
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unversioned_projections_keep_their_bytes() {
    let (dir, log) = temp_log("source-none");
    let mut store = Store::create(&log).unwrap();
    store.append_batch(others()).unwrap();
    store
        .append(rec("Other", "o0", false, &[("v", "2")]))
        .unwrap();
    let delta = std::fs::read(Store::join_delta_path(&log)).unwrap();
    assert_eq!(&delta[..8], JOIN_DELTA_FORMAT);
    drop(store);
    let budget = ServingBudget::from_bytes(1024 * 1024).expect("nonzero");
    drop(Store::open_with_serving_budget(&log, budget).unwrap());
    let paged = std::fs::read(Store::join_map_path(&log)).unwrap();
    assert_eq!(&paged[..8], JOIN_FORMAT_PAGED);
    let _ = std::fs::remove_dir_all(&dir);
}

fn versioned(key: &str, v: &str) -> ObjectRecord {
    rec("Thing", key, false, &[("v", v)])
}

fn status_of(store: &mut Store, key: &str, version: u64) -> ApplyStatus {
    store
        .ingest_versioned(versioned(key, &version.to_string()), version)
        .unwrap()
}

#[test]
fn versioned_writes_apply_only_newer_positions() {
    let (dir, log) = temp_log("source-order");
    let mut store = Store::create(&log).unwrap();
    let statuses: Vec<_> = [3, 1, 3, 2, 5]
        .into_iter()
        .map(|version| status_of(&mut store, "k", version))
        .collect();
    use ApplyStatus::*;
    assert_eq!(statuses, [Applied, Stale, Duplicate, Stale, Applied]);
    let held = store
        .load(NS, "Thing", "k", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!((held.gen, held.source_version), (2, 5));
    assert_eq!(held.props["v"], "5");
    drop(store);

    let mut reopened = Store::open(&log).unwrap();
    assert_eq!(
        reopened
            .load(NS, "Thing", "k", &PropertyAcl::allow_all())
            .unwrap()
            .source_version,
        5
    );
    assert_eq!(status_of(&mut reopened, "k", 4), Stale);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn versioned_hide_keeps_order_and_payload() {
    let (dir, log) = temp_log("source-hide");
    let mut store = Store::create(&log).unwrap();
    assert_eq!(status_of(&mut store, "k", 1), ApplyStatus::Applied);
    let mut hide = versioned("k", "ignored");
    hide.hidden = true;
    assert_eq!(
        store.ingest_versioned(hide, 4).unwrap(),
        ApplyStatus::Applied
    );
    let held = store
        .load(NS, "Thing", "k", &PropertyAcl::allow_all())
        .unwrap();
    assert!(held.hidden);
    assert_eq!(
        held.props["v"], "1",
        "a versioned hide keeps the last payload"
    );
    assert_eq!(status_of(&mut store, "k", 3), ApplyStatus::Stale);

    // A hide that arrives before any upsert leaves a tombstone.
    let mut early = versioned("late", "");
    early.hidden = true;
    early.props.clear();
    assert_eq!(
        store.ingest_versioned(early, 2).unwrap(),
        ApplyStatus::Applied
    );
    assert_eq!(status_of(&mut store, "late", 1), ApplyStatus::Stale);
    assert_eq!(status_of(&mut store, "late", 3), ApplyStatus::Applied);
    assert!(
        !store
            .load(NS, "Thing", "late", &PropertyAcl::allow_all())
            .unwrap()
            .hidden
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn versioned_refresh_merges_the_overlay_and_edits_keep_the_version() {
    let (dir, log) = temp_log("source-overlay");
    let mut store = Store::create(&log).unwrap();
    status_of(&mut store, "k", 1);
    store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "Thing".into(),
                key: "k".into(),
                props: HashMap::from([("v".into(), "edited".into())]),
                ..Default::default()
            },
            "act-edit".into(),
            Some(1),
        )
        .unwrap();
    assert_eq!(stored(&store).1, 1);
    assert_eq!(status_of(&mut store, "k", 1), ApplyStatus::Duplicate);
    assert_eq!(status_of(&mut store, "k", 2), ApplyStatus::Applied);
    let held = store
        .load(NS, "Thing", "k", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        held.props["v"], "edited",
        "the edit survives a source refresh"
    );
    assert_eq!(held.source_version, 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn versioned_batch_checks_each_record_and_fails_whole() {
    let (dir, log) = temp_log("source-batch");
    let mut store = Store::create(&log).unwrap();
    let statuses = store
        .ingest_versioned_batch(vec![
            (versioned("a", "2"), 2),
            (versioned("a", "1"), 1),
            (versioned("a", "2"), 2),
            (versioned("b", "1"), 1),
        ])
        .unwrap();
    use ApplyStatus::*;
    assert_eq!(statuses, [Applied, Stale, Duplicate, Applied]);

    let mut reserved = versioned("c", "1");
    reserved.kind = "mikura.overlay".into();
    assert!(store
        .ingest_versioned_batch(vec![(versioned("c", "1"), 1), (reserved, 1)])
        .is_err());
    assert!(store
        .load(NS, "Thing", "c", &PropertyAcl::allow_all())
        .is_err());
    assert!(store.ingest_versioned(versioned("c", "0"), 0).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn racing_versioned_writers_end_at_the_highest_version() {
    let (dir, log) = temp_log("source-race");
    let shared = std::sync::Arc::new(SharedStore::new(Store::create(&log).unwrap()));
    // Four writers deliver interleaved positions 1..=40, each twice, in
    // different orders: retries and reordering, no generation reads.
    let threads: Vec<_> = (0..4u64)
        .map(|t| {
            let shared = std::sync::Arc::clone(&shared);
            std::thread::spawn(move || {
                let mut applied = 0usize;
                let versions: Vec<u64> = (1..=40).filter(|v| v % 2 == t % 2).collect();
                let order: Vec<u64> = if t < 2 {
                    versions
                } else {
                    versions.into_iter().rev().collect()
                };
                for version in order {
                    let status = shared
                        .write()
                        .unwrap()
                        .ingest_versioned(versioned("k", &version.to_string()), version)
                        .unwrap();
                    applied += usize::from(status == ApplyStatus::Applied);
                }
                applied
            })
        })
        .collect();
    let applied: usize = threads.into_iter().map(|t| t.join().unwrap()).sum();
    let store = shared.read().unwrap();
    let held = store
        .load(NS, "Thing", "k", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(held.source_version, 40);
    assert_eq!(held.props["v"], "40");
    assert_eq!(held.gen, applied as u64, "one generation per applied write");
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}
