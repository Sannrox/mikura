//! Write orchestrator for [`mikura::Store`].
//!
//! A control plane maps datasets and admitted edits to [`ObjectRecord`]s.
//! This crate diffs snapshots, merges admitted edits by identity, and appends
//! them. It does not know tenants, policy, or datasets.

use mikura::{ObjectRecord, Store};

mod changelog;
mod common;
mod merge;
mod stream;

pub use changelog::{snapshot_changelog, ChangelogIngest};
pub use merge::{merge_source_and_edits, MergeIngest};
pub use stream::StreamIngest;

pub struct BatchIngest;

impl BatchIngest {
    /// Append `records` and group-commit them as one unit. Any error drops the
    /// whole batch and leaves the store as it was (ADR 0026). This also covers
    /// [`ChangelogIngest`] and [`MergeIngest`], which delegate here.
    pub fn run(store: &mut Store, records: Vec<ObjectRecord>) -> Result<(), String> {
        let tokens = records.iter().map(common::intern_token_budget).sum();
        store.reserve_intern(tokens);
        store.append_batch(records)
    }
}

#[cfg(test)]
mod tests;
