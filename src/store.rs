use crc32fast::Hasher as Crc;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::actions::Action;
use crate::log::{read_records, LogWriter, SyncPolicy};

const JOIN_MAGIC: &[u8; 8] = b"MKJOIN01";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObjectRecord {
    pub gen: u64,
    pub kind: String,
    pub key: String,
    pub hidden: bool,
    pub props: HashMap<String, String>,
}

/// Rebuildable hop/join projection. Hidden records are absent.
/// Generic over kind and property name; persisted as a checksummed sidecar.
#[derive(Clone, Debug, Default)]
pub struct JoinMaps {
    records: HashMap<(String, String), HashMap<String, String>>,
    by_kind: HashMap<String, HashSet<String>>,
    by_prop: HashMap<(String, String), HashMap<String, HashSet<String>>>,
}

impl PartialEq for JoinMaps {
    fn eq(&self, other: &Self) -> bool {
        self.records == other.records
    }
}

impl Eq for JoinMaps {}

impl JoinMaps {
    pub fn is_visible(&self, kind: &str, key: &str) -> bool {
        self.records
            .contains_key(&(kind.to_string(), key.to_string()))
    }

    pub fn prop(&self, kind: &str, key: &str, property: &str) -> Option<&str> {
        self.records
            .get(&(kind.to_string(), key.to_string()))
            .and_then(|props| props.get(property))
            .map(String::as_str)
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
        let Some(roots) = self.by_kind.get(root_kind) else {
            return (0, 0);
        };
        let mut paths: Vec<(String, String)> =
            roots.iter().map(|key| (key.clone(), key.clone())).collect();
        for (far_kind, join_property) in hops {
            let index = self
                .by_prop
                .get(&((*far_kind).to_string(), (*join_property).to_string()));
            let mut next = Vec::new();
            if let Some(index) = index {
                for (root, parent) in &paths {
                    if let Some(children) = index.get(parent) {
                        for child in children {
                            next.push((root.clone(), child.clone()));
                        }
                    }
                }
            }
            paths = next;
        }
        let leaf_kind = hops.last().map(|(kind, _)| *kind).unwrap_or(root_kind);
        let mut reachable = HashSet::new();
        let mut total = 0i64;
        for (root, leaf) in &paths {
            reachable.insert(root.as_str());
            if leaf_kind == sum_kind {
                if let Some(amount) = self
                    .prop(sum_kind, leaf, sum_property)
                    .and_then(|raw| raw.parse::<i64>().ok())
                {
                    total += amount;
                }
            }
        }
        (reachable.len(), total)
    }

    fn insert_visible(&mut self, kind: &str, key: &str, props: HashMap<String, String>) {
        self.remove(kind, key);
        self.by_kind
            .entry(kind.to_string())
            .or_default()
            .insert(key.to_string());
        for (prop, value) in &props {
            self.by_prop
                .entry((kind.to_string(), prop.clone()))
                .or_default()
                .entry(value.clone())
                .or_default()
                .insert(key.to_string());
        }
        self.records
            .insert((kind.to_string(), key.to_string()), props);
    }

    fn index(&mut self, record: &ObjectRecord) {
        if record.hidden {
            return;
        }
        self.insert_visible(&record.kind, &record.key, record.props.clone());
    }

    fn remove(&mut self, kind: &str, key: &str) {
        let Some(props) = self.records.remove(&(kind.to_string(), key.to_string())) else {
            return;
        };
        if let Some(keys) = self.by_kind.get_mut(kind) {
            keys.remove(key);
            if keys.is_empty() {
                self.by_kind.remove(kind);
            }
        }
        for (prop, value) in props {
            let idx = (kind.to_string(), prop);
            if let Some(by_val) = self.by_prop.get_mut(&idx) {
                if let Some(keys) = by_val.get_mut(&value) {
                    keys.remove(key);
                    if keys.is_empty() {
                        by_val.remove(&value);
                    }
                }
                if by_val.is_empty() {
                    self.by_prop.remove(&idx);
                }
            }
        }
    }

