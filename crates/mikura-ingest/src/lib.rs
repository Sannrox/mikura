//! Write orchestrator for [`mikura::Store`].
//!
//! A control plane maps datasets and admitted edits to [`ObjectRecord`]s.
//! This crate diffs snapshots, merges admitted edits by identity, appends
//! them, and applies an ordered source stream with a durable offset cursor.
//! It does not know tenants, policy, or datasets.

use mikura::{ObjectRecord, Store};

mod apply;
mod changelog;
mod common;
mod merge;
mod stream;

pub use apply::{
    ApplyStatus, StreamApply, StreamChange, SOURCE_KIND, SOURCE_OFFSET,
    STREAM_APPLY_LATENCY_TARGET_SECS,
};
pub use changelog::{snapshot_changelog, ChangelogIngest};
pub use merge::{merge_source_and_edits, MergeIngest};
pub use stream::StreamIngest;

pub struct BatchIngest;

impl BatchIngest {
    /// Append `records` and group-commit them as one unit. Any error drops the
    /// whole batch and leaves the store as it was (ADR 0026). This also covers
    /// [`ChangelogIngest`] and [`MergeIngest`], which delegate here.
    pub fn run(store: &mut Store, records: Vec<ObjectRecord>) -> Result<(), String> {
        for record in &records {
            store.reserve_intern(&record.namespace, common::intern_token_budget(record));
        }
        store.append_batch(records)
    }
}

#[cfg(test)]
mod tests;
