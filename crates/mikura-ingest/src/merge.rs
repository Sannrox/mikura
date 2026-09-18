use mikura::{ObjectRecord, Store};

use crate::common::fold_last_wins;
use crate::BatchIngest;

/// Merge a source snapshot and admitted edits for one write cycle.
///
/// Identity is `(kind, key)`. Within each input, the last record for an
/// identity wins. Edits then replace source for the same identity, including
/// `hidden`: a hidden source stays hidden unless an edit unhides it. The
/// returned list is not authority; only the object log after append is.
pub fn merge_source_and_edits(
    source: Vec<ObjectRecord>,
    edits: Vec<ObjectRecord>,
) -> Vec<ObjectRecord> {
    let (order, mut chosen) = fold_last_wins(source.into_iter().chain(edits));
    order
        .into_iter()
        .filter_map(|id| chosen.remove(&id))
        .collect()
}

pub struct MergeIngest;

impl MergeIngest {
    /// Merge source records and admitted edits, then group-commit the result.
    pub fn run(
        store: &mut Store,
        source: Vec<ObjectRecord>,
        edits: Vec<ObjectRecord>,
    ) -> Result<(), String> {
        BatchIngest::run(store, merge_source_and_edits(source, edits))
    }
}
