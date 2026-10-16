//! In-process object database: object log, store, object-set evaluate.
//!
//! The object log is the store of record. [`Store`] rebuilds identity from
//! that log, from a log checkpoint plus tail, or from a matching join
//! sidecar (rebuilt from the log if absent or stale). [`Store::load`] returns the live object for
//! `(namespace, kind, key)` from that projection, including the optional
//! Action id, and omits properties on the request deny list and treats
//! restriction-hidden identities as missing. Missing or unknown namespace
//! fails closed. Batch, changelog, merge, and
//! stream ingest live in `mikura-ingest`.
//! [`LocalCompute`] answers hop / count / sum from those maps, in either
//! join direction, with an optional exact-match on root properties or a
//! composed [`Predicate`] tree (ADR 0015). Optional predicates after each
//! hop restrict the far set. `EvaluateRequest.object_bound` greater than
//! zero also returns the distinct matching objects and fails closed if
//! the set is larger. [`Sort`] plus `page_size` returns snapshot pages;
//! the opaque cursor binds the restriction, query, and live writer stamp.
//! [`SparkCompute`] always returns [`ComputeError::UnsupportedBackend`].
//! A committed [`SchemaDescriptor`] (`mikura.schema/<kind>`) validates later
//! writes of that kind and optional [`Store::load_with_schema`] checks.
//! A later visible descriptor is a new schema version: additive changes
//! commit as before; recast, rename, retarget, and drop need a declared
//! migration ([`Store::load_at_schema`] names an earlier version).
//! Optional `sums` on that descriptor persist last-hop parent rollups.
//! Optional `types` declare boolean, integer, timestamp, and decimal values
//! stored as canonical UTF-8 in `props` ([`PropertyType`]).
//! A committed [`OverlayPatch`] (`mikura.overlay/{kind}/{key}`) merges
//! named properties onto later source writes of that identity. Default
//! merge is user-wins; a schema [`ConflictStrategy`] may select
//! newest-timestamp-wins per kind or per property.
//!
//! A [`ChangeSet`] commits edits to several identities under one Action
//! id, all or nothing ([`Store::apply_change_set`]).
//!
//! See `docs/architecture.md` in the repository for the v1 contract.
//! Format ids on the 1.0 surface: [`LOG_FORMAT`], [`JOIN_FORMAT`],
//! [`JOIN_DELTA_FORMAT`], [`CHECKPOINT_FORMAT`].

mod acl;
mod actions;
pub mod backup;
mod codec;
mod compute;
mod joins;
mod log;
mod objectset;
mod overlay;
mod page;
mod schema;
mod store;
mod value;

/// Superblock magic this crate writes and opens (`MIKURAV2`).
pub const LOG_FORMAT: &[u8; 8] = b"MIKURAV2";
/// Join sidecar magic (`MKJOIN05`). Rebuildable; unknown magic fails closed.
pub const JOIN_FORMAT: &[u8; 8] = b"MKJOIN05";
/// Join dirty-set magic this crate writes (`MKJOIN6D`).
pub const JOIN_DELTA_FORMAT: &[u8; 8] = b"MKJOIN6D";
/// Earlier dirty-set magic this crate still reads (`MKJOIN5D`).
pub const JOIN_DELTA_FORMAT_V5: &[u8; 8] = b"MKJOIN5D";
/// Log checkpoint magic (`MKCKPT01`). Unknown magic falls back.
pub const CHECKPOINT_FORMAT: &[u8; 8] = b"MKCKPT01";

pub use acl::{AclError, PropertyAcl, HIDE_IDENTITIES_BOUND, HIDE_KINDS_BOUND};
pub use actions::{Action, ChangeSet, Edit, CHANGE_SET_BOUND, CHANGE_SET_KIND};
pub use compute::{ComputeBackend, ComputeError, LocalCompute, SparkCompute};
pub use joins::JoinMaps;
pub use objectset::{
    Aggregate, Breakdown, EvaluateRequest, EvaluateResponse, ExactMatch, Group, Hop, ObjectSet,
    Predicate, Sort, MAX_GROUPS,
};
pub use overlay::{
    ConflictAudit, OverlayPatch, OVERLAY_AT, OVERLAY_CLEARED, OVERLAY_CONFLICT, OVERLAY_KIND,
    OVERLAY_LOST_PREFIX,
};
pub use schema::{
    ConflictPlan, ConflictStrategy, SchemaDescriptor, SchemaLink, SchemaMigration, SCHEMA_CONFLICT,
    SCHEMA_CONFLICT_PROPERTIES, SCHEMA_CONFLICT_TIME, SCHEMA_KIND, SCHEMA_LINKS,
    SCHEMA_MIGRATE_CAST, SCHEMA_MIGRATE_DROP, SCHEMA_MIGRATE_RENAME, SCHEMA_MIGRATE_RETARGET,
    SCHEMA_PROPERTIES, SCHEMA_REQUIRED, SCHEMA_SUMS, SCHEMA_TYPES, SCHEMA_VERSION,
};
pub use store::{
    tenant_namespace, AsOf, ObjectRecord, ObjectVersion, Store, VersionOrigin,
    JOIN_DELTA_COMPACT_BYTES, SOURCE_KIND, SOURCE_OFFSET,
};
pub use value::{PropertyType, PropertyValue};

#[cfg(test)]
mod crate_tests;