    fn persist(&self, path: &Path, stamp: u32) -> Result<(), String> {
        let tmp = path.with_extension("joins.tmp");
        let mut body = Vec::new();
        body.extend_from_slice(JOIN_MAGIC);
        body.extend_from_slice(&stamp.to_le_bytes());
        let n = u32::try_from(self.records.len()).map_err(|_| "too many join rows".to_string())?;
        body.extend_from_slice(&n.to_le_bytes());
        let mut ids: Vec<_> = self.records.keys().cloned().collect();
        ids.sort();
        for id in ids {
            let props = self
                .records
                .get(&id)
                .ok_or_else(|| "missing join row".to_string())?;
            write_str(&mut body, &id.0)?;
            write_str(&mut body, &id.1)?;
            let pn = u32::try_from(props.len()).map_err(|_| "too many properties".to_string())?;
            body.extend_from_slice(&pn.to_le_bytes());
            let mut names: Vec<_> = props.keys().cloned().collect();
            names.sort();
            for name in names {
                write_str(&mut body, &name)?;
                let value = props
                    .get(&name)
                    .ok_or_else(|| "missing join property".to_string())?;
                write_str(&mut body, value)?;
            }
        }
        let mut hasher = Crc::new();
        hasher.update(&body);
        let crc = hasher.finalize();
        let mut file = File::create(&tmp).map_err(|e| e.to_string())?;
        file.write_all(&body).map_err(|e| e.to_string())?;
        file.write_all(&crc.to_le_bytes())
            .map_err(|e| e.to_string())?;
        file.sync_data().map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn load(path: &Path) -> Result<(u32, Self), String> {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        if bytes.len() < 8 + 4 + 4 {
            return Err("join sidecar too short".into());
        }
        let (body, crc_bytes) = bytes.split_at(bytes.len() - 4);
        let expected = u32::from_le_bytes(crc_bytes.try_into().unwrap());
        let mut hasher = Crc::new();
        hasher.update(body);
        if hasher.finalize() != expected {
            return Err("join checksum mismatch".into());
        }
        if body.len() < 12 || &body[..8] != JOIN_MAGIC {
            return Err("bad join magic".into());
        }
        let mut cur = &body[8..];
        let stamp = read_u32(&mut cur)?;
        let n = read_u32(&mut cur)? as usize;
        let mut joins = JoinMaps::default();
        for _ in 0..n {
            let kind = read_str(&mut cur)?;
            let key = read_str(&mut cur)?;
            let pn = read_u32(&mut cur)? as usize;
            let mut props = HashMap::with_capacity(pn);
            for _ in 0..pn {
                let name = read_str(&mut cur)?;
                let value = read_str(&mut cur)?;
                props.insert(name, value);
            }
            joins.insert_visible(&kind, &key, props);
        }
        if !cur.is_empty() {
            return Err("trailing join bytes".into());
        }
        Ok((stamp, joins))
    }
}

fn identity_stamp(objects: &HashMap<(String, String), ObjectRecord>) -> u32 {
    let mut ids: Vec<_> = objects.keys().cloned().collect();
    ids.sort();
    let mut hasher = Crc::new();
    for id in ids {
        let Some(record) = objects.get(&id) else {
            continue;
        };
        hasher.update(&record.gen.to_le_bytes());
        hasher.update(&[u8::from(record.hidden)]);
        write_str_to_hasher(&mut hasher, &record.kind);
        write_str_to_hasher(&mut hasher, &record.key);
        let mut props: Vec<_> = record.props.iter().collect();
        props.sort_by(|a, b| a.0.cmp(b.0));
        hasher.update(&(props.len() as u32).to_le_bytes());
        for (name, value) in props {
            write_str_to_hasher(&mut hasher, name);
            write_str_to_hasher(&mut hasher, value);
        }
    }
    hasher.finalize()
}

fn write_str_to_hasher(hasher: &mut Crc, value: &str) {
    hasher.update(&(value.len() as u16).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn write_str(body: &mut Vec<u8>, value: &str) -> Result<(), String> {
    let len = u16::try_from(value.len()).map_err(|_| "string too long".to_string())?;
    body.extend_from_slice(&len.to_le_bytes());
    body.extend_from_slice(value.as_bytes());
    Ok(())
}

fn read_u32(cur: &mut &[u8]) -> Result<u32, String> {
    Ok(u32::from_le_bytes(take::<4>(cur)?.try_into().unwrap()))
}

fn read_str(cur: &mut &[u8]) -> Result<String, String> {
    let len = u16::from_le_bytes(take::<2>(cur)?.try_into().unwrap()) as usize;
    if cur.len() < len {
        return Err("short string".into());
    }
    let (head, rest) = cur.split_at(len);
    *cur = rest;
    String::from_utf8(head.to_vec()).map_err(|_| "not utf8".into())
}

fn take<'a, const N: usize>(cur: &mut &'a [u8]) -> Result<&'a [u8], String> {
    if cur.len() < N {
        return Err("short body".into());
    }
    let (head, rest) = cur.split_at(N);
    *cur = rest;
    Ok(head)
}

pub struct Store {
    log: PathBuf,
    writer: LogWriter,
    objects: HashMap<(String, String), ObjectRecord>,
    joins: JoinMaps,
}

impl Store {
    pub fn join_map_path(log: &Path) -> PathBuf {
        let mut path = log.as_os_str().to_os_string();
        path.push(".joins");
        PathBuf::from(path)
    }

