//! Source writes ordered by a per-identity source version (ADR 0056).
//!
//! A versioned write applies only when its version is greater than the
//! identity's stored one. Replays and late arrivals are answered without a
//! new generation, so a source drain retries blindly instead of fencing on
//! generation.

use serde::{Deserialize, Serialize};

use super::{ObjectRecord, Store};
use crate::acl::PropertyAcl;

/// Outcome of one ordered source write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyStatus {
    /// Written as a new generation.
    Applied,
    /// The identity already holds this position. Nothing written.
    Duplicate,
    /// The identity holds a newer position. Nothing written.
    Stale,
}

impl Store {
    /// Apply a source write only when `source_version` is greater than the
    /// identity's stored version (0 when absent). An equal version is
    /// [`ApplyStatus::Duplicate`], a smaller one [`ApplyStatus::Stale`];
    /// neither writes. An applied write merges the overlay under the kind's
    /// conflict plan and commits the record with its conflict audit row, as
    /// [`Self::ingest_fenced`] does, but needs no generation read.
    ///
    /// A hidden record is a versioned hide. It keeps the identity's last
    /// payload, as [`Self::hide`] does; an absent identity is recorded as a
    /// hidden tombstone so an older upsert that arrives later stays `Stale`.
    /// `record.action_id` is optional provenance and is only claimed when
    /// the write applies. Reserved kinds and version 0 fail closed.
    pub fn ingest_versioned(
        &mut self,
        record: ObjectRecord,
        source_version: u64,
    ) -> Result<ApplyStatus, String> {
        let mut statuses = self.ingest_versioned_batch(vec![(record, source_version)])?;
        statuses.pop().ok_or_else(|| "missing apply status".into())
    }

    /// [`Self::ingest_versioned`] for several records as one commit. Each
    /// record is checked on its own, after the batch records before it, and
    /// the result lists each record's status in order. Any error writes
    /// nothing.
    pub fn ingest_versioned_batch(
        &mut self,
        records: Vec<(ObjectRecord, u64)>,
    ) -> Result<Vec<ApplyStatus>, String> {
        for (record, version) in &records {
            if record.kind.starts_with("mikura.") {
                return Err(format!(
                    "ingest_versioned refuses reserved kind {}",
                    record.kind
                ));
            }
            if *version == 0 {
                return Err("source version must be at least 1".into());
            }
        }
        let mut statuses = Vec::with_capacity(records.len());
        self.atomically(|store| {
            for (record, version) in records {
                let status = store.write_versioned(record, version)?;
                statuses.push(status);
            }
            Ok(())
        })?;
        Ok(statuses)
    }

    fn write_versioned(
        &mut self,
        mut record: ObjectRecord,
        version: u64,
    ) -> Result<ApplyStatus, String> {
        let stored = self
            .find_live(&record.namespace, &record.kind, &record.key)
            .and_then(|id| self.live_meta(id))
            .map_or(0, |meta| meta.source_version);
        if version == stored {
            return Ok(ApplyStatus::Duplicate);
        }
        if version < stored {
            return Ok(ApplyStatus::Stale);
        }
        if record.hidden {
            if let Ok(current) = self.load(
                &record.namespace,
                &record.kind,
                &record.key,
                &PropertyAcl::allow_all(),
            ) {
                record.props = current.props;
            }
        }
        record.source_version = version;
        self.raising_source_version = true;
        let written = self.append_uncommitted(record);
        self.raising_source_version = false;
        written.map(|()| ApplyStatus::Applied)
    }
}
