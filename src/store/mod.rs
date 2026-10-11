use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::acl::PropertyAcl;
use crate::actions::Action;
use crate::joins::{JoinMaps, LiveMeta};
use crate::log::{read_records, LogWriter, SyncPolicy};
use crate::overlay::{OverlayPatch, OVERLAY_KIND};
use crate::schema::{SchemaDescriptor, SCHEMA_KIND};

mod sidecar;

/// Rewrite `{log}.joins` and delete `{log}.joins.delta` when the dirty-set
/// file exceeds this many bytes. Compact still also runs when there is no
/// checkpoint or dirty rows exceed a quarter of identity.
pub const JOIN_DELTA_COMPACT_BYTES: u64 = 64 * 1024 * 1024;

/// Live identity: `(namespace, kind, key)`.
type LiveId = (String, String, String);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObjectRecord {
    pub gen: u64,
    pub namespace: String,
    pub kind: String,
    pub key: String,
    pub hidden: bool,
    #[serde(default)]
    pub action_id: Option<String>,
    pub props: HashMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActionCommit {
    namespace: String,
    kind: String,
    key: String,
    gen: u64,
    hidden: bool,
    /// A later hide copied this id's payload onto the same identity.
    hide_applied: bool,
    props: HashMap<String, String>,
}

enum ActionClaim {
    New,
    Replay,
}

enum ActionIdWrite {
    Claim,
    Copy,
}

pub struct Store {
    log: PathBuf,
    writer: LogWriter,
    identity: HashMap<LiveId, LiveMeta>,
    hidden_props: HashMap<LiveId, HashMap<String, String>>,
    joins: HashMap<String, JoinMaps>,
    dirty: HashSet<LiveId>,
    has_checkpoint: bool,
    delta_bytes: u64,
    delta_compact_bytes: u64,
    action_commits: HashMap<(String, String), ActionCommit>,
    action_commits_from_log: bool,
}

impl Store {
    fn sidecar_path(log: &Path, suffix: &str) -> PathBuf {
        let mut path = log.as_os_str().to_os_string();
        path.push(suffix);
        PathBuf::from(path)
    }

    pub fn join_map_path(log: &Path) -> PathBuf {
        Self::sidecar_path(log, ".joins")
    }

    pub fn join_delta_path(log: &Path) -> PathBuf {
        Self::sidecar_path(log, ".joins.delta")
    }

    pub fn create(log: &Path) -> Result<Self, String> {
        Self::create_with_sync(log, SyncPolicy::Group(32))
    }

    pub fn create_with_sync(log: &Path, sync: SyncPolicy) -> Result<Self, String> {
        let _ = std::fs::remove_file(Self::join_map_path(log));
        let _ = std::fs::remove_file(Self::join_delta_path(log));
        Ok(Self::empty(
            log.to_path_buf(),
            LogWriter::create(log, sync)?,
        ))
    }

    pub fn open(log: &Path) -> Result<Self, String> {
        Self::open_with_sync(log, SyncPolicy::Group(32))
    }

    pub fn open_with_sync(log: &Path, sync: SyncPolicy) -> Result<Self, String> {
        let mut store = Self::empty(log.to_path_buf(), LogWriter::open(log, sync)?);
        store.install_projection()?;
        Ok(store)
    }

    fn empty(log: PathBuf, writer: LogWriter) -> Self {
        Self {
            log,
            writer,
            identity: HashMap::new(),
            hidden_props: HashMap::new(),
            joins: HashMap::new(),
            dirty: HashSet::new(),
            has_checkpoint: false,
            delta_bytes: 0,
            delta_compact_bytes: JOIN_DELTA_COMPACT_BYTES,
            action_commits: HashMap::new(),
            action_commits_from_log: true,
        }
    }

    /// Use a tiny compact bound in tests so a few dirty-set frames can exceed
    /// it without writing `JOIN_DELTA_COMPACT_BYTES` of delta.
    #[cfg(test)]
    pub(crate) fn set_join_delta_compact_bytes(&mut self, bytes: u64) {
        self.delta_compact_bytes = bytes;
    }

    fn install_live(&mut self, id: LiveId, meta: LiveMeta, props: HashMap<String, String>) {
        {
            let joins = self.joins_for_write(&id.0);
            joins.remove(&id.1, &id.2);
            joins.intern(&id.1);
            joins.intern(&id.2);
        }
        let hidden = meta.hidden;
        self.identity.insert(id.clone(), meta);
        if hidden {
            let joins = self.joins_for_write(&id.0);
            for (name, value) in &props {
                joins.intern(name);
                joins.intern(value);
            }
            self.hidden_props.insert(id, props);
        } else {
            self.hidden_props.remove(&id);
            self.joins_for_write(&id.0)
                .index(&id.1, &id.2, false, props);
        }
    }

    fn remember_action(&mut self, record: &ObjectRecord) {
        let Some(id) = record.action_id.as_deref().filter(|id| !id.is_empty()) else {
            return;
        };
        let commit = self
            .action_commits
            .entry((record.namespace.clone(), id.to_string()))
            .or_insert_with(|| ActionCommit {
                namespace: record.namespace.clone(),
                kind: record.kind.clone(),
                key: record.key.clone(),
                gen: record.gen,
                hidden: record.hidden,
                hide_applied: false,
                props: record.props.clone(),
            });
        if record.hidden
            && !commit.hidden
            && commit.namespace == record.namespace
            && commit.kind == record.kind
            && commit.key == record.key
            && commit.props == record.props
        {
            commit.hide_applied = true;
        }
    }

    fn ensure_action_commits(&mut self) -> Result<(), String> {
        if self.action_commits_from_log {
            return Ok(());
        }
        let since_open = std::mem::take(&mut self.action_commits);
        for record in read_records(&self.log)? {
            self.remember_action(&record);
        }
        for (id, commit) in since_open {
            self.action_commits
                .entry(id)
                .and_modify(|logged| logged.hide_applied |= commit.hide_applied)
                .or_insert(commit);
        }
        self.action_commits_from_log = true;
        Ok(())
    }

    fn replay_overlay(
        &self,
        committed: ActionCommit,
        patch: &OverlayPatch,
        action_id: &str,
    ) -> Result<(), String> {
        if committed.kind != OVERLAY_KIND {
            return Err(format!(
                "action id {action_id} already committed on {}/{}",
                committed.kind, committed.key
            ));
        }
        let stored = OverlayPatch::from_record(&ObjectRecord {
            gen: committed.gen,
            namespace: committed.namespace,
            kind: committed.kind,
            key: committed.key,
            hidden: committed.hidden,
            action_id: Some(action_id.to_string()),
            props: committed.props,
        })?;
        if stored.namespace != patch.namespace
            || stored.kind != patch.kind
            || stored.key != patch.key
        {
            return Err(format!(
                "action id {action_id} already committed on {}/{}",
                stored.kind, stored.key
            ));
        }
        let mut stored_cleared = stored.cleared.clone();
        let mut patch_cleared = patch.cleared.clone();
        stored_cleared.sort();
        patch_cleared.sort();
        if stored.props != patch.props || stored_cleared != patch_cleared {
            return Err(format!("action id {action_id} body conflict"));
        }
        Ok(())
    }

    fn require_expected_gen(
        &self,
        namespace: &str,
        kind: &str,
        key: &str,
        expected_gen: Option<u64>,
    ) -> Result<(), String> {
        let Some(expected) = expected_gen else {
            return Ok(());
        };
        let id = live_id(namespace, kind, key);
        match self.identity.get(&id) {
            Some(meta) if meta.gen != expected => Err(format!(
                "stale generation: expected {expected}, live {}",
                meta.gen
            )),
            None if expected != 0 => {
                Err(format!("stale generation: expected {expected}, live none"))
            }
            _ => Ok(()),
        }
    }

    fn apply_record(&mut self, record: ObjectRecord) {
        self.remember_action(&record);
        let id = live_id(&record.namespace, &record.kind, &record.key);
        let action_id = record
            .action_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .map(|id| self.joins_for_write(&record.namespace).intern(id));
        self.install_live(
            id.clone(),
            LiveMeta {
                gen: record.gen,
                hidden: record.hidden,
                action_id,
            },
            record.props,
        );
        self.dirty.insert(id);
    }

    fn require_action_id(id: &str, empty: &'static str) -> Result<(), String> {
        if id.is_empty() {
            Err(empty.into())
        } else {
            Ok(())
        }
    }

    /// One clerk-assigned Action id is the retry key on every write path
    /// (ADR 0011, ADR 0019, ADR 0025). Matching body is a no-op. A remapped
    /// identity or different body fails closed.
    fn action_claim(
        &mut self,
        namespace: &str,
        action_id: &str,
        kind: &str,
        key: &str,
        hidden: bool,
        props: &HashMap<String, String>,
    ) -> Result<ActionClaim, String> {
        self.ensure_action_commits()?;
        let Some(committed) = self
            .action_commits
            .get(&(namespace.to_string(), action_id.to_string()))
            .cloned()
        else {
            return Ok(ActionClaim::New);
        };
        if committed.namespace != namespace || committed.kind != kind || committed.key != key {
            return Err(format!(
                "action id {action_id} already committed on {}/{}/{}",
                committed.namespace, committed.kind, committed.key
            ));
        }
        if committed.props != *props {
            return Err(format!("action id {action_id} body conflict"));
        }
        if hidden {
            // A hide under a claimed id copies the claimed payload; the first
            // one appends, a retry after it is a replay.
            return Ok(if committed.hidden || committed.hide_applied {
                ActionClaim::Replay
            } else {
                ActionClaim::New
            });
        }
        if committed.hidden {
            return Err(format!("action id {action_id} body conflict"));
        }
        if let Some(schema) = self.schema(namespace, kind)? {
            schema.validate(&ObjectRecord {
                gen: 0,
                namespace: namespace.to_string(),
                kind: kind.to_string(),
                key: key.to_string(),
                hidden: false,
                action_id: Some(action_id.to_string()),
                props: props.clone(),
            })?;
        }
        Ok(ActionClaim::Replay)
    }

    fn load_reserved<T>(
        &self,
        namespace: &str,
        kind: &str,
        key: &str,
        decode: fn(&ObjectRecord) -> Result<T, String>,
    ) -> Result<Option<T>, String> {
        if !self.joins.contains_key(namespace) {
            return Ok(None);
        }
        let id = live_id(namespace, kind, key);
        let Some(meta) = self.identity.get(&id) else {
            return Ok(None);
        };
        if meta.hidden {
            return Ok(None);
        }
        let record = self.load(namespace, kind, key, &PropertyAcl::allow_all())?;
        decode(&record).map(Some)
    }

    /// Live object for `(namespace, kind, key)` after ingest or [`Store::open`].
    ///
    /// Missing or unknown namespace fails closed. Missing identity fails
    /// closed. Hidden records return the hidden payload and stay out of join
    /// maps. Denied properties are omitted; remaining keys keep stored
    /// values. The object log remains authority; interned sidecar props are
    /// a deletable projection.
    pub fn load(
        &self,
        namespace: &str,
        kind: &str,
        key: &str,
        acl: &PropertyAcl,
    ) -> Result<ObjectRecord, String> {
        let namespace = self.require_known_namespace(namespace)?;
        if !acl.object_visible(kind, key) {
            return Err(format!("unknown identity {namespace}/{kind}/{key}"));
        }
        let id = live_id(namespace, kind, key);
        let meta = self
            .identity
            .get(&id)
            .ok_or_else(|| format!("unknown identity {namespace}/{kind}/{key}"))?;
        let maps = self.joins_in(namespace)?;
        let denied = acl.interned_denies(|token| maps.intern_existing(token));
        let props = if meta.hidden {
            self.hidden_props_omitting(maps, kind, &id, &denied)
        } else {
            maps.row_props_omitting(kind, key, &denied)
        };
        Ok(ObjectRecord {
            gen: meta.gen,
            namespace: namespace.to_string(),
            kind: kind.to_string(),
            key: key.to_string(),
            hidden: meta.hidden,
            action_id: match meta.action_id {
                None => None,
                Some(intern) => Some(
                    maps.intern_get(intern)
                        .ok_or_else(|| "missing intern action".to_string())?
                        .to_string(),
                ),
            },
            props,
        })
    }

    /// Last visible `mikura.schema` row for `kind` in `namespace`, or `None`
    /// when absent or hidden. Missing or unknown namespace fails closed.
    pub fn schema(&self, namespace: &str, kind: &str) -> Result<Option<SchemaDescriptor>, String> {
        self.require_known_namespace(namespace)?;
        self.load_reserved(namespace, SCHEMA_KIND, kind, SchemaDescriptor::from_record)
    }

    /// Last visible `mikura.overlay` row for `(namespace, kind, key)`, or
    /// `None` when absent or hidden. Missing or unknown namespace fails
    /// closed.
    pub fn overlay(
        &self,
        namespace: &str,
        kind: &str,
        key: &str,
    ) -> Result<Option<OverlayPatch>, String> {
        self.require_known_namespace(namespace)?;
        self.load_reserved(
            namespace,
            OVERLAY_KIND,
            &OverlayPatch::identity_key(kind, key),
            OverlayPatch::from_record,
        )
    }

    /// Admit a property overlay and rematerialize a visible instance.
    ///
    /// The Action id is a retry key (ADR 0019). The same id and the same
    /// patch is a replay and does not consult `expected_gen`. A different
    /// patch or another identity fails closed. `expected_gen` is the live
    /// instance generation the clerk read on a **new** id. Hidden instances
    /// stay hidden; recreate does not apply a prior overlay.
    pub fn apply_overlay(
        &mut self,
        patch: OverlayPatch,
        action_id: String,
        expected_gen: Option<u64>,
    ) -> Result<(), String> {
        self.apply_overlay_in_view(patch, action_id, expected_gen, &PropertyAcl::allow_all())
    }

    /// [`Self::apply_overlay`] honoring a request restriction (ADR 0014).
    pub fn apply_overlay_in_view(
        &mut self,
        mut patch: OverlayPatch,
        action_id: String,
        expected_gen: Option<u64>,
        acl: &PropertyAcl,
    ) -> Result<(), String> {
        Self::require_action_id(&action_id, "action id required")?;
        self.require_known_namespace(&patch.namespace)?;
        acl.require_visible(&patch.kind, &patch.key)
            .map_err(|err| err.to_string())?;
        for property in patch.props.keys() {
            acl.check(&patch.kind, property)
                .map_err(|err| err.to_string())?;
        }
        for property in &patch.cleared {
            acl.check(&patch.kind, property)
                .map_err(|err| err.to_string())?;
        }
        self.ensure_action_commits()?;
        if let Some(committed) = self
            .action_commits
            .get(&(patch.namespace.clone(), action_id.clone()))
            .cloned()
        {
            return self.replay_overlay(committed, &patch, &action_id);
        }
        let id = live_id(&patch.namespace, &patch.kind, &patch.key);
        self.require_expected_gen(&patch.namespace, &patch.kind, &patch.key, expected_gen)?;
        patch.action_id = Some(action_id.clone());
        let instance_hidden = self.identity.get(&id).is_some_and(|meta| meta.hidden);
        let instance_exists = self.identity.contains_key(&id);
        self.append_uncommitted(patch.to_record()?)?;
        if !instance_hidden {
            let source = if instance_exists {
                self.load(
                    &patch.namespace,
                    &patch.kind,
                    &patch.key,
                    &PropertyAcl::allow_all(),
                )?
                .props
            } else {
                HashMap::new()
            };
            let props = patch.apply(source);
            self.write_uncommitted(
                ObjectRecord {
                    gen: 0,
                    namespace: patch.namespace,
                    kind: patch.kind,
                    key: patch.key,
                    hidden: false,
                    action_id: Some(action_id),
                    props,
                },
                true,
                ActionIdWrite::Copy,
            )?;
        }
        self.commit()
    }

    /// [`Self::load`] then fail closed if the record violates `schema`.
    ///
    /// A deny list that omits a required property fails closed. Historical
    /// rows written before a descriptor still return from [`Self::load`].
    pub fn load_with_schema(
        &self,
        namespace: &str,
        kind: &str,
        key: &str,
        acl: &PropertyAcl,
        schema: &SchemaDescriptor,
    ) -> Result<ObjectRecord, String> {
        let record = self.load(namespace, kind, key, acl)?;
        schema.validate(&record)?;
        Ok(record)
    }

    pub fn append(&mut self, record: ObjectRecord) -> Result<(), String> {
        self.append_uncommitted(record)?;
        self.commit()
    }

    /// Reserve intern capacity before a batch of new kind, key, property,
    /// value, or Action-id tokens. Unknown namespaces are left alone so a
    /// reserve cannot admit an empty namespace.
    pub fn reserve_intern(&mut self, namespace: &str, additional: usize) {
        if let Some(joins) = self.joins.get_mut(namespace) {
            joins.reserve(additional);
        }
    }

    /// Buffer a record on the writer and update live maps. Durability requires
    /// [`Self::commit`]: a crash before that leaves the uncommitted tail off
    /// the rebuild (ADR 0001). Stream ingest uses this; a batch that must be all or
    /// nothing uses [`Self::append_batch`] (ADR 0026).
    ///
    /// A supplied Action id is unique in the namespace (ADR 0025, ADR 0028).
    /// The same id and the same body (`namespace`, `kind`, `key`, `hidden`,
    /// `props`) is a no-op. A remapped identity or a different body fails closed. A hidden record under a
    /// claimed id must copy the claimed `props`; its retry is a no-op. Source
    /// ingest may omit the id. Hide and overlay rematerialize copy provenance
    /// and do not claim a new id.
    ///
    /// Visible source writes of a non-hidden identity merge a visible
    /// `mikura.overlay` and validate a visible `mikura.schema`. A visible
    /// `mikura.schema` replacement is checked against the previous visible
    /// descriptor (ADR 0013). Hide of the instance also hides that overlay.
    /// [`Self::apply_action`] uses [`Self::append_replace`] so it stays a
    /// whole-record replace.
    pub fn append_uncommitted(&mut self, record: ObjectRecord) -> Result<(), String> {
        self.write_uncommitted(record, true, ActionIdWrite::Claim)
    }

    /// Buffer a whole-record replace. Overlay merge is skipped; schema
    /// validation still runs on a visible instance.
    pub(crate) fn append_replace(&mut self, record: ObjectRecord) -> Result<(), String> {
        self.write_uncommitted(record, false, ActionIdWrite::Claim)
    }

    fn write_uncommitted(
        &mut self,
        mut record: ObjectRecord,
        merge_overlay: bool,
        action_id_write: ActionIdWrite,
    ) -> Result<(), String> {
        let claim_id = match action_id_write {
            ActionIdWrite::Claim => record.action_id.clone(),
            ActionIdWrite::Copy => None,
        };
        if let Some(id) = claim_id.as_deref().or(record.action_id.as_deref()) {
            Self::require_action_id(id, "empty action id")?;
        }
        require_namespace(&record.namespace)?;
        if record.kind == SCHEMA_KIND {
            let next = SchemaDescriptor::from_record(&record)?;
            if !record.hidden {
                if let Some(previous) = self.load_reserved(
                    &record.namespace,
                    SCHEMA_KIND,
                    &record.key,
                    SchemaDescriptor::from_record,
                )? {
                    previous.check_replacement(&next)?;
                }
            }
        } else if record.kind == OVERLAY_KIND {
            OverlayPatch::from_record(&record)?;
        } else if !record.hidden {
            let was_hidden = self
                .identity
                .get(&live_id(&record.namespace, &record.kind, &record.key))
                .is_some_and(|meta| meta.hidden);
            if !was_hidden && merge_overlay {
                if let Some(overlay) = self.load_reserved(
                    &record.namespace,
                    OVERLAY_KIND,
                    &OverlayPatch::identity_key(&record.kind, &record.key),
                    OverlayPatch::from_record,
                )? {
                    record.props = overlay.apply(std::mem::take(&mut record.props));
                    if record.action_id.is_none() {
                        record.action_id = overlay.action_id;
                    }
                }
            }
            if let Some(schema) = self.load_reserved(
                &record.namespace,
                SCHEMA_KIND,
                &record.kind,
                SchemaDescriptor::from_record,
            )? {
                schema.canonicalize_instance(&mut record)?;
                schema.validate(&record)?;
            }
        }
        if let Some(id) = claim_id.as_deref() {
            if matches!(
                self.action_claim(
                    &record.namespace,
                    id,
                    &record.kind,
                    &record.key,
                    record.hidden,
                    &record.props
                )?,
                ActionClaim::Replay
            ) {
                return Ok(());
            }
        }
        let hide_overlay =
            record.hidden && record.kind != OVERLAY_KIND && record.kind != SCHEMA_KIND;
        let overlay_target = if hide_overlay {
            Some((
                record.namespace.clone(),
                record.kind.clone(),
                record.key.clone(),
            ))
        } else {
            None
        };
        let id = live_id(&record.namespace, &record.kind, &record.key);
        if let Some(existing) = self.identity.get(&id) {
            record.gen = existing.gen.max(1) + 1;
        } else if record.gen == 0 {
            record.gen = 1;
        }
        self.writer.append_record(&record)?;
        let schema_namespace = record.namespace.clone();
        let schema_kind = record.kind.clone();
        let schema_key = record.key.clone();
        self.apply_record(record);
        self.maybe_refresh_schema(&schema_namespace, &schema_kind, &schema_key)?;
        if let Some((namespace, kind, key)) = overlay_target {
            if self.overlay(&namespace, &kind, &key)?.is_some() {
                let mut hide = OverlayPatch {
                    namespace,
                    kind,
                    key,
                    props: HashMap::new(),
                    cleared: Vec::new(),
                    action_id: None,
                }
                .to_record()?;
                hide.hidden = true;
                self.append_uncommitted(hide)?;
            }
        }
        Ok(())
    }

    /// Group-commit the current writer pages and persist the sidecar.
    pub fn commit(&mut self) -> Result<(), String> {
        self.writer.flush()?;
        self.persist_projection()
    }

    /// Append `records` and commit them as one unit (ADR 0026).
    ///
    /// An error from admitting a record, or from writing the log commit,
    /// drops the whole batch: no record of it stays live or becomes durable,
    /// and the store equals its committed log again. Group commit is held
    /// until the batch ends, so a large batch is never partly durable.
    /// Records already buffered by [`Self::append_uncommitted`] are committed
    /// first, so the abort never discards work the caller did not hand in.
    ///
    /// A failed superblock write leaves the commit point unknown: the store
    /// then keeps the batch as is, refuses further writes, and must be
    /// reopened, because rolling back could delete pages the log references.
    /// A rollback that itself fails has the same effect, so a half-rebuilt
    /// projection is never committed as a checkpoint.
    ///
    /// Once the log commit succeeded the batch is durable. A failure to
    /// persist the derived sidecar after that is reported as such and must not
    /// be retried as a failed batch; the next open rebuilds the sidecar.
    pub fn append_batch(&mut self, records: Vec<ObjectRecord>) -> Result<(), String> {
        if self.writer.has_pending() {
            self.commit()?;
        }
        self.writer.hold_commits(true);
        let appended = records
            .into_iter()
            .try_for_each(|record| self.append_uncommitted(record));
        self.writer.hold_commits(false);
        let committed = appended.and_then(|()| self.writer.flush());
        if let Err(error) = committed {
            if self.writer.is_failed() {
                return Err(error);
            }
            return Err(match self.abort_uncommitted() {
                Ok(()) => error,
                Err(abort) => {
                    self.writer.poison();
                    format!("{error}; rollback failed, reopen the store: {abort}")
                }
            });
        }
        self.persist_projection().map_err(|error| {
            format!("batch committed to the log; projection sidecar not persisted: {error}")
        })
    }

    /// Drop every uncommitted record and rebuild the live projection from the
    /// committed log, as a reopen after a crash would (ADR 0001).
    fn abort_uncommitted(&mut self) -> Result<(), String> {
        self.writer.discard_pending()?;
        self.identity.clear();
        self.hidden_props.clear();
        self.joins.clear();
        self.dirty.clear();
        self.has_checkpoint = false;
        self.delta_bytes = 0;
        self.action_commits.clear();
        self.action_commits_from_log = true;
        self.install_projection()
    }

    /// Writer `fsync` count. Used by ingest tests to characterize group commit.
    pub fn log_fsync_count(&self) -> u64 {
        self.writer.fsync_count()
    }

    /// Whole-record replace with a clerk Action id (ADR 0006).
    ///
    /// A repeated id with the same body (`namespace`, `kind`, `key`,
    /// `hidden = false`, `props`) is a replay: no append (ADR 0011). The
    /// same id with a different body or on another identity fails closed. A
    /// replay whose body is illegal under the current descriptor also fails
    /// closed (ADR 0013). Ids are unique per namespace; lookup rebuilds from
    /// the log. `expected_gen` still applies to a new id. A legal replay that
    /// already committed does not append.
    pub fn apply_action(
        &mut self,
        action: Action,
        expected_gen: Option<u64>,
    ) -> Result<(), String> {
        self.apply_action_in_view(action, expected_gen, &PropertyAcl::allow_all())
    }

    /// [`Self::apply_action`] honoring a request restriction (ADR 0014).
    pub fn apply_action_in_view(
        &mut self,
        action: Action,
        expected_gen: Option<u64>,
        acl: &PropertyAcl,
    ) -> Result<(), String> {
        Self::require_action_id(&action.id, "action id required")?;
        self.require_known_namespace(&action.namespace)?;
        acl.require_visible(&action.kind, &action.key)
            .map_err(|err| err.to_string())?;
        for property in action.props.keys() {
            acl.check(&action.kind, property)
                .map_err(|err| err.to_string())?;
        }
        match self.action_claim(
            &action.namespace,
            &action.id,
            &action.kind,
            &action.key,
            false,
            &action.props,
        )? {
            ActionClaim::Replay => return Ok(()),
            ActionClaim::New => {}
        }
        self.require_expected_gen(&action.namespace, &action.kind, &action.key, expected_gen)?;
        self.append_replace(ObjectRecord {
            gen: 0,
            namespace: action.namespace,
            kind: action.kind,
            key: action.key,
            hidden: false,
            action_id: Some(action.id),
            props: action.props,
        })?;
        self.commit()
    }

    /// Hide `(namespace, kind, key)` from evaluate. [`Self::load`] still
    /// returns the last payload. Missing identity or namespace fails closed.
    /// Denied properties this hide would copy fail closed. Overlay rows stay
    /// on the log under the existing instance-hide rule.
    pub fn hide(
        &mut self,
        namespace: &str,
        kind: &str,
        key: &str,
        acl: &PropertyAcl,
    ) -> Result<(), String> {
        self.require_known_namespace(namespace)?;
        acl.require_visible(kind, key)
            .map_err(|err| err.to_string())?;
        let current = self.load(namespace, kind, key, &PropertyAcl::allow_all())?;
        for property in current.props.keys() {
            acl.check(kind, property)
                .map_err(|err| format!("{err:?}"))?;
        }
        self.write_uncommitted(
            ObjectRecord {
                gen: 0,
                namespace: current.namespace,
                kind: current.kind,
                key: current.key,
                hidden: true,
                action_id: current.action_id,
                props: current.props,
            },
            true,
            ActionIdWrite::Copy,
        )?;
        self.commit()
    }

    /// Join maps for a known namespace. Missing or unknown namespace fails
    /// closed.
    pub fn joins(&self, namespace: &str) -> Result<&JoinMaps, String> {
        self.joins_in(self.require_known_namespace(namespace)?)
    }

    /// Superblock `committed_pages`. Snapshot cursors bind this value
    /// together with the uncommitted writer head.
    pub fn committed_pages(&self) -> u32 {
        self.writer.committed_pages()
    }

    /// `(committed_pages, written_pages, current page used)`. Cursors bind
    /// this so an uncommitted stream append invalidates the snapshot.
    pub fn snapshot_stamp(&self) -> (u32, u32, u32) {
        (
            self.writer.committed_pages(),
            self.writer.written_pages(),
            self.writer.page_used() as u32,
        )
    }

    fn hidden_props_omitting(
        &self,
        maps: &JoinMaps,
        kind: &str,
        id: &LiveId,
        denied: &HashSet<(u32, u32)>,
    ) -> HashMap<String, String> {
        let Some(props) = self.hidden_props.get(id) else {
            return HashMap::new();
        };
        if denied.is_empty() {
            return props.clone();
        }
        let Some(kind_id) = maps.intern_existing(kind) else {
            return props.clone();
        };
        let mut allowed = HashMap::with_capacity(props.len());
        for (name, value) in props {
            if let Some(prop_id) = maps.intern_existing(name) {
                if denied.contains(&(kind_id, prop_id)) {
                    continue;
                }
            }
            allowed.insert(name.clone(), value.clone());
        }
        allowed
    }

    fn replay_from_log(&mut self) -> Result<(), String> {
        self.identity.clear();
        self.hidden_props.clear();
        self.joins.clear();
        self.dirty.clear();
        self.action_commits.clear();
        for record in read_records(&self.log)? {
            let schema_namespace = record.namespace.clone();
            let schema_kind = record.kind.clone();
            let schema_key = record.key.clone();
            self.apply_record(record);
            self.maybe_refresh_schema(&schema_namespace, &schema_kind, &schema_key)?;
        }
        self.dirty.clear();
        self.action_commits_from_log = true;
        Ok(())
    }

    fn maybe_refresh_schema(
        &mut self,
        namespace: &str,
        kind: &str,
        key: &str,
    ) -> Result<(), String> {
        if kind == SCHEMA_KIND {
            self.refresh_leaf_measures(namespace, key)?;
        }
        Ok(())
    }

    fn refresh_leaf_measures(&mut self, namespace: &str, kind: &str) -> Result<(), String> {
        let schema = self.schema(namespace, kind)?;
        match schema {
            Some(schema) => self
                .joins_for_write(namespace)
                .set_declared_sums(&schema.kind, &schema.sums),
            None => {
                if let Some(joins) = self.joins.get_mut(namespace) {
                    joins.clear_declared_sums(kind);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn adopt_declared_measures(&mut self) -> Result<(), String> {
        let mut rows: Vec<(String, String)> = self
            .identity
            .iter()
            .filter(|((_namespace, kind, _), meta)| kind == SCHEMA_KIND && !meta.hidden)
            .map(|((namespace, _, key), _)| (namespace.clone(), key.clone()))
            .collect();
        rows.sort();
        for (namespace, kind) in rows {
            let schema = self.schema(&namespace, &kind)?;
            match schema {
                Some(schema) => self
                    .joins_for_write(&namespace)
                    .adopt_declared_sums(&schema.kind, &schema.sums),
                None => {
                    if let Some(joins) = self.joins.get_mut(&namespace) {
                        joins.clear_declared_sums(&kind);
                    }
                }
            }
        }
        Ok(())
    }

    fn joins_for_write(&mut self, namespace: &str) -> &mut JoinMaps {
        self.joins.entry(namespace.to_string()).or_default()
    }

    fn joins_in(&self, namespace: &str) -> Result<&JoinMaps, String> {
        self.joins
            .get(namespace)
            .ok_or_else(|| format!("unknown namespace {namespace}"))
    }

    fn require_known_namespace<'a>(&self, namespace: &'a str) -> Result<&'a str, String> {
        let namespace = require_namespace(namespace)?;
        if self.joins.contains_key(namespace) {
            Ok(namespace)
        } else {
            Err(format!("unknown namespace {namespace}"))
        }
    }
}

fn live_id(namespace: &str, kind: &str, key: &str) -> LiveId {
    (namespace.to_string(), kind.to_string(), key.to_string())
}

fn require_namespace(namespace: &str) -> Result<&str, String> {
    if namespace.is_empty() {
        Err("namespace required".into())
    } else {
        Ok(namespace)
    }
}
