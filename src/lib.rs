//! In-process object database: object log, store, object-set evaluate.
//!
//! The object log is the store of record. [`Store`] rebuilds identity from
//! that log and keeps join maps as a checksummed sidecar (rebuilt from the
//! log if absent or stale). [`Store::load`] returns the live object for a
//! primary key from that projection, including the optional Action id, and
//! omits properties on the request deny list. Batch, changelog, merge, and
//! stream ingest live in `mikura-ingest`.
//! [`LocalCompute`] answers hop / count / sum from those maps, in either
//! join direction, with an optional exact-match on root properties.
//! `EvaluateRequest.object_bound` greater than zero also returns the
//! distinct matching objects and fails closed if the set is larger.
//! [`SparkCompute`] returns [`ComputeError::UnsupportedBackend`] until a
//! published envelope says otherwise.
//! A committed [`SchemaDescriptor`] (`mikura.schema/<kind>`) validates later
//! writes of that kind and optional [`Store::load_with_schema`] checks.
//! Optional `sums` on that descriptor persist last-hop parent rollups.
//! A committed [`OverlayPatch`] (`mikura.overlay/{kind}/{key}`) merges
//! named properties onto later source writes of that identity.
//!
//! See `docs/architecture.md` in the repository for the v1 contract.

mod acl;
mod actions;
mod codec;
mod compute;
mod joins;
mod log;
mod objectset;
mod overlay;
mod schema;
mod store;

pub use acl::{AclError, PropertyAcl};
pub use actions::Action;
pub use compute::{ComputeBackend, ComputeError, LocalCompute, SparkCompute};
pub use joins::JoinMaps;
pub use objectset::{Aggregate, EvaluateRequest, EvaluateResponse, ExactMatch, Hop, ObjectSet};
pub use overlay::{OverlayPatch, OVERLAY_CLEARED, OVERLAY_KIND};
pub use schema::{
    SchemaDescriptor, SchemaLink, SCHEMA_KIND, SCHEMA_LINKS, SCHEMA_PROPERTIES, SCHEMA_REQUIRED,
    SCHEMA_SUMS,
};
pub use store::{ObjectRecord, Store, JOIN_DELTA_COMPACT_BYTES};

#[cfg(test)]
mod crate_tests;
