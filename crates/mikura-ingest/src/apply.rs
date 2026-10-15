use std::collections::HashMap;

use mikura::{ObjectRecord, PropertyAcl, Store, OVERLAY_KIND, SCHEMA_KIND};

pub use mikura::{SOURCE_KIND, SOURCE_OFFSET};

use crate::common::{identity, intern_token_budget, Identity};

const SOURCE_CURSOR_TOKEN_MAX: usize = 1024;

/// Documented end-to-end target: a flushed stream change is visible to
/// `load` / `evaluate` on this process within this many seconds on the
/// product-loop fixture. Not a p95 SLO.
pub const STREAM_APPLY_LATENCY_TARGET_SECS: u64 = 5;

/// One source change: monotone stream offset plus the object to apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamChange {
    pub offset: u64,
    pub record: ObjectRecord,
}

/// Result of [`StreamApply::apply`]. Skipped changes do not append or bump
/// generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyStatus {
    Applied,
    Duplicate,
    Stale,
}

/// Ordered per-key stream apply with a durable source cursor.
///
/// Offsets are monotone positions on one named source. For each identity,
/// a change applies only when its offset is strictly greater than the last
/// offset applied to that identity (or the last committed source offset
/// after resume). Replays and older offsets skip without a new generation.
/// `flush` writes `mikura.source/<source>` and group-commits. Overlay merge
/// on a visible source write follows the kind descriptor (default user-wins)
/// ([`Store::append_uncommitted`]).
#[derive(Debug)]
pub struct StreamApply {
    namespace: String,
    source: String,
    bound: usize,
    uncommitted: usize,
    last_by_id: HashMap<Identity, u64>,
    last_committed: Option<u64>,
    pending_max: Option<u64>,
}

impl StreamApply {
    /// Bound outstanding uncommitted applied records. `bound` must be
    /// greater than 0. `source` is the cursor key; it must be non-empty
    /// and must not contain `/`.
    pub fn new(
        namespace: impl Into<String>,
        source: impl Into<String>,
        bound: usize,
    ) -> Result<Self, String> {
        let namespace = namespace.into();
        let source = source.into();
        require_apply_namespace(&namespace)?;
        require_source(&source)?;
        if namespace.len() > SOURCE_CURSOR_TOKEN_MAX || source.len() > SOURCE_CURSOR_TOKEN_MAX {
            return Err("source cursor identity is too long for one log page".into());
        }
        if bound == 0 {
            return Err("stream apply bound must be greater than 0".into());
        }
        Ok(Self {
            namespace,
            source,
            bound,
            uncommitted: 0,
            last_by_id: HashMap::new(),
            last_committed: None,
            pending_max: None,
        })
    }

