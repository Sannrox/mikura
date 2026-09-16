use crc32fast::Hasher as Crc;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Write;
use std::path::Path;

thread_local! {
    static COUNT_SCRATCH: RefCell<CountScratch> = RefCell::new(CountScratch::default());
}

#[derive(Default)]
struct CountScratch {
    paths: Vec<(u32, u32)>,
    next: Vec<(u32, u32)>,
    reachable: Vec<u64>,
}

impl CountScratch {
    fn count_and_sum(
        &mut self,
        maps: &JoinMaps,
        roots: &HashSet<u32>,
        hops: &[(&str, &str)],
        root_kind: &str,
        sum_kind: &str,
        sum_property: &str,
    ) -> (usize, i64) {
        self.paths.clear();
        self.next.clear();
        self.paths.extend(roots.iter().map(|&key| (key, key)));
        for (far_kind, join_property) in hops {
            self.next.clear();
            if let (Some(&far_id), Some(&prop_id)) = (
                maps.intern_ix.get(*far_kind),
                maps.intern_ix.get(*join_property),
            ) {
                if let Some(index) = maps.by_prop.get(&(far_id, prop_id)) {
                    for (root, parent) in &self.paths {
                        if let Some(children) = index.get(parent) {
                            for child in children {
                                self.next.push((*root, *child));
                            }
                        }
                    }
                }
            }
            std::mem::swap(&mut self.paths, &mut self.next);
        }
        let leaf_kind = hops.last().map(|(kind, _)| *kind).unwrap_or(root_kind);
        let words = maps.intern.len().div_ceil(64);
        if self.reachable.len() < words {
            self.reachable.resize(words, 0);
        } else {
            self.reachable[..words].fill(0);
        }
        let mut reachable = 0usize;
        let mut total = 0i64;
        let amounts = match (
            maps.intern_ix.get(sum_kind),
            maps.intern_ix.get(sum_property),
        ) {
            (Some(&kind_id), Some(&prop_id)) => maps.amounts.get(&(kind_id, prop_id)),
            _ => None,
        };
        for (root, leaf) in &self.paths {
            let word = (*root as usize) / 64;
            let bit = 1u64 << (*root % 64);
            if self.reachable[word] & bit == 0 {
                self.reachable[word] |= bit;
                reachable += 1;
            }
            if leaf_kind == sum_kind {
                if let Some(amount) = amounts.and_then(|map| map.get(leaf)) {
                    total += *amount;
                }
            }
        }
        (reachable, total)
    }
}

pub(crate) const JOIN_MAGIC: &[u8; 8] = b"MKJOIN02";
pub(crate) const JOIN_DELTA_MAGIC: &[u8; 8] = b"MKJOIN2D";

#[derive(Clone, Debug)]
pub(crate) struct LiveMeta {
    pub(crate) gen: u64,
    pub(crate) hidden: bool,
}

/// Rebuildable hop/join projection. Hidden records are absent.
/// Strings are interned once; hop/sum walks `u32` ids. Sidecar stores interned
/// join keys and sum columns, not a second full-string object map.
#[derive(Clone, Debug, Default)]
pub struct JoinMaps {
    pub(crate) intern: Vec<String>,
    pub(crate) intern_ix: HashMap<String, u32>,
    by_kind: HashMap<u32, HashSet<u32>>,
    by_prop: HashMap<(u32, u32), HashMap<u32, HashSet<u32>>>,
    amounts: HashMap<(u32, u32), HashMap<u32, i64>>,
    pub(crate) owned: HashMap<(u32, u32), Vec<(u32, u32)>>,
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
        COUNT_SCRATCH.with(|scratch| {
            scratch
                .borrow_mut()
                .count_and_sum(self, roots, hops, root_kind, sum_kind, sum_property)
        })
    }

    pub(crate) fn intern(&mut self, value: &str) -> u32 {
        if let Some(&id) = self.intern_ix.get(value) {
            return id;
        }
        let id = u32::try_from(self.intern.len()).expect("too many interned strings");
        self.intern.push(value.to_string());
        self.intern_ix.insert(value.to_string(), id);
        id
    }

    pub(crate) fn intern_existing(&self, value: &str) -> Option<u32> {
        self.intern_ix.get(value).copied()
    }

    pub(crate) fn insert_visible(&mut self, kind: &str, key: &str, props: HashMap<String, String>) {
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

    pub(crate) fn index(
        &mut self,
        kind: &str,
        key: &str,
        hidden: bool,
        props: HashMap<String, String>,
    ) {
        if hidden {
            return;
        }
        self.insert_visible(kind, key, props);
    }

    pub(crate) fn remove(&mut self, kind: &str, key: &str) {
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

    pub(crate) fn row_props(&self, kind: &str, key: &str) -> HashMap<String, String> {
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

pub(crate) fn write_checksummed(path: &Path, body: &[u8]) -> Result<(), String> {
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

pub(crate) fn read_checksummed(path: &Path) -> Result<Vec<u8>, String> {
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

pub(crate) struct Checkpoint {
    pub(crate) pages: u32,
    pub(crate) joins: JoinMaps,
    pub(crate) identity: HashMap<(String, String), LiveMeta>,
}
