use crc32fast::Hasher as Crc;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::actions::Action;
use crate::codec::{read_str, read_u32, read_u64, take, write_str};
use crate::log::{read_records, LogWriter, SyncPolicy};

const JOIN_MAGIC: &[u8; 8] = b"MKJOIN02";
const JOIN_DELTA_MAGIC: &[u8; 8] = b"MKJOIN2D";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObjectRecord {
    pub gen: u64,
    pub kind: String,
    pub key: String,
    pub hidden: bool,
    pub props: HashMap<String, String>,
}

#[derive(Clone, Debug)]
struct LiveMeta {
    gen: u64,
    hidden: bool,
}

/// Rebuildable hop/join projection. Hidden records are absent.
/// Strings are interned once; hop/sum walks `u32` ids. Sidecar stores interned
/// join keys and sum columns, not a second full-string object map.
#[derive(Clone, Debug, Default)]
pub struct JoinMaps {
    intern: Vec<String>,
    intern_ix: HashMap<String, u32>,
    by_kind: HashMap<u32, HashSet<u32>>,
    by_prop: HashMap<(u32, u32), HashMap<u32, HashSet<u32>>>,
    amounts: HashMap<(u32, u32), HashMap<u32, i64>>,
    owned: HashMap<(u32, u32), Vec<(u32, u32)>>,
}

impl JoinMaps {
    pub fn is_visible(&self, kind: &str, key: &str) -> bool {
        let (Some(&kind_id), Some(&key_id)) = (self.intern_ix.get(kind), self.intern_ix.get(key))
        else {
            return false;
        };
        self.by_kind
            .get(&kind_id)
            .is_some_and(|keys| keys.contains(&key_id))
    }

    pub fn prop(&self, kind: &str, key: &str, property: &str) -> Option<&str> {
        let kind_id = *self.intern_ix.get(kind)?;
        let key_id = *self.intern_ix.get(key)?;
        let prop_id = *self.intern_ix.get(property)?;
        let owned = self.owned.get(&(kind_id, key_id))?;
        for (pid, value_id) in owned {
            if *pid == prop_id {
                return self.intern.get(*value_id as usize).map(String::as_str);
            }
        }
        None
    }

    /// Distinct root keys in surviving hop paths, and the sum of `sum_property`
    /// on leaves whose kind is `sum_kind`.
    pub fn count_and_sum(
        &self,
        root_kind: &str,
        hops: &[(&str, &str)],
        sum_kind: &str,
        sum_property: &str,
    ) -> (usize, i64) {
        let Some(&root_kind_id) = self.intern_ix.get(root_kind) else {
            return (0, 0);
        };
        let Some(roots) = self.by_kind.get(&root_kind_id) else {
            return (0, 0);
        };
        let mut paths: Vec<(u32, u32)> = roots.iter().map(|&key| (key, key)).collect();
        for (far_kind, join_property) in hops {
            let mut next = Vec::new();
            if let (Some(&far_id), Some(&prop_id)) = (
                self.intern_ix.get(*far_kind),
                self.intern_ix.get(*join_property),
            ) {
                if let Some(index) = self.by_prop.get(&(far_id, prop_id)) {
                    for (root, parent) in &paths {
                        if let Some(children) = index.get(parent) {
                            for child in children {
                                next.push((*root, *child));
                            }
                        }
                    }
                }
            }
            paths = next;
        }
        let leaf_kind = hops.last().map(|(kind, _)| *kind).unwrap_or(root_kind);
        let mut reachable = HashSet::new();
        let mut total = 0i64;
        let amounts = match (
            self.intern_ix.get(sum_kind),
            self.intern_ix.get(sum_property),
        ) {
            (Some(&kind_id), Some(&prop_id)) => self.amounts.get(&(kind_id, prop_id)),
            _ => None,
        };
        for (root, leaf) in &paths {
            reachable.insert(*root);
            if leaf_kind == sum_kind {
                if let Some(amount) = amounts.and_then(|map| map.get(leaf)) {
                    total += *amount;
                }
            }
        }
        (reachable.len(), total)
    }

