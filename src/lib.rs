//! In-process object database: object log, store, object-set evaluate.
//!
//! The object log is the store of record. [`Store`] rebuilds identity from
//! that log and keeps join maps as a checksummed sidecar (rebuilt from the
//! log if absent or stale). [`Store::load`] returns the live object for a
//! primary key from that projection, including the optional Action id, and
//! omits properties on the request deny list and treats restriction-hidden
//! identities as missing. Batch, changelog, merge, and
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
//! A later visible descriptor is rejected when it recasts a previously
//! declared type or retargets an outgoing link.
//! Optional `sums` on that descriptor persist last-hop parent rollups.
//! Optional `types` declare boolean, integer, timestamp, and decimal values
//! stored as canonical UTF-8 in `props` ([`PropertyType`]).
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
mod page;
mod schema;
mod store;
mod value;

pub use acl::{AclError, PropertyAcl, HIDE_IDENTITIES_BOUND, HIDE_KINDS_BOUND};
pub use actions::Action;
pub use compute::{ComputeBackend, ComputeError, LocalCompute, SparkCompute};
pub use joins::JoinMaps;
pub use objectset::{
    Aggregate, EvaluateRequest, EvaluateResponse, ExactMatch, Hop, ObjectSet, Predicate, Sort,
};
pub use overlay::{OverlayPatch, OVERLAY_CLEARED, OVERLAY_KIND};
pub use schema::{
    SchemaDescriptor, SchemaLink, SCHEMA_KIND, SCHEMA_LINKS, SCHEMA_PROPERTIES, SCHEMA_REQUIRED,
    SCHEMA_SUMS, SCHEMA_TYPES,
};
pub use store::{ObjectRecord, Store, JOIN_DELTA_COMPACT_BYTES};
pub use value::{PropertyType, PropertyValue};

#[cfg(test)]
mod crate_tests;