    /// Rebuild the cursor from the last committed `mikura.source/<source>`
    /// row. Missing cursor starts at the beginning. Unknown namespace is
    /// an empty store for this apply, not an error.
    pub fn resume(
        store: &Store,
        namespace: impl Into<String>,
        source: impl Into<String>,
        bound: usize,
    ) -> Result<Self, String> {
        let mut apply = Self::new(namespace, source, bound)?;
        if store.has_pending() {
            return Err("stream apply resume requires a committed store".into());
        }
        apply.last_committed = load_committed_offset(store, &apply.namespace, &apply.source)?;
        Ok(apply)
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn bound(&self) -> usize {
        self.bound
    }

    pub fn uncommitted(&self) -> usize {
        self.uncommitted
    }

    /// Last offset written by a successful [`Self::flush`], if any.
    pub fn last_committed_offset(&self) -> Option<u64> {
        self.last_committed
    }

    /// Apply `change` in offset order for its identity. Live maps update
    /// on [`ApplyStatus::Applied`]; durability still requires [`Self::flush`].
    pub fn apply(
        &mut self,
        store: &mut Store,
        change: StreamChange,
    ) -> Result<ApplyStatus, String> {
        if change.record.namespace != self.namespace {
            return Err(format!(
                "stream apply namespace {} does not match record {}",
                self.namespace, change.record.namespace
            ));
        }
        if change.record.kind == SOURCE_KIND
            || change.record.kind == OVERLAY_KIND
            || change.record.kind == SCHEMA_KIND
        {
            return Err(format!(
                "stream apply cannot ingest reserved kind {}",
                change.record.kind
            ));
        }
        let id = identity(&change.record);
        match self.last_applied(&id) {
            Some(last) if change.offset == last => return Ok(ApplyStatus::Duplicate),
            Some(last) if change.offset < last => return Ok(ApplyStatus::Stale),
            _ => {}
        }
        if self.uncommitted >= self.bound {
            return Err(format!(
                "stream apply bound {} exceeded; flush before applying more",
                self.bound
            ));
        }
        store.hold_commits(true);
        store.reserve_intern(
            &change.record.namespace,
            intern_token_budget(&change.record),
        );
        if let Err(error) = store.append_uncommitted(change.record) {
            if self.uncommitted == 0 && !store.has_pending() {
                store.hold_commits(false);
            }
            return Err(error);
        }
        self.last_by_id.insert(id, change.offset);
        let offset = self.pending_max.unwrap_or(change.offset).max(change.offset);
        self.pending_max = Some(offset);
        self.uncommitted += 1;
        self.write_cursor(store, offset)?;
        Ok(ApplyStatus::Applied)
    }

    /// Persist the source cursor and group-commit. Rebuild after this sees
    /// applied records and the last committed offset. No-op when nothing is
    /// pending. The cursor never moves backward.
    pub fn flush(&mut self, store: &mut Store) -> Result<Option<u64>, String> {
        let Some(pending) = self.pending_max else {
            return Ok(self.last_committed);
        };
        let live = load_committed_offset(store, &self.namespace, &self.source)?.unwrap_or(0);
        let offset = self.last_committed.unwrap_or(0).max(pending).max(live);
        self.write_cursor(store, offset)?;
        store.hold_commits(false);
        if let Err(error) = store.commit() {
            store.hold_commits(true);
            return Err(error);
        }
        self.uncommitted = 0;
        self.pending_max = None;
        self.last_committed = Some(offset);
        self.last_by_id.clear();
        Ok(self.last_committed)
    }

    fn write_cursor(&self, store: &mut Store, offset: u64) -> Result<(), String> {
        let cursor = cursor_record(&self.namespace, &self.source, offset);
        store.reserve_intern(&cursor.namespace, intern_token_budget(&cursor));
        store.append_uncommitted(cursor)
    }

    fn last_applied(&self, id: &Identity) -> Option<u64> {
        match (self.last_by_id.get(id).copied(), self.last_committed) {
            (Some(applied), Some(committed)) => Some(applied.max(committed)),
            (Some(applied), None) => Some(applied),
            (None, committed) => committed,
        }
    }
}

fn require_apply_namespace(namespace: &str) -> Result<(), String> {
    if namespace.is_empty() {
        Err("namespace required".into())
    } else {
        Ok(())
    }
}

fn require_source(source: &str) -> Result<(), String> {
    if source.is_empty() {
        return Err("source required".into());
    }
    if source.contains('/') {
        return Err("source must not contain '/'".into());
    }
    Ok(())
}

fn cursor_record(namespace: &str, source: &str, offset: u64) -> ObjectRecord {
    ObjectRecord {
        gen: 0,
        namespace: namespace.to_string(),
        kind: SOURCE_KIND.to_string(),
        key: source.to_string(),
        hidden: false,
        action_id: None,
        schema_version: 1,
        props: HashMap::from([(SOURCE_OFFSET.to_string(), offset.to_string())]),
    }
}

fn load_committed_offset(
    store: &Store,
    namespace: &str,
    source: &str,
) -> Result<Option<u64>, String> {
    match store.load(namespace, SOURCE_KIND, source, &PropertyAcl::allow_all()) {
        Ok(record) => {
            if record.hidden {
                return Ok(None);
            }
            Ok(Some(offset_from_cursor(&record)?))
        }
        Err(err) if err.contains("unknown identity") || err.contains("unknown namespace") => {
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

fn offset_from_cursor(record: &ObjectRecord) -> Result<u64, String> {
    if record.kind != SOURCE_KIND {
        return Err(format!(
            "source cursor kind must be {SOURCE_KIND}, got {}",
            record.kind
        ));
    }
    if record.props.len() != 1 {
        return Err("source cursor must have only offset".into());
    }
    let raw = record
        .props
        .get(SOURCE_OFFSET)
        .ok_or_else(|| format!("source cursor missing {SOURCE_OFFSET}"))?;
    raw.parse::<u64>()
        .map_err(|_| format!("source cursor offset {raw} is invalid"))
}
