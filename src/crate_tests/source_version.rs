//! Read release of the per-identity source version (ADR 0056, #412): the
//! stored version survives reopen through every projection, and writes that
//! carry no version copy it forward.

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

/// A log where `Thing/k` carries a source version, the way a write release
/// would leave it. This release cannot set one through `Store`.
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

    // This release never raises the stored version.
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
