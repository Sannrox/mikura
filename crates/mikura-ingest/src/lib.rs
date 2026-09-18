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
    pub fn run(store: &mut Store, records: Vec<ObjectRecord>) -> Result<(), String> {
        let tokens = records.iter().map(common::intern_token_budget).sum();
        store.reserve_intern(tokens);
        for record in records {
            store.append_uncommitted(record)?;
        }
        store.commit()
    }
}

#[cfg(test)]
mod tests;
