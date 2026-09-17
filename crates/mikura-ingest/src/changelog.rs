use mikura::{ObjectRecord, Store};

use crate::common::{fold_snapshot, source_payload_eq};
use crate::BatchIngest;

/// Diff two source snapshots into records to append.
///
/// Identity is `(kind, key)`. Within each snapshot the last record for an
/// identity wins. New keys emit the current row; changed `props`, `hidden`,
/// or `action_id` emit the current row; keys that disappear emit a hide of
/// the last visible payload. Identical snapshots emit nothing. The list is
/// not authority; only the object log after append is.
pub fn snapshot_changelog(
    previous: Vec<ObjectRecord>,
    current: Vec<ObjectRecord>,
) -> Vec<ObjectRecord> {
    let previous = fold_snapshot(previous);
    let current = fold_snapshot(current);
    let mut records = Vec::new();
    for (id, old) in &previous {
        if current.contains_key(id) || old.hidden {
            continue;
        }
        let mut hide = old.clone();
        hide.hidden = true;
        records.push(hide);
    }
    for (id, new) in &current {
        match previous.get(id) {
            None => records.push(new.clone()),
            Some(old) if !source_payload_eq(old, new) => records.push(new.clone()),
            Some(_) => {}
        }
    }
    records.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.key.cmp(&right.key))
    });
    records
}

pub struct ChangelogIngest;

impl ChangelogIngest {
    /// Diff two snapshots and group-commit only the records that changed.
    /// An empty diff does not append.
    pub fn run(
        store: &mut Store,
        previous: Vec<ObjectRecord>,
        current: Vec<ObjectRecord>,
    ) -> Result<(), String> {
        let records = snapshot_changelog(previous, current);
        if records.is_empty() {
            return Ok(());
        }
        BatchIngest::run(store, records)
    }
}