    fn intern(&mut self, value: &str) -> u32 {
        if let Some(&id) = self.intern_ix.get(value) {
            return id;
        }
        let id = u32::try_from(self.intern.len()).expect("too many interned strings");
        self.intern.push(value.to_string());
        self.intern_ix.insert(value.to_string(), id);
        id
    }

    fn intern_existing(&self, value: &str) -> Option<u32> {
        self.intern_ix.get(value).copied()
    }

    fn insert_visible(&mut self, kind: &str, key: &str, props: HashMap<String, String>) {
        self.remove(kind, key);
        let kind_id = self.intern(kind);
        let key_id = self.intern(key);
        self.by_kind.entry(kind_id).or_default().insert(key_id);
        let mut owned = Vec::with_capacity(props.len());
        for (prop, value) in &props {
            let prop_id = self.intern(prop);
            let value_id = self.intern(value);
            self.by_prop
                .entry((kind_id, prop_id))
                .or_default()
                .entry(value_id)
                .or_default()
                .insert(key_id);
            if let Ok(amount) = value.parse::<i64>() {
                self.amounts
                    .entry((kind_id, prop_id))
                    .or_default()
                    .insert(key_id, amount);
            }
            owned.push((prop_id, value_id));
        }
        self.owned.insert((kind_id, key_id), owned);
    }

    fn index(&mut self, record: &ObjectRecord) {
        if record.hidden {
            return;
        }
        self.insert_visible(&record.kind, &record.key, record.props.clone());
    }

    fn remove(&mut self, kind: &str, key: &str) {
        let Some(kind_id) = self.intern_existing(kind) else {
            return;
        };
        let Some(key_id) = self.intern_existing(key) else {
            return;
        };
        let Some(owned) = self.owned.remove(&(kind_id, key_id)) else {
            return;
        };
        if let Some(keys) = self.by_kind.get_mut(&kind_id) {
            keys.remove(&key_id);
            if keys.is_empty() {
                self.by_kind.remove(&kind_id);
            }
        }
        for (prop_id, value_id) in owned {
            if let Some(by_val) = self.by_prop.get_mut(&(kind_id, prop_id)) {
                if let Some(keys) = by_val.get_mut(&value_id) {
                    keys.remove(&key_id);
                    if keys.is_empty() {
                        by_val.remove(&value_id);
                    }
                }
                if by_val.is_empty() {
                    self.by_prop.remove(&(kind_id, prop_id));
                }
            }
            if let Some(amounts) = self.amounts.get_mut(&(kind_id, prop_id)) {
                amounts.remove(&key_id);
                if amounts.is_empty() {
                    self.amounts.remove(&(kind_id, prop_id));
                }
            }
        }
    }

    fn row_props(&self, kind: &str, key: &str) -> HashMap<String, String> {
        let (Some(&kind_id), Some(&key_id)) = (self.intern_ix.get(kind), self.intern_ix.get(key))
        else {
            return HashMap::new();
        };
        let Some(owned) = self.owned.get(&(kind_id, key_id)) else {
            return HashMap::new();
        };
        let mut props = HashMap::new();
        for (prop_id, value_id) in owned {
            if let (Some(prop), Some(value)) = (
                self.intern.get(*prop_id as usize),
                self.intern.get(*value_id as usize),
            ) {
                props.insert(prop.clone(), value.clone());
            }
        }
        props
    }
}

fn write_checksummed(path: &Path, body: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    let mut hasher = Crc::new();
    hasher.update(body);
    let crc = hasher.finalize();
    let mut file = File::create(&tmp).map_err(|e| e.to_string())?;
    file.write_all(body).map_err(|e| e.to_string())?;
    file.write_all(&crc.to_le_bytes())
        .map_err(|e| e.to_string())?;
    file.sync_data().map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn read_checksummed(path: &Path) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() < 12 {
        return Err("join sidecar too short".into());
    }
    let (body, crc_bytes) = bytes.split_at(bytes.len() - 4);
    let expected = u32::from_le_bytes(crc_bytes.try_into().unwrap());
    let mut hasher = Crc::new();
    hasher.update(body);
    if hasher.finalize() != expected {
        return Err("join checksum mismatch".into());
    }
    Ok(body.to_vec())
}

