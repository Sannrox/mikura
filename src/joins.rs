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
    fn hop_index<'a>(
        maps: &'a JoinMaps,
        far_kind: &str,
        join_property: &str,
    ) -> Option<&'a HashMap<u32, Vec<u32>>> {
        let far_id = *maps.intern_ix.get(far_kind)?;
        let prop_id = *maps.intern_ix.get(join_property)?;
        maps.by_prop.get(&(far_id, prop_id))
    }

    fn expand(&mut self, index: Option<&HashMap<u32, Vec<u32>>>) {
        self.next.clear();
        if let Some(index) = index {
            for &(root, parent) in &self.paths {
                let Some(children) = index.get(&parent) else {
                    continue;
                };
                for &child in children {
                    self.next.push((root, child));
                }
            }
        }
        std::mem::swap(&mut self.paths, &mut self.next);
    }

    fn expand_follow(
        &mut self,
        maps: &JoinMaps,
        frontier_kind: &str,
        join_property: &str,
        far_kind: &str,
    ) {
        self.next.clear();
        let Some(&kind_id) = maps.intern_ix.get(frontier_kind) else {
            std::mem::swap(&mut self.paths, &mut self.next);
            return;
        };
        let Some(&prop_id) = maps.intern_ix.get(join_property) else {
            std::mem::swap(&mut self.paths, &mut self.next);
            return;
        };
        let Some(&far_kind_id) = maps.intern_ix.get(far_kind) else {
            std::mem::swap(&mut self.paths, &mut self.next);
            return;
        };
        let visible = maps.by_kind.get(&far_kind_id);
        for &(root, parent) in &self.paths {
            let Some(owned) = maps.owned.get(&(kind_id, parent)) else {
                continue;
            };
            let Some((_, value_id)) = owned.iter().find(|(pid, _)| *pid == prop_id) else {
                continue;
            };
            if visible.is_some_and(|keys| keys.contains(value_id)) {
                self.next.push((root, *value_id));
            }
        }
        std::mem::swap(&mut self.paths, &mut self.next);
    }

    fn apply_hop(&mut self, maps: &JoinMaps, frontier_kind: &str, hop: (&str, &str, bool)) {
        if hop.2 {
            self.expand_follow(maps, frontier_kind, hop.1, hop.0);
        } else {
            self.expand(Self::hop_index(maps, hop.0, hop.1));
        }
    }

    fn fold_leaves(
        &mut self,
        maps: &JoinMaps,
        index: Option<&HashMap<u32, Vec<u32>>>,
        leaf_kind: &str,
        sum_kind: &str,
        sum_property: &str,
    ) -> (usize, i64) {
        let words = maps.intern.len().div_ceil(64);
        if self.reachable.len() < words {
            self.reachable.resize(words, 0);
        } else {
            self.reachable[..words].fill(0);
        }
        let amounts = match (
            maps.intern_ix.get(sum_kind),
            maps.intern_ix.get(sum_property),
        ) {
            (Some(&kind_id), Some(&prop_id)) => maps.amounts.get(&(kind_id, prop_id)),
            _ => None,
        };
        let sum_leaves = leaf_kind == sum_kind;
        let mut reachable = 0usize;
        let mut total = 0i64;
        if let Some(index) = index {
            for &(root, parent) in &self.paths {
                let Some(children) = index.get(&parent) else {
                    continue;
                };
                if children.is_empty() {
                    continue;
                }
                let word = root as usize / 64;
                let bit = 1u64 << (root % 64);
                if self.reachable[word] & bit == 0 {
                    self.reachable[word] |= bit;
                    reachable += 1;
                }
                if sum_leaves {
                    for child in children {
                        if let Some(amount) = amounts.and_then(|map| map.get(child)) {
                            total += *amount;
                        }
                    }
                }
            }
        }
        (reachable, total)
    }

    fn fold_follow(
        &mut self,
        maps: &JoinMaps,
        frontier_kind: &str,
        join_property: &str,
        far_kind: &str,
        sum_kind: &str,
        sum_property: &str,
    ) -> (usize, i64) {
        let words = maps.intern.len().div_ceil(64);
        if self.reachable.len() < words {
            self.reachable.resize(words, 0);
        } else {
            self.reachable[..words].fill(0);
        }
        let amounts = match (
            maps.intern_ix.get(sum_kind),
            maps.intern_ix.get(sum_property),
        ) {
            (Some(&kind_id), Some(&prop_id)) => maps.amounts.get(&(kind_id, prop_id)),
            _ => None,
        };
        let sum_leaves = far_kind == sum_kind;
        let mut reachable = 0usize;
        let mut total = 0i64;
        let Some(&kind_id) = maps.intern_ix.get(frontier_kind) else {
            return (0, 0);
        };
        let Some(&prop_id) = maps.intern_ix.get(join_property) else {
            return (0, 0);
        };
        let Some(&far_kind_id) = maps.intern_ix.get(far_kind) else {
            return (0, 0);
        };
        let visible = maps.by_kind.get(&far_kind_id);
        for &(root, parent) in &self.paths {
            let Some(owned) = maps.owned.get(&(kind_id, parent)) else {
                continue;
            };
            let Some((_, value_id)) = owned.iter().find(|(pid, _)| *pid == prop_id) else {
                continue;
            };
            if !visible.is_some_and(|keys| keys.contains(value_id)) {
                continue;
            }
            let word = root as usize / 64;
            let bit = 1u64 << (root % 64);
            if self.reachable[word] & bit == 0 {
                self.reachable[word] |= bit;
                reachable += 1;
            }
            if sum_leaves {
                if let Some(amount) = amounts.and_then(|map| map.get(value_id)) {
                    total += *amount;
                }
            }
        }
        (reachable, total)
    }

    fn count_and_sum(
        &mut self,
        maps: &JoinMaps,
        roots: &HashSet<u32>,
        hops: &[(&str, &str, bool)],
        root_kind: &str,
        sum_kind: &str,
        sum_property: &str,
    ) -> (usize, i64) {
        self.paths.clear();
        self.paths.extend(roots.iter().map(|&key| (key, key)));
        let Some((last, rest)) = hops.split_last() else {
            let amounts = match (
                maps.intern_ix.get(sum_kind),
                maps.intern_ix.get(sum_property),
            ) {
                (Some(&kind_id), Some(&prop_id)) => maps.amounts.get(&(kind_id, prop_id)),
                _ => None,
            };
            let mut total = 0i64;
            if root_kind == sum_kind {
                for &(_, leaf) in &self.paths {
                    if let Some(amount) = amounts.and_then(|map| map.get(&leaf)) {
                        total += *amount;
                    }
                }
            }
            return (self.paths.len(), total);
        };
        let mut frontier_kind = root_kind;
        for hop in rest {
            self.apply_hop(maps, frontier_kind, *hop);
            frontier_kind = hop.0;
        }
        if last.2 {
            self.fold_follow(maps, frontier_kind, last.1, last.0, sum_kind, sum_property)
        } else {
            self.fold_leaves(
                maps,
                Self::hop_index(maps, last.0, last.1),
                last.0,
                sum_kind,
                sum_property,
            )
        }
    }

    fn collect_leaves(
        &mut self,
        maps: &JoinMaps,
        roots: &HashSet<u32>,
        hops: &[(&str, &str, bool)],
        root_kind: &str,
    ) -> Vec<u32> {
        self.paths.clear();
        self.paths.extend(roots.iter().map(|&key| (key, key)));
        let mut frontier_kind = root_kind;
        for hop in hops {
            self.apply_hop(maps, frontier_kind, *hop);
            frontier_kind = hop.0;
        }
        let mut seen = HashSet::new();
        let mut leaves = Vec::new();
        for &(_, leaf) in &self.paths {
            if seen.insert(leaf) {
                leaves.push(leaf);
            }
        }
        leaves
    }
}