    pub fn create(log: &Path) -> Result<Self, String> {
        Self::create_with_sync(log, SyncPolicy::Group(32))
    }

    pub fn create_with_sync(log: &Path, sync: SyncPolicy) -> Result<Self, String> {
        let _ = std::fs::remove_file(Self::join_map_path(log));
        Ok(Self {
            log: log.to_path_buf(),
            writer: LogWriter::create(log, sync)?,
            objects: HashMap::new(),
            joins: JoinMaps::default(),
        })
    }

    pub fn open(log: &Path) -> Result<Self, String> {
        Self::open_with_sync(log, SyncPolicy::Group(32))
    }

    pub fn open_with_sync(log: &Path, sync: SyncPolicy) -> Result<Self, String> {
        let mut store = Self {
            log: log.to_path_buf(),
            writer: LogWriter::open(log, sync)?,
            objects: HashMap::new(),
            joins: JoinMaps::default(),
        };
        for record in read_records(log)? {
            store
                .objects
                .insert((record.kind.clone(), record.key.clone()), record);
        }
        store.install_joins()?;
        Ok(store)
    }

    pub fn apply_record(&mut self, record: ObjectRecord) {
        let id = (record.kind.clone(), record.key.clone());
        if let Some(old) = self.objects.remove(&id) {
            self.joins.remove(&old.kind, &old.key);
        }
        self.joins.index(&record);
        self.objects.insert(id, record);
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
        if let Some(existing) = self.objects.get(&id) {
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
        self.persist_joins()
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

    pub fn visible_of_kind(&self, kind: &str) -> Vec<&ObjectRecord> {
        self.objects
            .values()
            .filter(|record| record.kind == kind && !record.hidden)
            .collect()
    }

    pub fn replace_kind(&mut self, kind: &str, records: Vec<ObjectRecord>) -> Result<(), String> {
        let keep: Vec<ObjectRecord> = self
            .objects
            .values()
            .filter(|record| record.kind != kind)
            .cloned()
            .collect();
        self.writer = LogWriter::create(&self.log, SyncPolicy::Group(32))?;
        let _ = std::fs::remove_file(Self::join_map_path(&self.log));
        self.objects.clear();
        self.joins = JoinMaps::default();
        for record in keep.into_iter().chain(records) {
            self.append_uncommitted(record)?;
        }
        self.commit()
    }

    fn persist_joins(&self) -> Result<(), String> {
        self.joins.persist(
            &Self::join_map_path(&self.log),
            identity_stamp(&self.objects),
        )
    }

    fn rebuild_joins(&mut self) {
        self.joins = JoinMaps::default();
        for record in self.objects.values() {
            self.joins.index(record);
        }
    }

    fn install_joins(&mut self) -> Result<(), String> {
        let sidecar = Self::join_map_path(&self.log);
        let stamp = identity_stamp(&self.objects);
        if sidecar.exists() {
            let (loaded_stamp, joins) = JoinMaps::load(&sidecar)?;
            if loaded_stamp == stamp {
                self.joins = joins;
                return Ok(());
            }
        }
        self.rebuild_joins();
        self.persist_joins()
    }
}
