//! Compile-lock and on-disk check for the listed 1.0 surface
//! (`docs/plans/1.0-surface.md`, ADR 0042).
use std::collections::HashMap;
use std::io::Read;

use mikura::{
    backup, AclError, Action, Aggregate, AsOf, Breakdown, ChangeSet, ComputeError, ConflictAudit,
    ConflictPlan, ConflictStrategy, Edit, EvaluateRequest, EvaluateResponse, ExactMatch, Group,
    Hop, JoinMaps, LocalCompute, ObjectRecord, ObjectSet, ObjectVersion, OverlayPatch, Predicate,
    PropertyAcl, PropertyType, PropertyValue, SchemaDescriptor, SchemaLink, SchemaMigration, Sort,
    SparkCompute, Store, VersionOrigin, CHANGE_SET_BOUND, CHANGE_SET_KIND, CHECKPOINT_FORMAT,
    HIDE_IDENTITIES_BOUND, HIDE_KINDS_BOUND, JOIN_DELTA_COMPACT_BYTES, JOIN_DELTA_FORMAT,
    JOIN_DELTA_FORMAT_V5, JOIN_FORMAT, LOG_FORMAT, MAX_GROUPS, OVERLAY_AT, OVERLAY_CLEARED,
    OVERLAY_CONFLICT, OVERLAY_KIND, OVERLAY_LOST_PREFIX, SCHEMA_CONFLICT,
    SCHEMA_CONFLICT_PROPERTIES, SCHEMA_CONFLICT_TIME, SCHEMA_INDEXES, SCHEMA_KIND, SCHEMA_LINKS,
    SCHEMA_MIGRATE_CAST, SCHEMA_MIGRATE_DROP, SCHEMA_MIGRATE_RENAME, SCHEMA_MIGRATE_RETARGET,
    SCHEMA_PROPERTIES, SCHEMA_REQUIRED, SCHEMA_SUMS, SCHEMA_TYPES, SCHEMA_VERSION, SOURCE_KIND,
    SOURCE_OFFSET,
};
use mikura_ingest::{
    snapshot_changelog, ApplyStatus, BatchIngest, ChangelogIngest, MergeIngest, StreamApply,
    StreamChange, StreamIngest, STREAM_APPLY_LATENCY_TARGET_SECS,
};

