//! One store shared by concurrent readers and one writer at a time, with a
//! split commit path for ingest batches (#261, ADR 0040 amendment).
//!
//! A split commit admits and seals a batch under shared access, writes and
//! syncs it while readers continue, and takes exclusive access only to apply
//! the durable records to the live projection. Readers therefore see the
//! state before the commit or after it, never a part of it, and never a
//! record that is not durable.

use std::collections::HashSet;
use std::ops::{Deref, DerefMut};
use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use super::{require_namespace, ObjectRecord, Store, SOURCE_KIND};
use crate::actions::CHANGE_SET_KIND;
use crate::log::StagedCommit;
use crate::overlay::{OverlayPatch, OVERLAY_KIND};
use crate::schema::SCHEMA_KIND;

/// A [`Store`] behind a reader/writer lock plus a single-writer lock.
///
/// Every write takes the writer lock first, so two writers never admit
/// against the same state, and nothing changes the store between a split
/// commit's admission and its apply. Readers take only the reader lock.
pub struct SharedStore {
    store: RwLock<Store>,
    writer: Mutex<()>,
}

/// Exclusive access for a write that runs entirely under the store lock.
pub struct StoreWriteGuard<'a> {
    store: RwLockWriteGuard<'a, Store>,
    _writer: MutexGuard<'a, ()>,
}

impl Deref for StoreWriteGuard<'_> {
    type Target = Store;

    fn deref(&self) -> &Store {
        &self.store
    }
}

impl DerefMut for StoreWriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut Store {
        &mut self.store
    }
}

/// Outcome of [`SharedStore::append_batch`].
pub enum BatchWrite {
    /// The split path ran. `Ok` means durable and visible.
    Done(Result<(), String>),
    /// The batch needs the exclusive path: [`SharedStore::write`] plus
    /// [`Store::append_batch`]. Nothing was written.
    Exclusive(Vec<ObjectRecord>),
}

/// A batch admitted against the live projection without changing it.
struct PreparedCommit {
    records: Vec<ObjectRecord>,
    staged: StagedCommit,
}

/// Sidecar work after a split commit is applied.
enum Persist {
    None,
    /// Append a delta frame stamped at these committed pages.
    Delta(u32),
}

const POISONED: &str = "store lock poisoned; reopen the store";
const WRITER_POISONED: &str = "store writer lock poisoned; reopen the store";

impl SharedStore {
    pub fn new(store: Store) -> Self {
        Self {
            store: RwLock::new(store),
            writer: Mutex::new(()),
        }
    }

    /// Shared access to the applied, committed state.
    pub fn read(&self) -> Result<RwLockReadGuard<'_, Store>, String> {
        self.store.read().map_err(|_| POISONED.to_string())
    }

    /// Exclusive access, after any writer in progress has finished.
    pub fn write(&self) -> Result<StoreWriteGuard<'_>, String> {
        let writer = self
            .writer
            .lock()
            .map_err(|_| WRITER_POISONED.to_string())?;
        let store = self.store.write().map_err(|_| POISONED.to_string())?;
        Ok(StoreWriteGuard {
            store,
            _writer: writer,
        })
    }

    pub fn into_inner(self) -> Result<Store, String> {
        self.store.into_inner().map_err(|_| POISONED.to_string())
    }

    /// Commit `records` as one unit (ADR 0026) without holding readers out
    /// during the log write and sync.
    ///
    /// 1. Under the writer lock and shared access, admit every record and
    ///    seal its pages ([`Store::prepare_batch`]).
    /// 2. Still shared, write and sync the pages and the superblock.
    /// 3. Exclusive, move the writer to the new commit point and apply the
    ///    records with the recovery replay step. Only then is the batch
    ///    acknowledged.
    ///
    /// A crash before step 2's superblock sync loses only this
    /// unacknowledged batch. A crash after it and before step 3 recovers
    /// the batch on open by replaying the log, once. A failure in step 2
    /// before the superblock leaves the log and the projection unchanged.
    ///
    /// A batch the split path does not cover returns
    /// [`BatchWrite::Exclusive`] untouched.
    pub fn append_batch(&self, records: Vec<ObjectRecord>) -> BatchWrite {
        let writer = match self.writer.lock() {
            Ok(guard) => guard,
            Err(_) => return BatchWrite::Done(Err(WRITER_POISONED.into())),
        };
        let durable = {
            let store = match self.read() {
                Ok(store) => store,
                Err(error) => return BatchWrite::Done(Err(error)),
            };
            let Some(mut prepared) = store.prepare_batch(&records) else {
                return BatchWrite::Exclusive(records);
            };
            store
                .writer
                .write_staged(&mut prepared.staged)
                .map(|()| prepared)
        };
        let prepared = match durable {
            Ok(prepared) => prepared,
            Err(error) => return BatchWrite::Done(Err(error)),
        };
        let persist = match self.store.write() {
            Ok(mut store) => store.apply_prepared(prepared),
            // The batch is durable but cannot be applied. Refuse every
            // further write so none lands past it; a reopen replays it.
            Err(poisoned) => {
                poisoned.into_inner().writer.poison();
                return BatchWrite::Done(Err(format!("{POISONED}: committed batch not applied")));
            }
        };
        let persisted = persist.and_then(|persist| match persist {
            Persist::None => Ok(()),
            Persist::Delta(pages) => self.persist_delta(pages),
        });
        drop(writer);
        BatchWrite::Done(persisted.map_err(|error| {
            format!("committed to the log; projection sidecar not persisted: {error}")
        }))
    }

    /// Append the dirty set under shared access, then record it exclusively.
    fn persist_delta(&self, pages: u32) -> Result<(), String> {
        let bytes = self.read()?.write_delta(pages)?;
        self.store
            .write()
            .map_err(|_| POISONED.to_string())?
            .adopt_delta(pages, bytes)
    }
}

