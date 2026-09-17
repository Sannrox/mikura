use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::acl::PropertyAcl;
use crate::actions::Action;
use crate::joins::{JoinMaps, LiveMeta};
use crate::log::{read_records, LogWriter, SyncPolicy};

mod sidecar;

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
        };
        store.install_projection()?;
        Ok(store)
    }

    fn apply_record(&mut self, record: ObjectRecord) {
        let id = (record.kind.clone(), record.key.clone());
        self.joins.remove(&record.kind, &record.key);
        self.joins.intern(&record.kind);
        self.joins.intern(&record.key);
        let action_id = record
            .action_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .map(|id| self.joins.intern(id));
        self.identity.insert(
            id.clone(),
            LiveMeta {
                gen: record.gen,
                hidden: record.hidden,
                action_id,
            },
        );
        if record.hidden {
            for (name, value) in &record.props {
                self.joins.intern(name);
                self.joins.intern(value);
            }
            self.hidden_props.insert(id.clone(), record.props.clone());
        } else {
            self.hidden_props.remove(&id);
        }
        self.joins.index(
            &record.kind,
            &record.key,
            record.hidden,
            record.props.clone(),
        );
        self.dirty.insert(id);
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
        if matches!(record.action_id.as_deref(), Some("")) {
            return Err("empty action id".into());
        }
        let id = (record.kind.clone(), record.key.clone());
        if let Some(existing) = self.identity.get(&id) {
            record.gen = existing.gen.max(1) + 1;
        } else if record.gen == 0 {
            record.gen = 1;
        }
        self.writer.append_record(&record)?;
        self.apply_record(record);
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
        if action.id.is_empty() {
            return Err("action id required".into());
        }
        self.append(ObjectRecord {
            gen: 0,
            kind: action.kind,
            key: action.key,
            hidden: false,
            action_id: Some(action.id),
            props: action.props,
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
            self.apply_record(record);
        }
        self.dirty.clear();
        Ok(())
    }
}
