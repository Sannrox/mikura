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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObjectRecord {
    pub gen: u64,
    pub kind: String,
    pub key: String,
    pub hidden: bool,
    #[serde(default)]
    pub action_id: Option<String>,
    pub props: HashMap<String, String>,
}

pub struct Store {
    log: PathBuf,
    writer: LogWriter,
    identity: HashMap<(String, String), LiveMeta>,
    hidden_props: HashMap<(String, String), HashMap<String, String>>,
    joins: JoinMaps,
    dirty: HashSet<(String, String)>,
    has_checkpoint: bool,
    delta_bytes: u64,
    delta_compact_bytes: u64,
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
        Ok(Self {
            log: log.to_path_buf(),
            writer: LogWriter::create(log, sync)?,
            identity: HashMap::new(),
            hidden_props: HashMap::new(),
            joins: JoinMaps::default(),
            dirty: HashSet::new(),
            has_checkpoint: false,
            delta_bytes: 0,
            delta_compact_bytes: JOIN_DELTA_COMPACT_BYTES,
        })
    }

    pub fn open(log: &Path) -> Result<Self, String> {
        Self::open_with_sync(log, SyncPolicy::Group(32))
    }

    pub fn open_with_sync(log: &Path, sync: SyncPolicy) -> Result<Self, String> {
        let mut store = Self {
            log: log.to_path_buf(),
            writer: LogWriter::open(log, sync)?,
            identity: HashMap::new(),
            hidden_props: HashMap::new(),
            joins: JoinMaps::default(),
            dirty: HashSet::new(),
            has_checkpoint: false,
            delta_bytes: 0,
            delta_compact_bytes: JOIN_DELTA_COMPACT_BYTES,
        };
        store.install_projection()?;
        Ok(store)
    }

    /// Use a tiny compact bound in tests so a few dirty-set frames can exceed
    /// it without writing `JOIN_DELTA_COMPACT_BYTES` of delta.
    #[cfg(test)]
    pub(crate) fn set_join_delta_compact_bytes(&mut self, bytes: u64) {
        self.delta_compact_bytes = bytes;
    }

    fn install_live(
        &mut self,
        kind: String,
        key: String,
        gen: u64,
        hidden: bool,
        action_id: Option<u32>,
        props: HashMap<String, String>,
    ) {
        self.joins.remove(&kind, &key);
        self.joins.intern(&kind);
        self.joins.intern(&key);
        let id = (kind.clone(), key.clone());
        self.identity.insert(
            id.clone(),
            LiveMeta {
                gen,
                hidden,
                action_id,
            },
        );
        if hidden {
            for (name, value) in &props {
                self.joins.intern(name);
                self.joins.intern(value);
            }
            self.hidden_props.insert(id, props);
        } else {
            self.hidden_props.remove(&id);
            self.joins.index(&kind, &key, false, props);
        }
    }

    fn apply_record(&mut self, record: ObjectRecord) {
        let id = (record.kind.clone(), record.key.clone());
        let action_id = record
            .action_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .map(|id| self.joins.intern(id));
        self.install_live(
            record.kind,
            record.key,
            record.gen,
            record.hidden,
            action_id,
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

    fn load_reserved<T>(
        &self,
        kind: &str,
        key: &str,
        decode: fn(&ObjectRecord) -> Result<T, String>,
    ) -> Result<Option<T>, String> {
        let id = (kind.to_string(), key.to_string());
        let Some(meta) = self.identity.get(&id) else {
            return Ok(None);
        };
        if meta.hidden {
            return Ok(None);
        }
        let record = self.load(kind, key, &PropertyAcl::allow_all())?;
        decode(&record).map(Some)
    }

    /// Live object for `(kind, key)` after ingest or [`Store::open`].
    ///
    /// Missing identity fails closed. Hidden records return the hidden
    /// payload and stay out of join maps. Denied properties are omitted;
    /// remaining keys keep stored values. The object log remains authority;
    /// interned sidecar props are a deletable projection.
    pub fn load(&self, kind: &str, key: &str, acl: &PropertyAcl) -> Result<ObjectRecord, String> {
        let id = (kind.to_string(), key.to_string());
        let meta = self
            .identity
            .get(&id)
            .ok_or_else(|| format!("unknown identity {kind}/{key}"))?;
        let denied = acl.interned_denies(|token| self.joins.intern_existing(token));
        let props = if meta.hidden {
            self.hidden_props_omitting(kind, &id, &denied)
        } else {
            self.joins.row_props_omitting(kind, key, &denied)
        };
        Ok(ObjectRecord {
            gen: meta.gen,
            kind: kind.to_string(),
            key: key.to_string(),
            hidden: meta.hidden,
            action_id: match meta.action_id {
                None => None,
                Some(intern) => Some(
                    self.joins
                        .intern_get(intern)
                        .ok_or_else(|| "missing intern action".to_string())?
                        .to_string(),
                ),
            },
            props,
        })
    }

    /// Last visible `mikura.schema` row for `kind`, or `None` when absent or hidden.
    pub fn schema(&self, kind: &str) -> Result<Option<SchemaDescriptor>, String> {
        self.load_reserved(SCHEMA_KIND, kind, SchemaDescriptor::from_record)
    }

    /// Last visible `mikura.overlay` row for `(kind, key)`, or `None` when absent or hidden.
    pub fn overlay(&self, kind: &str, key: &str) -> Result<Option<OverlayPatch>, String> {
        self.load_reserved(
            OVERLAY_KIND,
            &OverlayPatch::identity_key(kind, key),
            OverlayPatch::from_record,
        )
    }

    /// Admit a property overlay and rematerialize a visible instance.
    ///
    /// `expected_gen` is the live instance generation the clerk read. Mismatch
    /// fails closed. Hidden instances stay hidden; recreate does not apply a
    /// prior overlay.
    pub fn apply_overlay(
        &mut self,
        mut patch: OverlayPatch,
        action_id: String,
        expected_gen: Option<u64>,
    ) -> Result<(), String> {
        Self::require_action_id(&action_id, "action id required")?;
        let id = (patch.kind.clone(), patch.key.clone());
        if let Some(expected) = expected_gen {
            match self.identity.get(&id) {
                Some(meta) if meta.gen != expected => {
                    return Err(format!(
                        "stale generation: expected {expected}, live {}",
                        meta.gen
                    ));
                }
                None if expected != 0 => {
                    return Err(format!("stale generation: expected {expected}, live none"));
                }
                _ => {}
            }
        }
        patch.action_id = Some(action_id.clone());
        let instance_hidden = self.identity.get(&id).is_some_and(|meta| meta.hidden);
        let instance_exists = self.identity.contains_key(&id);
        self.append_uncommitted(patch.to_record()?)?;
        if !instance_hidden {
            let source = if instance_exists {
                self.load(&patch.kind, &patch.key, &PropertyAcl::allow_all())?
                    .props
            } else {
                HashMap::new()
            };
            let props = patch.apply(source);
            self.append_uncommitted(ObjectRecord {
                gen: 0,
                kind: patch.kind,
                key: patch.key,
                hidden: false,
                action_id: Some(action_id),
                props,
            })?;
        }
        self.commit()
    }

    /// [`Self::load`] then fail closed if the record violates `schema`.
    ///
    /// A deny list that omits a required property fails closed. Historical
    /// rows written before a descriptor still return from [`Self::load`].
    pub fn load_with_schema(
        &self,
        kind: &str,
        key: &str,
        acl: &PropertyAcl,
        schema: &SchemaDescriptor,
    ) -> Result<ObjectRecord, String> {
        let record = self.load(kind, key, acl)?;
        schema.validate(&record)?;
        Ok(record)
    }

    pub fn append(&mut self, record: ObjectRecord) -> Result<(), String> {
        self.append_uncommitted(record)?;
        self.commit()
    }

    /// Reserve intern capacity before a batch of new property or value tokens.
    pub fn reserve_intern(&mut self, additional: usize) {
        self.joins.reserve(additional);
    }

    /// Buffer a record on the writer and update live maps. Durability requires
    /// [`Self::commit`]: a crash before that leaves the uncommitted tail off
    /// the rebuild (ADR 0001). `mikura-ingest` uses this for group-commit batches.
    pub fn append_uncommitted(&mut self, mut record: ObjectRecord) -> Result<(), String> {
        if let Some(id) = record.action_id.as_deref() {
            Self::require_action_id(id, "empty action id")?;
        }
        if record.kind == SCHEMA_KIND {
            SchemaDescriptor::from_record(&record)?;
        } else if record.kind == OVERLAY_KIND {
            OverlayPatch::from_record(&record)?;
        } else if !record.hidden {
            let was_hidden = self
                .identity
                .get(&(record.kind.clone(), record.key.clone()))
                .is_some_and(|meta| meta.hidden);
            if !was_hidden {
                if let Some(overlay) = self.overlay(&record.kind, &record.key)? {
                    record.props = overlay.apply(std::mem::take(&mut record.props));
                    if record.action_id.is_none() {
                        record.action_id = overlay.action_id;
                    }
                }
            }
            if let Some(schema) = self.schema(&record.kind)? {
                schema.validate(&record)?;
            }
        }
        let hide_overlay =
            record.hidden && record.kind != OVERLAY_KIND && record.kind != SCHEMA_KIND;
        let overlay_target = if hide_overlay {
            Some((record.kind.clone(), record.key.clone()))
        } else {
            None
        };
        let id = (record.kind.clone(), record.key.clone());
        if let Some(existing) = self.identity.get(&id) {
            record.gen = existing.gen.max(1) + 1;
        } else if record.gen == 0 {
            record.gen = 1;
        }
        self.writer.append_record(&record)?;
        let schema_kind = record.kind.clone();
        let schema_key = record.key.clone();
        self.apply_record(record);
        self.maybe_refresh_schema(&schema_kind, &schema_key)?;
        if let Some((kind, key)) = overlay_target {
            if self.overlay(&kind, &key)?.is_some() {
                let mut hide = OverlayPatch {
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

    /// Group-commit the current writer pages and persist join maps.
    pub fn commit(&mut self) -> Result<(), String> {
        self.writer.flush()?;
        self.persist_projection()
    }

    /// Writer `fsync` count. Used by ingest tests to characterize group commit.
    pub fn log_fsync_count(&self) -> u64 {
        self.writer.fsync_count()
    }

    pub fn apply_action(&mut self, action: Action) -> Result<(), String> {
        Self::require_action_id(&action.id, "action id required")?;
        self.append(ObjectRecord {
            gen: 0,
            kind: action.kind,
            key: action.key,
            hidden: false,
            action_id: Some(action.id),
            props: action.props,
        })
    }

    /// Hide `(kind, key)` from evaluate. [`Self::load`] still returns the last
    /// payload. Missing identity fails closed. Denied properties this hide
    /// would copy fail closed. Overlay rows stay on the log under the
    /// existing instance-hide rule.
    pub fn hide(&mut self, kind: &str, key: &str, acl: &PropertyAcl) -> Result<(), String> {
        let current = self.load(kind, key, &PropertyAcl::allow_all())?;
        for property in current.props.keys() {
            acl.check(kind, property)
                .map_err(|err| format!("{err:?}"))?;
        }
        self.append(ObjectRecord {
            gen: 0,
            kind: current.kind,
            key: current.key,
            hidden: true,
            action_id: current.action_id,
            props: current.props,
        })
    }

    pub fn joins(&self) -> &JoinMaps {
        &self.joins
    }

    fn hidden_props_omitting(
        &self,
        kind: &str,
        id: &(String, String),
        denied: &HashSet<(u32, u32)>,
    ) -> HashMap<String, String> {
        let Some(props) = self.hidden_props.get(id) else {
            return HashMap::new();
        };
        if denied.is_empty() {
            return props.clone();
        }
        let Some(kind_id) = self.joins.intern_existing(kind) else {
            return props.clone();
        };
        let mut allowed = HashMap::with_capacity(props.len());
        for (name, value) in props {
            if let Some(prop_id) = self.joins.intern_existing(name) {
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
        self.joins = JoinMaps::default();
        self.dirty.clear();
        for record in read_records(&self.log)? {
            let schema_kind = record.kind.clone();
            let schema_key = record.key.clone();
            self.apply_record(record);
            self.maybe_refresh_schema(&schema_kind, &schema_key)?;
        }
        self.dirty.clear();
        Ok(())
    }

    fn maybe_refresh_schema(&mut self, kind: &str, key: &str) -> Result<(), String> {
        if kind == SCHEMA_KIND {
            self.refresh_leaf_measures(key)?;
        }
        Ok(())
    }

    fn refresh_leaf_measures(&mut self, kind: &str) -> Result<(), String> {
        match self.schema(kind)? {
            Some(schema) => self.joins.set_declared_sums(&schema.kind, &schema.sums),
            None => self.joins.clear_declared_sums(kind),
        }
        Ok(())
    }

    pub(crate) fn adopt_declared_measures(&mut self) -> Result<(), String> {
        let mut kinds: Vec<String> = self
            .identity
            .iter()
            .filter(|((kind, _), meta)| kind == SCHEMA_KIND && !meta.hidden)
            .map(|((_, key), _)| key.clone())
            .collect();
        kinds.sort();
        for kind in kinds {
            match self.schema(&kind)? {
                Some(schema) => self.joins.adopt_declared_sums(&schema.kind, &schema.sums),
                None => self.joins.clear_declared_sums(&kind),
            }
        }
        Ok(())
    }
}