impl Store {
    /// Admit `records` for a split commit and seal their pages, reading the
    /// live projection only. `None` sends the batch to the exclusive path,
    /// which also produces every admission error: the split path covers
    /// visible or hidden source records of distinct non-reserved identities
    /// with new or no Action ids, whose overlay merge logs no audit and
    /// whose hide copies no overlay, on a writer with nothing pending.
    fn prepare_batch(&self, records: &[ObjectRecord]) -> Option<PreparedCommit> {
        let mut identities = HashSet::with_capacity(records.len());
        let mut action_ids = HashSet::new();
        let mut admitted = Vec::with_capacity(records.len());
        for record in records {
            let reserved = [SCHEMA_KIND, OVERLAY_KIND, SOURCE_KIND, CHANGE_SET_KIND];
            if reserved.contains(&record.kind.as_str())
                || require_namespace(&record.namespace).is_err()
            {
                return None;
            }
            let identity = (&record.namespace, &record.kind, &record.key);
            if !identities.insert(identity) {
                return None;
            }
            if let Some(id) = record.action_id.as_deref() {
                let claimed = (record.namespace.clone(), id.to_string());
                if id.is_empty()
                    || !self.action_commits_from_log
                    || self.action_commits.contains_key(&claimed)
                    || !action_ids.insert(claimed)
                {
                    return None;
                }
            }
            let mut record = record.clone();
            if record.hidden {
                let overlay = self
                    .load_reserved(
                        &record.namespace,
                        OVERLAY_KIND,
                        &OverlayPatch::identity_key(&record.kind, &record.key),
                        OverlayPatch::from_record,
                    )
                    .ok()?;
                if overlay.is_some() {
                    return None;
                }
            } else if self.resolve_instance(&mut record, true).ok()?.is_some() {
                return None;
            }
            self.assign_gen(&mut record);
            admitted.push(record);
        }
        let staged = self.writer.stage(&admitted)?;
        Some(PreparedCommit {
            records: admitted,
            staged,
        })
    }

    /// Apply a durable split commit to the live projection: the writer
    /// moves to the new commit point and every record takes the recovery
    /// replay step, together under exclusive access. Returns the sidecar
    /// work to do once readers are back, or a sidecar error after the
    /// commit is already durable and applied.
    fn apply_prepared(&mut self, prepared: PreparedCommit) -> Result<Persist, String> {
        self.writer.adopt_staged(&prepared.staged);
        for record in prepared.records {
            // `apply_logged` fails only on a schema record, which the split
            // path never admits. A failure would leave the projection behind
            // the log, so refuse further writes until a reopen replays it.
            if let Err(error) = self.apply_logged(record) {
                self.writer.poison();
                return Err(format!("batch not applied, reopen the store: {error}"));
            }
        }
        self.sidecar_commits_since_persist = self.sidecar_commits_since_persist.saturating_add(1);
        if self.has_pending() || !self.projection_persist_due(false) {
            return Ok(Persist::None);
        }
        if self.writer.keys().is_some() || self.needs_checkpoint_rewrite() {
            // Encrypted sidecars and checkpoint rewrites keep the exclusive
            // persist. A failed persist leaves the dirty set for the next.
            self.persist_projection()?;
            return Ok(Persist::None);
        }
        Ok(Persist::Delta(self.writer.committed_pages()))
    }
}

#[cfg(test)]
impl Store {
    /// Run `hook` at each stage of a split commit's log write.
    pub(crate) fn set_stage_hook(&mut self, hook: Option<crate::log::StageHook>) {
        self.writer.set_stage_hook(hook);
    }
}