struct Checkpoint {
    pages: u32,
    joins: JoinMaps,
    identity: HashMap<(String, String), LiveMeta>,
}

pub struct Store {
    log: PathBuf,
    writer: LogWriter,
    identity: HashMap<(String, String), LiveMeta>,
    objects: HashMap<(String, String), ObjectRecord>,
    joins: JoinMaps,
    dirty: HashSet<(String, String)>,
    has_checkpoint: bool,
}

impl Store {
    pub fn join_map_path(log: &Path) -> PathBuf {
        let mut path = log.as_os_str().to_os_string();
        path.push(".joins");
        PathBuf::from(path)
    }

    pub fn join_delta_path(log: &Path) -> PathBuf {
        let mut path = log.as_os_str().to_os_string();
        path.push(".joins.delta");
        PathBuf::from(path)
    }

    /// Count of fully hydrated object payloads in RAM. Sidecar restart leaves
    /// this at 0; hop/sum still answers from join maps.
    pub fn hot_payloads(&self) -> usize {
        self.objects.len()
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
            objects: HashMap::new(),
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
            objects: HashMap::new(),
            joins: JoinMaps::default(),
            dirty: HashSet::new(),
            has_checkpoint: false,
        };
        store.install_projection()?;
        Ok(store)
    }

    pub fn apply_record(&mut self, record: ObjectRecord) {
        let id = (record.kind.clone(), record.key.clone());
        self.joins.remove(&record.kind, &record.key);
        self.joins.intern(&record.kind);
        self.joins.intern(&record.key);
        self.identity.insert(
            id.clone(),
            LiveMeta {
                gen: record.gen,
                hidden: record.hidden,
            },
        );
        self.joins.index(&record);
        self.dirty.insert(id);
    }

    pub fn append(&mut self, record: ObjectRecord) -> Result<(), String> {
        self.append_uncommitted(record)?;
        self.commit()
    }

    /// Buffer a record on the writer and update live maps. Durability requires
    /// [`Self::commit`]: a crash before that leaves the uncommitted tail off
    /// the rebuild (ADR 0001). `mikura-ingest` uses this for group-commit batches.
    pub fn append_uncommitted(&mut self, mut record: ObjectRecord) -> Result<(), String> {
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
        self.append(ObjectRecord {
            gen: 0,
            kind: action.kind,
            key: action.key,
            hidden: false,
            props: action.props,
        })
    }

    pub fn joins(&self) -> &JoinMaps {
        &self.joins
    }

    fn persist_projection(&mut self) -> Result<(), String> {
        let pages = self.writer.committed_pages();
        let compact =
            !self.has_checkpoint || self.dirty.len().saturating_mul(4) > self.identity.len().max(1);
        if compact {
            self.persist_checkpoint(pages)?;
            let _ = std::fs::remove_file(Self::join_delta_path(&self.log));
            self.dirty.clear();
            self.has_checkpoint = true;
            return Ok(());
        }
        self.persist_delta(pages)?;
        Ok(())
    }

    fn persist_checkpoint(&self, pages: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(JOIN_MAGIC);
        body.extend_from_slice(&pages.to_le_bytes());
        let n = u32::try_from(self.joins.intern.len())
            .map_err(|_| "too many interned strings".to_string())?;
        body.extend_from_slice(&n.to_le_bytes());
        for value in &self.joins.intern {
            write_str(&mut body, value)?;
        }
        let mut ids: Vec<_> = self.identity.keys().cloned().collect();
        ids.sort();
        let kn = u32::try_from(ids.len()).map_err(|_| "too many identities".to_string())?;
        body.extend_from_slice(&kn.to_le_bytes());
        for id in ids {
            let meta = self
                .identity
                .get(&id)
                .ok_or_else(|| "missing identity".to_string())?;
            let kind_id = self
                .joins
                .intern_existing(&id.0)
                .ok_or_else(|| "missing intern kind".to_string())?;
            let key_id = self
                .joins
                .intern_existing(&id.1)
                .ok_or_else(|| "missing intern key".to_string())?;
            body.extend_from_slice(&kind_id.to_le_bytes());
            body.extend_from_slice(&key_id.to_le_bytes());
            body.extend_from_slice(&meta.gen.to_le_bytes());
            body.push(u8::from(meta.hidden));
            let owned = if meta.hidden {
                Vec::new()
            } else {
                self.joins
                    .owned
                    .get(&(kind_id, key_id))
                    .cloned()
                    .unwrap_or_default()
            };
            let cn = u32::try_from(owned.len()).map_err(|_| "too many properties".to_string())?;
            body.extend_from_slice(&cn.to_le_bytes());
            for (prop_id, value_id) in owned {
                body.extend_from_slice(&prop_id.to_le_bytes());
                body.extend_from_slice(&value_id.to_le_bytes());
            }
        }
        write_checksummed(&Self::join_map_path(&self.log), &body)
    }

    fn persist_delta(&self, pages: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(JOIN_DELTA_MAGIC);
        body.extend_from_slice(&pages.to_le_bytes());
        let mut ids: Vec<_> = self.dirty.iter().cloned().collect();
        ids.sort();
        let n = u32::try_from(ids.len()).map_err(|_| "too many dirty rows".to_string())?;
        body.extend_from_slice(&n.to_le_bytes());
        for id in ids {
            let meta = self
                .identity
                .get(&id)
                .ok_or_else(|| "missing dirty identity".to_string())?;
            write_str(&mut body, &id.0)?;
            write_str(&mut body, &id.1)?;
            body.extend_from_slice(&meta.gen.to_le_bytes());
            body.push(u8::from(meta.hidden));
            let props = if meta.hidden {
                HashMap::new()
            } else {
                self.joins.row_props(&id.0, &id.1)
            };
            let cn = u32::try_from(props.len()).map_err(|_| "too many properties".to_string())?;
            body.extend_from_slice(&cn.to_le_bytes());
            let mut names: Vec<_> = props.keys().cloned().collect();
            names.sort();
            for name in names {
                write_str(&mut body, &name)?;
                write_str(
                    &mut body,
                    props
                        .get(&name)
                        .ok_or_else(|| "missing dirty prop".to_string())?,
                )?;
            }
        }
        write_checksummed(&Self::join_delta_path(&self.log), &body)
    }

    fn load_checkpoint(path: &Path) -> Result<Checkpoint, String> {
        let body = read_checksummed(path)?;
        if body.len() < 12 || &body[..8] != JOIN_MAGIC {
            return Err("bad join magic".into());
        }
        let mut cur = &body[8..];
        let pages = read_u32(&mut cur)?;
        let intern_n = read_u32(&mut cur)? as usize;
        let mut joins = JoinMaps::default();
        joins.intern.reserve(intern_n);
        for i in 0..intern_n {
            let value = read_str(&mut cur)?;
            joins.intern_ix.insert(value.clone(), i as u32);
            joins.intern.push(value);
        }
        let id_n = read_u32(&mut cur)? as usize;
        let mut identity = HashMap::with_capacity(id_n);
        for _ in 0..id_n {
            let kind_id = read_u32(&mut cur)?;
            let key_id = read_u32(&mut cur)?;
            let gen = read_u64(&mut cur)?;
            let hidden = take::<1>(&mut cur)?[0] != 0;
            let cn = read_u32(&mut cur)? as usize;
            let mut owned = Vec::with_capacity(cn);
            let kind = joins
                .intern
                .get(kind_id as usize)
                .ok_or_else(|| "bad intern kind".to_string())?
                .clone();
            let key = joins
                .intern
                .get(key_id as usize)
                .ok_or_else(|| "bad intern key".to_string())?
                .clone();
            for _ in 0..cn {
                let prop_id = read_u32(&mut cur)?;
                let value_id = read_u32(&mut cur)?;
                owned.push((prop_id, value_id));
            }
            identity.insert((kind.clone(), key.clone()), LiveMeta { gen, hidden });
            if !hidden {
                let mut props = HashMap::new();
                for (prop_id, value_id) in owned {
                    let prop = joins
                        .intern
                        .get(prop_id as usize)
                        .ok_or_else(|| "bad intern prop".to_string())?
                        .clone();
                    let value = joins
                        .intern
                        .get(value_id as usize)
                        .ok_or_else(|| "bad intern value".to_string())?
                        .clone();
                    props.insert(prop, value);
                }
                joins.insert_visible(&kind, &key, props);
            } else {
                let _ = joins.intern(kind.as_str());
                let _ = joins.intern(key.as_str());
            }
        }
        if !cur.is_empty() {
            return Err("trailing join bytes".into());
        }
        Ok(Checkpoint {
            pages,
            joins,
            identity,
        })
    }

    fn apply_delta(&mut self, path: &Path) -> Result<u32, String> {
        let body = read_checksummed(path)?;
        if body.len() < 12 || &body[..8] != JOIN_DELTA_MAGIC {
            return Err("bad join magic".into());
        }
        let mut cur = &body[8..];
        let pages = read_u32(&mut cur)?;
        let n = read_u32(&mut cur)? as usize;
        for _ in 0..n {
            let kind = read_str(&mut cur)?;
            let key = read_str(&mut cur)?;
            let gen = read_u64(&mut cur)?;
            let hidden = take::<1>(&mut cur)?[0] != 0;
            let cn = read_u32(&mut cur)? as usize;
            let mut props = HashMap::with_capacity(cn);
            for _ in 0..cn {
                let name = read_str(&mut cur)?;
                let value = read_str(&mut cur)?;
                props.insert(name, value);
            }
            self.joins.remove(&kind, &key);
            self.identity
                .insert((kind.clone(), key.clone()), LiveMeta { gen, hidden });
            if !hidden {
                self.joins.insert_visible(&kind, &key, props);
            } else {
                let _ = self.joins.intern(&kind);
                let _ = self.joins.intern(&key);
            }
        }
        if !cur.is_empty() {
            return Err("trailing join bytes".into());
        }
        Ok(pages)
    }

    fn replay_from_log(&mut self) -> Result<(), String> {
        self.identity.clear();
        self.objects.clear();
        self.joins = JoinMaps::default();
        self.dirty.clear();
        for record in read_records(&self.log)? {
            self.joins.remove(&record.kind, &record.key);
            self.joins.intern(&record.kind);
            self.joins.intern(&record.key);
            self.identity.insert(
                (record.kind.clone(), record.key.clone()),
                LiveMeta {
                    gen: record.gen,
                    hidden: record.hidden,
                },
            );
            self.joins.index(&record);
        }
        Ok(())
    }

    fn install_projection(&mut self) -> Result<(), String> {
        let sidecar = Self::join_map_path(&self.log);
        let delta = Self::join_delta_path(&self.log);
        let pages = self.writer.committed_pages();
        if sidecar.exists() {
            let loaded = Self::load_checkpoint(&sidecar)?;
            if loaded.pages == pages && !delta.exists() {
                self.joins = loaded.joins;
                self.identity = loaded.identity;
                self.has_checkpoint = true;
                return Ok(());
            }
            if loaded.pages <= pages {
                self.joins = loaded.joins;
                self.identity = loaded.identity;
                self.has_checkpoint = true;
                if delta.exists() {
                    let delta_pages = self.apply_delta(&delta)?;
                    if delta_pages == pages {
                        return Ok(());
                    }
                } else if loaded.pages == pages {
                    return Ok(());
                }
            }
        } else if delta.exists() {
            let _ = std::fs::remove_file(&delta);
        }
        self.replay_from_log()?;
        self.has_checkpoint = false;
        self.persist_projection()
    }
}