pub(crate) const JOIN_MAGIC: &[u8; 8] = b"MKJOIN03";
pub(crate) const JOIN_DELTA_MAGIC: &[u8; 8] = b"MKJOIN3D";
pub(crate) const ACTION_NONE: u32 = u32::MAX;

#[derive(Clone, Debug)]
pub(crate) struct LiveMeta {
    pub(crate) gen: u64,
    pub(crate) hidden: bool,
    pub(crate) action_id: Option<u32>,
}

/// Rebuildable hop/join projection. Hidden records are absent.
/// Strings are interned once; hop/sum walks `u32` ids. Join children are
/// packed identity lists. Sidecar stores interned join keys and sum columns,
/// not a second full-string object map.
#[derive(Clone, Debug, Default)]
pub struct JoinMaps {
    pub(crate) intern: Vec<String>,
    pub(crate) intern_ix: HashMap<String, u32>,
    by_kind: HashMap<u32, HashSet<u32>>,
    by_prop: HashMap<(u32, u32), HashMap<u32, Vec<u32>>>,
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

    /// Distinct roots that still have a hop path, and the sum of `sum_property`
    /// on every leaf along those paths (fan-out multiplies; diamonds do not).
    pub fn count_and_sum(
        &self,
        root_kind: &str,
        hops: &[(&str, &str, bool)],
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

    fn matching_root_ids(&self, root_kind: &str, property: &str, value: &str) -> HashSet<u32> {
        let (Some(&kind_id), Some(&prop_id), Some(&value_id)) = (
            self.intern_ix.get(root_kind),
            self.intern_ix.get(property),
            self.intern_ix.get(value),
        ) else {
            return HashSet::new();
        };
        self.by_prop
            .get(&(kind_id, prop_id))
            .and_then(|by_val| by_val.get(&value_id))
            .map(|keys| keys.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Like [`Self::count_and_sum`], but only roots matching `property == value`.
    pub fn count_and_sum_matching(
        &self,
        root_kind: &str,
        hops: &[(&str, &str, bool)],
        sum_kind: &str,
        sum_property: &str,
        property: &str,
        value: &str,
    ) -> (usize, i64) {
        let roots = self.matching_root_ids(root_kind, property, value);
        if roots.is_empty() {
            return (0, 0);
        }
        COUNT_SCRATCH.with(|scratch| {
            scratch.borrow_mut().count_and_sum(
                self,
                &roots,
                hops,
                root_kind,
                sum_kind,
                sum_property,
            )
        })
    }

    /// Distinct visible keys of the last hop's `far_kind`, or of `root_kind`
    /// when `hops` is empty. Order is intern-string sorted.
    pub fn result_keys(
        &self,
        root_kind: &str,
        hops: &[(&str, &str, bool)],
        filter: Option<(&str, &str)>,
    ) -> Vec<String> {
        let roots = match filter {
            None => {
                let Some(&root_kind_id) = self.intern_ix.get(root_kind) else {
                    return Vec::new();
                };
                match self.by_kind.get(&root_kind_id) {
                    Some(roots) => roots.clone(),
                    None => return Vec::new(),
                }
            }
            Some((property, value)) => self.matching_root_ids(root_kind, property, value),
        };
        if roots.is_empty() {
            return Vec::new();
        }
        COUNT_SCRATCH.with(|scratch| {
            let ids = scratch
                .borrow_mut()
                .collect_leaves(self, &roots, hops, root_kind);
            let mut keys: Vec<String> = ids
                .into_iter()
                .filter_map(|id| self.intern_get(id).map(str::to_string))
                .collect();
            keys.sort();
            keys
        })
    }

    pub(crate) fn intern(&mut self, value: &str) -> u32 {
        if let Some(&id) = self.intern_ix.get(value) {
            return id;
        }
        let id = u32::try_from(self.intern.len()).expect("too many interned strings");
        let owned = value.to_string();
        self.intern.push(owned.clone());
        self.intern_ix.insert(owned, id);
        id
    }

    pub(crate) fn reserve(&mut self, additional: usize) {
        self.intern.reserve(additional);
        self.intern_ix.reserve(additional);
    }

    pub(crate) fn intern_existing(&self, value: &str) -> Option<u32> {
        self.intern_ix.get(value).copied()
    }

    pub(crate) fn intern_get(&self, id: u32) -> Option<&str> {
        self.intern.get(id as usize).map(String::as_str)
    }

    pub(crate) fn insert_visible_ids(
        &mut self,
        kind_id: u32,
        key_id: u32,
        owned: Vec<(u32, u32)>,
    ) -> Result<(), String> {
        for &(prop_id, value_id) in &owned {
            if self.intern.get(prop_id as usize).is_none() {
                return Err("bad intern prop".into());
            }
            if self.intern.get(value_id as usize).is_none() {
                return Err("bad intern value".into());
            }
        }
        if self.intern.get(kind_id as usize).is_none() {
            return Err("bad intern kind".into());
        }
        if self.intern.get(key_id as usize).is_none() {
            return Err("bad intern key".into());
        }
        self.by_kind.entry(kind_id).or_default().insert(key_id);
        for &(prop_id, value_id) in &owned {
            let value = self
                .intern
                .get(value_id as usize)
                .ok_or_else(|| "bad intern value".to_string())?;
            self.by_prop
                .entry((kind_id, prop_id))
                .or_default()
                .entry(value_id)
                .or_default()
                .push(key_id);
            if let Ok(amount) = value.parse::<i64>() {
                self.amounts
                    .entry((kind_id, prop_id))
                    .or_default()
                    .insert(key_id, amount);
            }
        }
        self.owned.insert((kind_id, key_id), owned);
        Ok(())
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
                .push(key_id);
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
                    if let Some(i) = keys.iter().position(|&k| k == key_id) {
                        keys.swap_remove(i);
                    }
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
        self.row_props_omitting(kind, key, &HashSet::new())
    }

    pub(crate) fn row_props_omitting(
        &self,
        kind: &str,
        key: &str,
        denied: &HashSet<(u32, u32)>,
    ) -> HashMap<String, String> {
        let (Some(&kind_id), Some(&key_id)) = (self.intern_ix.get(kind), self.intern_ix.get(key))
        else {
            return HashMap::new();
        };
        let Some(owned) = self.owned.get(&(kind_id, key_id)) else {
            return HashMap::new();
        };
        crate::acl::PropertyAcl::omit_owned(&self.intern, kind_id, owned, denied)
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
    pub(crate) hidden_props: HashMap<(String, String), HashMap<String, String>>,
}