#[test]
fn listed_mikura_types_are_public() {
    let _ = std::any::type_name::<Store>();
    let _ = std::any::type_name::<ObjectRecord>();
    let _ = std::any::type_name::<AsOf>();
    let _ = std::any::type_name::<ObjectVersion>();
    let _ = std::any::type_name::<VersionOrigin>();
    let _ = std::any::type_name::<ObjectSet<LocalCompute>>();
    let _ = std::any::type_name::<EvaluateRequest>();
    let _ = std::any::type_name::<EvaluateResponse>();
    let _ = std::any::type_name::<Hop>();
    let _ = std::any::type_name::<Aggregate>();
    let _ = std::any::type_name::<ExactMatch>();
    let _ = std::any::type_name::<Predicate>();
    let _ = std::any::type_name::<Sort>();
    let _ = std::any::type_name::<Breakdown>();
    let _ = std::any::type_name::<Group>();
    let _ = std::any::type_name::<PropertyAcl>();
    let _ = std::any::type_name::<AclError>();
    let _ = std::any::type_name::<Action>();
    let _ = std::any::type_name::<ChangeSet>();
    let _ = std::any::type_name::<Edit>();
    let _ = std::any::type_name::<ComputeError>();
    let _ = std::any::type_name::<SparkCompute>();
    let _ = std::any::type_name::<OverlayPatch>();
    let _ = std::any::type_name::<ConflictAudit>();
    let _ = std::any::type_name::<SchemaDescriptor>();
    let _ = std::any::type_name::<SchemaLink>();
    let _ = std::any::type_name::<SchemaMigration>();
    let _ = std::any::type_name::<ConflictPlan>();
    let _ = std::any::type_name::<ConflictStrategy>();
    let _ = std::any::type_name::<PropertyType>();
    let _ = std::any::type_name::<PropertyValue>();
    let _ = std::any::type_name::<JoinMaps>();
    let _ = std::any::type_name::<backup::RestorePoint>();
    let _ = std::any::type_name::<backup::VerifyReport>();
    let _ = (
        backup::backup_log
            as fn(&std::path::Path, &std::path::Path) -> Result<backup::RestorePoint, String>,
        backup::backup_log_incremental
            as fn(&std::path::Path, &std::path::Path) -> Result<backup::RestorePoint, String>,
        backup::verify_log as fn(&std::path::Path) -> Result<backup::VerifyReport, String>,
        backup::restore_point_at
            as fn(&std::path::Path, u32) -> Result<backup::RestorePoint, String>,
        mikura::tenant_namespace as fn(&str, &str) -> Result<String, String>,
        MAX_GROUPS,
        CHANGE_SET_BOUND,
        CHANGE_SET_KIND,
        HIDE_IDENTITIES_BOUND,
        HIDE_KINDS_BOUND,
        JOIN_DELTA_COMPACT_BYTES,
        OVERLAY_AT,
        OVERLAY_CLEARED,
        OVERLAY_CONFLICT,
        OVERLAY_KIND,
        OVERLAY_LOST_PREFIX,
        SCHEMA_CONFLICT,
        SCHEMA_CONFLICT_PROPERTIES,
        SCHEMA_CONFLICT_TIME,
        SCHEMA_INDEXES,
        SCHEMA_KIND,
        SCHEMA_LINKS,
        SCHEMA_MIGRATE_CAST,
        SCHEMA_MIGRATE_DROP,
        SCHEMA_MIGRATE_RENAME,
        SCHEMA_MIGRATE_RETARGET,
        SCHEMA_PROPERTIES,
        SCHEMA_REQUIRED,
        SCHEMA_SUMS,
        SCHEMA_TYPES,
        SCHEMA_VERSION,
        SOURCE_KIND,
        SOURCE_OFFSET,
        LOG_FORMAT,
        JOIN_FORMAT,
        JOIN_DELTA_FORMAT,
        JOIN_DELTA_FORMAT_V5,
        CHECKPOINT_FORMAT,
    );
}

#[test]
fn listed_ingest_types_are_public() {
    let _ = std::any::type_name::<BatchIngest>();
    let _ = std::any::type_name::<ChangelogIngest>();
    let _ = std::any::type_name::<MergeIngest>();
    let _ = std::any::type_name::<StreamIngest>();
    let _ = std::any::type_name::<StreamApply>();
    let _ = std::any::type_name::<StreamChange>();
    let _ = std::any::type_name::<ApplyStatus>();
    let _ = (
        snapshot_changelog as fn(Vec<ObjectRecord>, Vec<ObjectRecord>) -> Vec<ObjectRecord>,
        mikura_ingest::merge_source_and_edits
            as fn(Vec<ObjectRecord>, Vec<ObjectRecord>) -> Vec<ObjectRecord>,
        backup::restore_log
            as fn(
                &std::path::Path,
                &std::path::Path,
                Option<backup::RestorePoint>,
            ) -> Result<backup::RestorePoint, String>,
        STREAM_APPLY_LATENCY_TARGET_SECS,
        mikura_ingest::SOURCE_KIND,
        mikura_ingest::SOURCE_OFFSET,
    );
}

#[test]
fn new_log_writes_documented_log_format() {
    let dir = std::env::temp_dir().join(format!("mikura-stability-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("objects.mikura");
    let mut store = Store::create(&log).unwrap();
    store
        .append(ObjectRecord {
            gen: 1,
            namespace: "ns".into(),
            kind: "Thing".into(),
            key: "k".into(),
            hidden: false,
            action_id: None,
            schema_version: 1,
            props: HashMap::new(),
        })
        .unwrap();
    drop(store);

    let mut page = [0u8; 12];
    std::fs::File::open(&log)
        .unwrap()
        .read_exact(&mut page)
        .unwrap();
    assert_eq!(&page[4..12], LOG_FORMAT);
    let _ = std::fs::remove_dir_all(&dir);
}
