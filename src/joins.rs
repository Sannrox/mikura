use crc32fast::Hasher as Crc;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::value::PropertyType;

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

    fn reset_reachable(&mut self, intern_len: usize) {
        let words = intern_len.div_ceil(64);
        if self.reachable.len() < words {
            self.reachable.resize(words, 0);
        } else {
            self.reachable[..words].fill(0);
        }
    }

    fn amounts_table<'a>(
        maps: &'a JoinMaps,
        sum_kind: &str,
        sum_property: &str,
    ) -> Option<&'a HashMap<u32, i64>> {
        match (
            maps.intern_ix.get(sum_kind),
            maps.intern_ix.get(sum_property),
        ) {
            (Some(&kind_id), Some(&prop_id)) => maps.amounts.get(&(kind_id, prop_id)),
            _ => None,
        }
    }

    fn mark_root(reachable: &mut [u64], root: u32) -> bool {
        let word = root as usize / 64;
        let bit = 1u64 << (root % 64);
        if reachable[word] & bit == 0 {
            reachable[word] |= bit;
            true
        } else {
            false
        }
    }

    fn fold_rollups(
        &mut self,
        maps: &JoinMaps,
        leaf_kind: &str,
        join_property: &str,
        sum_property: &str,
    ) -> (usize, i64) {
        self.reset_reachable(maps.intern.len());
        if let Some(rollup) = maps.measure_table(leaf_kind, join_property, sum_property) {
            return self.fold_rollup_table(rollup);
        }
        let Ok(guard) = maps.lazy_rollups.lock() else {
            return (0, 0);
        };
        let Some(kind_id) = maps.intern_ix.get(leaf_kind) else {
            return (0, 0);
        };
        let Some(join_id) = maps.intern_ix.get(join_property) else {
            return (0, 0);
        };
        let Some(sum_id) = maps.intern_ix.get(sum_property) else {
            return (0, 0);
        };
        let Some(rollup) = guard.get(&(*kind_id, *join_id, *sum_id)) else {
            return (0, 0);
        };
        self.fold_rollup_table(rollup)
    }

    fn fold_rollup_table(&mut self, rollup: &MeasureTable) -> (usize, i64) {
        let mut reachable = 0usize;
        let mut total = 0i64;
        for &(root, parent) in &self.paths {
            let Some(&(count, sum)) = rollup.get(&parent) else {
                continue;
            };
            if count == 0 {
                continue;
            }
            if Self::mark_root(&mut self.reachable, root) {
                reachable += 1;
            }
            total = total.wrapping_add(sum);
        }
        (reachable, total)
    }

    fn fold_leaves(
        &mut self,
        maps: &JoinMaps,
        index: Option<&HashMap<u32, Vec<u32>>>,
        leaf_kind: &str,
        sum_kind: &str,
        sum_property: &str,
    ) -> (usize, i64) {
        self.reset_reachable(maps.intern.len());
        let amounts = Self::amounts_table(maps, sum_kind, sum_property);
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
                if Self::mark_root(&mut self.reachable, root) {
                    reachable += 1;
                }
                if sum_leaves {
                    for child in children {
                        if let Some(amount) = amounts.and_then(|map| map.get(child)) {
                            total = total.wrapping_add(*amount);
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
        self.reset_reachable(maps.intern.len());
        let amounts = Self::amounts_table(maps, sum_kind, sum_property);
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
            if Self::mark_root(&mut self.reachable, root) {
                reachable += 1;
            }
            if sum_leaves {
                if let Some(amount) = amounts.and_then(|map| map.get(value_id)) {
                    total = total.wrapping_add(*amount);
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
            let amounts = Self::amounts_table(maps, sum_kind, sum_property);
            let mut total = 0i64;
            if root_kind == sum_kind {
                for &(_, leaf) in &self.paths {
                    if let Some(amount) = amounts.and_then(|map| map.get(&leaf)) {
                        total = total.wrapping_add(*amount);
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
        } else if last.0 == sum_kind && maps.has_last_hop_table(last.0, last.1, sum_property) {
            self.fold_rollups(maps, last.0, last.1, sum_property)
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

/// First-use lazy rollup refuses a leaf kind larger than this (#258).
/// 10^7 undeclared two-hop is in budget; a larger envelope fails closed.
pub(crate) const MAX_LAZY_MEASURE_LEAVES: usize = 20_000_000;
/// Distinct undeclared `(leaf, sum)` caches per namespace. Schema-named
/// sums do not count. Extra first-use measures fail closed.
pub(crate) const MAX_CACHED_SUMS: usize = 32;

pub(crate) const JOIN_MAGIC: &[u8; 8] = crate::JOIN_FORMAT;
/// Delta frames without per-row schema versions; still read, never written.
pub(crate) const JOIN_DELTA_MAGIC_V5: &[u8; 8] = crate::JOIN_DELTA_FORMAT_V5;
/// Delta frames that end with one schema version per row (ADR 0035).
pub(crate) const JOIN_DELTA_MAGIC: &[u8; 8] = crate::JOIN_DELTA_FORMAT;
pub(crate) const ACTION_NONE: u32 = u32::MAX;

type MeasureKey = (u32, u32, u32);
type MeasureTable = HashMap<u32, (u32, i64)>;

#[derive(Clone, Debug)]
pub(crate) struct LiveMeta {
    pub(crate) gen: u64,
    pub(crate) hidden: bool,
    pub(crate) action_id: Option<u32>,
    /// Descriptor version this live generation was written under (ADR 0035).
    pub(crate) schema_version: u32,
}

/// Rebuildable hop/join projection.
///
/// Strings are interned once; hop/sum walks `u32` ids. Join children are
/// packed identity lists. Hidden records are absent from hop/sum indexes.
/// The checkpoint stores interned owned property pairs for every identity
/// (hidden props sit on the identity row), interned Action ids, and
/// schema-named last-hop rollups (`MKJOIN06`). Hop and sum indexes are
/// derived. Undeclared sums build the same parent rollups on first
/// evaluate and keep them on apply (#258).
#[derive(Debug, Default)]
pub struct JoinMaps {
    pub(crate) intern: Vec<String>,
    pub(crate) intern_ix: HashMap<String, u32>,
    by_kind: HashMap<u32, HashSet<u32>>,
    by_prop: HashMap<(u32, u32), HashMap<u32, Vec<u32>>>,
    amounts: HashMap<(u32, u32), HashMap<u32, i64>>,
    /// Declared `(leaf_kind, sum_property)` intern ids from `mikura.schema`.
    declared_sums: HashSet<(u32, u32)>,
    /// Undeclared `(leaf_kind, sum_property)` intern ids built on first evaluate.
    cached_sums: Mutex<HashSet<(u32, u32)>>,
    /// Last-hop parent → `(count, sum)` for `(leaf_kind, join_property, sum_property)`.
    rollups: HashMap<MeasureKey, MeasureTable>,
    /// Rollups for [`Self::cached_sums`]. Evaluate is `&Store`, so first use
    /// fills this under the mutex; apply merges into it too.
    lazy_rollups: Mutex<HashMap<MeasureKey, MeasureTable>>,
    lazy_dirty: AtomicBool,
    pub(crate) owned: HashMap<(u32, u32), Vec<(u32, u32)>>,
}

impl Clone for JoinMaps {
    fn clone(&self) -> Self {
        Self {
            intern: self.intern.clone(),
            intern_ix: self.intern_ix.clone(),
            by_kind: self.by_kind.clone(),
            by_prop: self.by_prop.clone(),
            amounts: self.amounts.clone(),
            declared_sums: self.declared_sums.clone(),
            cached_sums: Mutex::new(
                self.cached_sums
                    .lock()
                    .map(|g| g.clone())
                    .unwrap_or_default(),
            ),
            rollups: self.rollups.clone(),
            lazy_rollups: Mutex::new(
                self.lazy_rollups
                    .lock()
                    .map(|g| g.clone())
                    .unwrap_or_default(),
            ),
            lazy_dirty: AtomicBool::new(self.lazy_dirty.load(Ordering::Relaxed)),
            owned: self.owned.clone(),
        }
    }
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

    /// Distinct visible roots (zero hops counts every visible root) and the
    /// sum of `sum_property` on leaves whose kind is `sum_kind`, once per path
    /// (fan-out multiplies; a diamond still counts the root once). A declared
    /// last-hop sum reads parent rollups instead of walking every leaf.
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

    pub(crate) fn matching_root_ids(
        &self,
        root_kind: &str,
        property: &str,
        value: &str,
    ) -> HashSet<u32> {
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

    /// Visible keys of `kind` whose `property` value is in `[min, max]`
    /// under `ty`. Distinct stored values are walked; missing keys stay out.
    pub(crate) fn matching_range_ids(
        &self,
        kind: &str,
        property: &str,
        ty: PropertyType,
        min: Option<&str>,
        max: Option<&str>,
    ) -> Result<HashSet<u32>, String> {
        let (Some(&kind_id), Some(&prop_id)) =
            (self.intern_ix.get(kind), self.intern_ix.get(property))
        else {
            return Ok(HashSet::new());
        };
        let Some(by_val) = self.by_prop.get(&(kind_id, prop_id)) else {
            return Ok(HashSet::new());
        };
        let mut ids = HashSet::new();
        for (&value_id, keys) in by_val {
            let Some(stored) = self.intern_get(value_id) else {
                continue;
            };
            if ty.in_range(stored, min, max)? {
                ids.extend(keys.iter().copied());
            }
        }
        Ok(ids)
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
        self.sorted_keys(self.result_ids(root_kind, hops, filter))
    }

    /// Interned ids behind [`Self::result_keys`], distinct and unordered.
    pub(crate) fn result_ids(
        &self,
        root_kind: &str,
        hops: &[(&str, &str, bool)],
        filter: Option<(&str, &str)>,
    ) -> Vec<u32> {
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
        self.result_ids_from_roots(root_kind, &roots, hops)
    }

    /// Interned key strings of `ids`, byte-sorted.
    pub(crate) fn sorted_keys(&self, ids: Vec<u32>) -> Vec<String> {
        let mut keys: Vec<String> = ids
            .into_iter()
            .filter_map(|id| self.intern_get(id).map(str::to_string))
            .collect();
        keys.sort();
        keys
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

    pub(crate) fn visible_ids(&self, kind: &str) -> HashSet<u32> {
        let Some(&kind_id) = self.intern_ix.get(kind) else {
            return HashSet::new();
        };
        self.by_kind.get(&kind_id).cloned().unwrap_or_default()
    }

    pub(crate) fn hop_targets(
        &self,
        frontier_kind: &str,
        parent: u32,
        far_kind: &str,
        join_property: &str,
        incoming: bool,
    ) -> Vec<u32> {
        if incoming {
            let Some(&kind_id) = self.intern_ix.get(frontier_kind) else {
                return Vec::new();
            };
            let Some(&prop_id) = self.intern_ix.get(join_property) else {
                return Vec::new();
            };
            let Some(&far_kind_id) = self.intern_ix.get(far_kind) else {
                return Vec::new();
            };
            let Some(owned) = self.owned.get(&(kind_id, parent)) else {
                return Vec::new();
            };
            let Some((_, value_id)) = owned.iter().find(|(pid, _)| *pid == prop_id) else {
                return Vec::new();
            };
            if self
                .by_kind
                .get(&far_kind_id)
                .is_some_and(|keys| keys.contains(value_id))
            {
                vec![*value_id]
            } else {
                Vec::new()
            }
        } else {
            let Some(&far_id) = self.intern_ix.get(far_kind) else {
                return Vec::new();
            };
            let Some(&prop_id) = self.intern_ix.get(join_property) else {
                return Vec::new();
            };
            self.by_prop
                .get(&(far_id, prop_id))
                .and_then(|by_val| by_val.get(&parent))
                .cloned()
                .unwrap_or_default()
        }
    }

    /// Frontier keys that join to `child` on `join_property`. Incoming follows
    /// the frontier's owned pair; outgoing reads the child's join value.
    pub(crate) fn join_parents(
        &self,
        frontier_kind: &str,
        far_kind: &str,
        join_property: &str,
        incoming: bool,
        child: u32,
    ) -> Vec<u32> {
        if incoming {
            let (Some(&kind_id), Some(&prop_id)) = (
                self.intern_ix.get(frontier_kind),
                self.intern_ix.get(join_property),
            ) else {
                return Vec::new();
            };
            self.by_prop
                .get(&(kind_id, prop_id))
                .and_then(|by_val| by_val.get(&child))
                .cloned()
                .unwrap_or_default()
        } else {
            let (Some(&far_id), Some(&prop_id), Some(&frontier_id)) = (
                self.intern_ix.get(far_kind),
                self.intern_ix.get(join_property),
                self.intern_ix.get(frontier_kind),
            ) else {
                return Vec::new();
            };
            let Some(owned) = self.owned.get(&(far_id, child)) else {
                return Vec::new();
            };
            let Some((_, parent_id)) = owned.iter().find(|(pid, _)| *pid == prop_id) else {
                return Vec::new();
            };
            if self
                .by_kind
                .get(&frontier_id)
                .is_some_and(|keys| keys.contains(parent_id))
            {
                vec![*parent_id]
            } else {
                Vec::new()
            }
        }
    }

    pub(crate) fn leaf_amount(&self, kind: &str, key_id: u32, property: &str) -> Option<i64> {
        let kind_id = *self.intern_ix.get(kind)?;
        let prop_id = *self.intern_ix.get(property)?;
        self.amounts.get(&(kind_id, prop_id))?.get(&key_id).copied()
    }

    pub(crate) fn count_and_sum_roots(
        &self,
        root_kind: &str,
        roots: &HashSet<u32>,
        hops: &[(&str, &str, bool)],
        sum_kind: &str,
        sum_property: &str,
    ) -> (usize, i64) {
        if roots.is_empty() {
            return (0, 0);
        }
        COUNT_SCRATCH.with(|scratch| {
            scratch
                .borrow_mut()
                .count_and_sum(self, roots, hops, root_kind, sum_kind, sum_property)
        })
    }

    /// Distinct leaf ids reached from `roots` over `hops`, unordered.
    pub(crate) fn result_ids_from_roots(
        &self,
        root_kind: &str,
        roots: &HashSet<u32>,
        hops: &[(&str, &str, bool)],
    ) -> Vec<u32> {
        if hops.is_empty() {
            return roots.iter().copied().collect();
        }
        COUNT_SCRATCH.with(|scratch| {
            scratch
                .borrow_mut()
                .collect_leaves(self, roots, hops, root_kind)
        })
    }

    /// Value id -> visible key ids of `kind_id` holding `prop_id`: the
    /// equality index that filters and hops read.
    pub(crate) fn value_index(
        &self,
        kind_id: u32,
        prop_id: u32,
    ) -> Option<&HashMap<u32, Vec<u32>>> {
        self.by_prop.get(&(kind_id, prop_id))
    }

    /// [`Self::prop`] by interned ids.
    pub(crate) fn owned_value(&self, kind_id: u32, key_id: u32, prop_id: u32) -> Option<&str> {
        let owned = self.owned.get(&(kind_id, key_id))?;
        let (_, value_id) = owned.iter().find(|(pid, _)| *pid == prop_id)?;
        self.intern_get(*value_id)
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
        let mut owned = Vec::with_capacity(props.len());
        for (prop, value) in &props {
            owned.push((self.intern(prop), self.intern(value)));
        }
        self.insert_visible_ids(kind_id, key_id, owned)
            .expect("interned ids are valid");
        self.add_leaf_contribution(kind_id, key_id);
        self.restore_parent_rollups(key_id);
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
        self.sub_leaf_contribution(kind_id, &owned);
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
        // Parent rows are keyed by interned key only. Rebuild after dropping
        // this row's indexes so a leaf that shares the key cannot restore
        // itself. Another still-visible kind keeps the parent row.
        if self.key_is_visible(key_id) {
            self.restore_parent_rollups(key_id);
        } else {
            self.drop_parent_rollups(key_id);
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

    #[cfg(test)]
    pub(crate) fn has_declared_sum(&self, leaf_kind: &str, sum_property: &str) -> bool {
        self.sum_ids(leaf_kind, sum_property)
            .is_some_and(|ids| self.declared_sums.contains(&ids))
    }

    pub(crate) fn has_measure_sum(&self, leaf_kind: &str, sum_property: &str) -> bool {
        let Some(ids) = self.sum_ids(leaf_kind, sum_property) else {
            return false;
        };
        if self.declared_sums.contains(&ids) {
            return true;
        }
        self.cached_sums
            .lock()
            .map(|g| g.contains(&ids))
            .unwrap_or(false)
    }

    /// Parent keys a declared last-hop table for `leaf_kind`.`sum_property`
    /// would hold (one entry per join-property value that is also a visible
    /// identity). Used by the tuning advisor's memory estimate.
    pub(crate) fn measure_cache_parents(&self, leaf_kind: &str, sum_property: &str) -> u64 {
        let Some(&kind_id) = self.intern_ix.get(leaf_kind) else {
            return 0;
        };
        let sum_id = self.intern_ix.get(sum_property).copied();
        let mut n = 0u64;
        for ((kind, join_id), by_val) in &self.by_prop {
            if *kind != kind_id || sum_id == Some(*join_id) {
                continue;
            }
            for (parent_id, children) in by_val {
                if children.is_empty() || !self.key_is_visible(*parent_id) {
                    continue;
                }
                n = n.saturating_add(1);
            }
        }
        n
    }

    fn sum_ids(&self, leaf_kind: &str, sum_property: &str) -> Option<(u32, u32)> {
        Some((
            *self.intern_ix.get(leaf_kind)?,
            *self.intern_ix.get(sum_property)?,
        ))
    }

    /// First undeclared evaluate builds last-hop rollups. Returns whether a
    /// new table was built. Leaves above [`MAX_LAZY_MEASURE_LEAVES`] fail closed.
    pub(crate) fn ensure_cached_sum(
        &self,
        leaf_kind: &str,
        sum_property: &str,
    ) -> Result<bool, String> {
        self.ensure_cached_sum_limited(leaf_kind, sum_property, MAX_LAZY_MEASURE_LEAVES)
    }

    pub(crate) fn ensure_cached_sum_limited(
        &self,
        leaf_kind: &str,
        sum_property: &str,
        leaf_budget: usize,
    ) -> Result<bool, String> {
        if self.has_measure_sum(leaf_kind, sum_property) {
            return Ok(false);
        }
        let Some(kind_id) = self.intern_existing(leaf_kind) else {
            return Ok(false);
        };
        let leaves = self.by_kind.get(&kind_id).map(HashSet::len).unwrap_or(0);
        if leaves > leaf_budget {
            return Err(format!(
                "undeclared measure {leaf_kind}.{sum_property} has {leaves} leaves; budget is {leaf_budget}"
            ));
        }
        let Some(prop_id) = self.intern_existing(sum_property) else {
            return Ok(false);
        };
        if !self.amounts.contains_key(&(kind_id, prop_id)) {
            return Ok(false);
        }
        {
            let mut sums = self
                .cached_sums
                .lock()
                .map_err(|_| "lazy measure lock poisoned".to_string())?;
            if sums.contains(&(kind_id, prop_id)) {
                return Ok(false);
            }
            if sums.len() >= MAX_CACHED_SUMS {
                return Err(format!("undeclared measures at cap {MAX_CACHED_SUMS}"));
            }
            sums.insert((kind_id, prop_id));
        }
        let built = self.materialize_lazy_rollups(kind_id, prop_id);
        if built.is_empty() {
            if let Ok(mut sums) = self.cached_sums.lock() {
                sums.remove(&(kind_id, prop_id));
            }
            return Ok(false);
        }
        let mut tables = self
            .lazy_rollups
            .lock()
            .map_err(|_| "lazy measure lock poisoned".to_string())?;
        for (key, parents) in built {
            tables.insert(key, parents);
        }
        self.lazy_dirty.store(true, Ordering::Relaxed);
        Ok(true)
    }

    pub(crate) fn has_unpersisted_lazy(&self) -> bool {
        self.lazy_dirty.load(Ordering::Relaxed)
    }

    /// Sidecar rollups whose `(leaf, sum)` is not schema-declared are lazy caches.
    pub(crate) fn recover_cached_sums(&mut self) {
        let mut cached = HashSet::new();
        for (leaf, _, sum) in self.rollups.keys() {
            if !self.declared_sums.contains(&(*leaf, *sum)) {
                cached.insert((*leaf, *sum));
            }
        }
        match self.cached_sums.get_mut() {
            Ok(g) => *g = cached,
            Err(p) => *p.into_inner() = cached,
        }
    }

    #[cfg(test)]
    pub(crate) fn last_hop_measure(
        &self,
        leaf_kind: &str,
        join_property: &str,
        sum_property: &str,
        parent: &str,
    ) -> Option<(u32, i64)> {
        let parent_id = self.intern_existing(parent)?;
        if let Some(table) = self.measure_table(leaf_kind, join_property, sum_property) {
            return table.get(&parent_id).copied();
        }
        let kind_id = *self.intern_ix.get(leaf_kind)?;
        let join_id = *self.intern_ix.get(join_property)?;
        let sum_id = *self.intern_ix.get(sum_property)?;
        self.lazy_rollups
            .lock()
            .ok()?
            .get(&(kind_id, join_id, sum_id))
            .and_then(|table| table.get(&parent_id).copied())
    }

    fn has_last_hop_table(&self, leaf_kind: &str, join_property: &str, sum_property: &str) -> bool {
        if self
            .measure_table(leaf_kind, join_property, sum_property)
            .is_some()
        {
            return true;
        }
        let Some(kind_id) = self.intern_ix.get(leaf_kind) else {
            return false;
        };
        let Some(join_id) = self.intern_ix.get(join_property) else {
            return false;
        };
        let Some(sum_id) = self.intern_ix.get(sum_property) else {
            return false;
        };
        self.lazy_rollups
            .lock()
            .map(|g| g.contains_key(&(*kind_id, *join_id, *sum_id)))
            .unwrap_or(false)
    }

    fn measure_table(
        &self,
        leaf_kind: &str,
        join_property: &str,
        sum_property: &str,
    ) -> Option<&HashMap<u32, (u32, i64)>> {
        let kind_id = *self.intern_ix.get(leaf_kind)?;
        let join_id = *self.intern_ix.get(join_property)?;
        let sum_id = *self.intern_ix.get(sum_property)?;
        self.rollups.get(&(kind_id, join_id, sum_id))
    }

    pub(crate) fn set_declared_sums(&mut self, leaf_kind: &str, sums: &[String]) {
        let kind_id = self.intern(leaf_kind);
        self.declared_sums.retain(|(kind, _)| *kind != kind_id);
        for name in sums {
            let prop_id = self.intern(name);
            self.declared_sums.insert((kind_id, prop_id));
            self.drop_cached_pair(kind_id, prop_id);
        }
        self.rebuild_leaf_rollups(kind_id);
    }

    pub(crate) fn clear_declared_sums(&mut self, leaf_kind: &str) {
        let Some(kind_id) = self.intern_existing(leaf_kind) else {
            return;
        };
        self.declared_sums.retain(|(kind, _)| *kind != kind_id);
        if self.leaf_sum_ids(kind_id).is_empty() {
            self.drop_cached_kind(kind_id);
            self.rollups.retain(|(kind, _, _), _| *kind != kind_id);
            return;
        }
        self.rebuild_leaf_rollups(kind_id);
    }

    pub(crate) fn adopt_declared_sums(&mut self, leaf_kind: &str, sums: &[String]) {
        let kind_id = self.intern(leaf_kind);
        self.declared_sums.retain(|(kind, _)| *kind != kind_id);
        for name in sums {
            let prop_id = self.intern(name);
            self.declared_sums.insert((kind_id, prop_id));
            self.drop_cached_pair(kind_id, prop_id);
        }
    }

    fn drop_cached_pair(&mut self, kind_id: u32, prop_id: u32) {
        if let Ok(cached) = self.cached_sums.get_mut() {
            cached.remove(&(kind_id, prop_id));
        }
        if let Ok(lazy) = self.lazy_rollups.get_mut() {
            lazy.retain(|(leaf, _, sum), _| !(*leaf == kind_id && *sum == prop_id));
        }
    }

    fn drop_cached_kind(&mut self, kind_id: u32) {
        if let Ok(cached) = self.cached_sums.get_mut() {
            cached.retain(|(kind, _)| *kind != kind_id);
        }
        if let Ok(lazy) = self.lazy_rollups.get_mut() {
            lazy.retain(|(leaf, _, _), _| *leaf != kind_id);
        }
    }

    /// Move first-use tables into [`Self::rollups`] after a sidecar write.
    pub(crate) fn adopt_lazy_rollups(&mut self) {
        let Ok(lazy) = self.lazy_rollups.get_mut() else {
            return;
        };
        for (key, table) in lazy.drain() {
            self.rollups.entry(key).or_insert(table);
        }
        self.lazy_dirty.store(false, Ordering::Relaxed);
    }

    fn key_is_visible(&self, key_id: u32) -> bool {
        self.by_kind.values().any(|keys| keys.contains(&key_id))
    }

    fn leaf_sum_ids(&self, kind_id: u32) -> Vec<u32> {
        let cached = self.cached_sums.lock().ok();
        self.declared_sums
            .iter()
            .chain(cached.iter().flat_map(|g| g.iter()))
            .filter(|(kind, _)| *kind == kind_id)
            .map(|(_, prop)| *prop)
            .collect()
    }

    fn owned_amount(intern: &[String], owned: &[(u32, u32)], sum_prop: u32) -> i64 {
        for &(prop_id, value_id) in owned {
            if prop_id == sum_prop {
                return intern
                    .get(value_id as usize)
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0);
            }
        }
        0
    }

    fn bump_rollup(&mut self, table: (u32, u32, u32), parent: u32, dcount: i32, dsum: i64) {
        let declared = self.declared_sums.contains(&(table.0, table.2));
        if declared || self.rollups.contains_key(&table) {
            Self::bump_table(&mut self.rollups, table, parent, dcount, dsum);
            return;
        }
        if let Ok(mut lazy) = self.lazy_rollups.lock() {
            Self::bump_table(&mut lazy, table, parent, dcount, dsum);
        }
    }

    fn bump_table(
        rollups: &mut HashMap<MeasureKey, MeasureTable>,
        table: MeasureKey,
        parent: u32,
        dcount: i32,
        dsum: i64,
    ) {
        let parents = rollups.entry(table).or_default();
        let entry = parents.entry(parent).or_insert((0, 0));
        let next = i64::from(entry.0) + i64::from(dcount);
        if next <= 0 {
            parents.remove(&parent);
            if parents.is_empty() {
                rollups.remove(&table);
            }
            return;
        }
        entry.0 = next as u32;
        entry.1 = entry.1.wrapping_add(dsum);
    }

    fn add_leaf_contribution(&mut self, kind_id: u32, key_id: u32) {
        let sums = self.leaf_sum_ids(kind_id);
        if sums.is_empty() {
            return;
        }
        let Some(owned) = self.owned.get(&(kind_id, key_id)).cloned() else {
            return;
        };
        for sum_prop in sums {
            let amount = Self::owned_amount(&self.intern, &owned, sum_prop);
            for &(prop_id, parent_id) in &owned {
                if prop_id == sum_prop || !self.key_is_visible(parent_id) {
                    continue;
                }
                self.bump_rollup((kind_id, prop_id, sum_prop), parent_id, 1, amount);
            }
        }
    }

    fn sub_leaf_contribution(&mut self, kind_id: u32, owned: &[(u32, u32)]) {
        let sums = self.leaf_sum_ids(kind_id);
        if sums.is_empty() {
            return;
        }
        for sum_prop in sums {
            let amount = Self::owned_amount(&self.intern, owned, sum_prop);
            for &(prop_id, parent_id) in owned {
                if prop_id == sum_prop {
                    continue;
                }
                self.bump_rollup(
                    (kind_id, prop_id, sum_prop),
                    parent_id,
                    -1,
                    amount.wrapping_neg(),
                );
            }
        }
    }

    fn drop_parent_rollups(&mut self, parent_id: u32) {
        let mut empty = Vec::new();
        for (table, parents) in &mut self.rollups {
            parents.remove(&parent_id);
            if parents.is_empty() {
                empty.push(*table);
            }
        }
        for table in empty {
            self.rollups.remove(&table);
        }
        if let Ok(lazy) = self.lazy_rollups.get_mut() {
            let mut empty = Vec::new();
            for (table, parents) in lazy.iter_mut() {
                parents.remove(&parent_id);
                if parents.is_empty() {
                    empty.push(*table);
                }
            }
            for table in empty {
                lazy.remove(&table);
            }
        }
    }

    fn restore_parent_rollups(&mut self, parent_id: u32) {
        let cached = self.cached_sums.lock().ok();
        let sums: Vec<(u32, u32)> = self
            .declared_sums
            .iter()
            .chain(cached.iter().flat_map(|g| g.iter()))
            .copied()
            .collect();
        for (leaf_id, sum_prop) in sums {
            let joins: Vec<u32> = self
                .by_prop
                .keys()
                .filter(|(kind, join)| *kind == leaf_id && *join != sum_prop)
                .map(|(_, join)| *join)
                .collect();
            for join_id in joins {
                let Some(children) = self
                    .by_prop
                    .get(&(leaf_id, join_id))
                    .and_then(|by_val| by_val.get(&parent_id))
                    .cloned()
                else {
                    continue;
                };
                if children.is_empty() {
                    continue;
                }
                let amounts = self.amounts.get(&(leaf_id, sum_prop));
                let mut count = 0u32;
                let mut total = 0i64;
                for child in children {
                    count += 1;
                    if let Some(amount) = amounts.and_then(|map| map.get(&child)) {
                        total = total.wrapping_add(*amount);
                    }
                }
                let table = (leaf_id, join_id, sum_prop);
                let into_rollups = self.declared_sums.contains(&(leaf_id, sum_prop))
                    || self.rollups.contains_key(&table);
                if into_rollups {
                    self.rollups
                        .entry(table)
                        .or_default()
                        .insert(parent_id, (count, total));
                } else if let Ok(lazy) = self.lazy_rollups.get_mut() {
                    lazy.entry(table)
                        .or_default()
                        .insert(parent_id, (count, total));
                }
            }
        }
    }

    fn materialize_lazy_rollups(
        &self,
        leaf_id: u32,
        sum_prop: u32,
    ) -> HashMap<MeasureKey, MeasureTable> {
        let mut out = HashMap::new();
        let amounts = self.amounts.get(&(leaf_id, sum_prop));
        for ((kind, join_id), by_val) in &self.by_prop {
            if *kind != leaf_id || *join_id == sum_prop {
                continue;
            }
            let mut parents = MeasureTable::new();
            for (parent_id, children) in by_val {
                if children.is_empty() || !self.key_is_visible(*parent_id) {
                    continue;
                }
                let mut total = 0i64;
                for child in children {
                    if let Some(amount) = amounts.and_then(|map| map.get(child)) {
                        total = total.wrapping_add(*amount);
                    }
                }
                parents.insert(*parent_id, (children.len() as u32, total));
            }
            if !parents.is_empty() {
                out.insert((leaf_id, *join_id, sum_prop), parents);
            }
        }
        out
    }

    fn rebuild_leaf_rollups(&mut self, leaf_id: u32) {
        self.rollups.retain(|(kind, _, _), _| *kind != leaf_id);
        if let Ok(lazy) = self.lazy_rollups.get_mut() {
            lazy.retain(|(kind, _, _), _| *kind != leaf_id);
        }
        let sums = self.leaf_sum_ids(leaf_id);
        if sums.is_empty() {
            return;
        }
        let joins: Vec<(u32, HashMap<u32, Vec<u32>>)> = self
            .by_prop
            .iter()
            .filter(|((kind, _), _)| *kind == leaf_id)
            .map(|((_, join), by_val)| (*join, by_val.clone()))
            .collect();
        for sum_prop in sums {
            let amounts = self.amounts.get(&(leaf_id, sum_prop)).cloned();
            for (join_id, by_val) in &joins {
                if *join_id == sum_prop {
                    continue;
                }
                for (parent_id, children) in by_val {
                    if children.is_empty() || !self.key_is_visible(*parent_id) {
                        continue;
                    }
                    let mut total = 0i64;
                    for child in children {
                        if let Some(amount) = amounts.as_ref().and_then(|map| map.get(child)) {
                            total = total.wrapping_add(*amount);
                        }
                    }
                    self.rollups
                        .entry((leaf_id, *join_id, sum_prop))
                        .or_default()
                        .insert(*parent_id, (children.len() as u32, total));
                }
            }
        }
    }

    /// Serialize declared and lazy tables. Does not clear [`Self::lazy_dirty`]:
    /// log checkpoints reuse this encoder, and reopen prefers the join sidecar.
    pub(crate) fn write_measures(&self, body: &mut Vec<u8>) -> Result<(), String> {
        let lazy = self.lazy_rollups.lock().ok();
        let mut tables: Vec<(MeasureKey, MeasureTable)> = self
            .rollups
            .iter()
            .map(|(key, parents)| (*key, parents.clone()))
            .collect();
        if let Some(guard) = lazy.as_ref() {
            for (key, parents) in guard.iter() {
                if !self.rollups.contains_key(key) {
                    tables.push((*key, parents.clone()));
                }
            }
        }
        tables.sort_by_key(|(key, _)| *key);
        let n = u32::try_from(tables.len()).map_err(|_| "too many measure tables".to_string())?;
        body.extend_from_slice(&n.to_le_bytes());
        for ((leaf, join, sum), parents) in &tables {
            body.extend_from_slice(&leaf.to_le_bytes());
            body.extend_from_slice(&join.to_le_bytes());
            body.extend_from_slice(&sum.to_le_bytes());
            let mut rows: Vec<_> = parents.iter().collect();
            rows.sort_by_key(|(parent, _)| *parent);
            let pn =
                u32::try_from(rows.len()).map_err(|_| "too many measure parents".to_string())?;
            body.extend_from_slice(&pn.to_le_bytes());
            for (parent, (count, total)) in rows {
                body.extend_from_slice(&parent.to_le_bytes());
                body.extend_from_slice(&count.to_le_bytes());
                body.extend_from_slice(&total.to_le_bytes());
            }
        }
        Ok(())
    }

    pub(crate) fn read_measures(&mut self, cur: &mut &[u8]) -> Result<(), String> {
        let n = crate::codec::read_u32(cur)? as usize;
        self.rollups.clear();
        for _ in 0..n {
            let leaf = crate::codec::read_u32(cur)?;
            let join = crate::codec::read_u32(cur)?;
            let sum = crate::codec::read_u32(cur)?;
            if self.intern.get(leaf as usize).is_none()
                || self.intern.get(join as usize).is_none()
                || self.intern.get(sum as usize).is_none()
            {
                return Err("bad intern measure".into());
            }
            let pn = crate::codec::read_u32(cur)? as usize;
            let mut parents = HashMap::with_capacity(pn);
            for _ in 0..pn {
                let parent = crate::codec::read_u32(cur)?;
                let count = crate::codec::read_u32(cur)?;
                let total = crate::codec::read_i64(cur)?;
                if self.intern.get(parent as usize).is_none() {
                    return Err("bad intern measure parent".into());
                }
                parents.insert(parent, (count, total));
            }
            self.rollups.insert((leaf, join, sum), parents);
        }
        Ok(())
    }
}

pub(crate) fn crc32(body: &[u8]) -> u32 {
    let mut hasher = Crc::new();
    hasher.update(body);
    hasher.finalize()
}

pub(crate) fn check_join_crc(body: &[u8], expected: u32) -> Result<(), String> {
    if crc32(body) != expected {
        Err("join checksum mismatch".into())
    } else {
        Ok(())
    }
}

fn write_body_and_crc(file: &mut impl Write, body: &[u8]) -> Result<(), String> {
    file.write_all(body).map_err(|e| e.to_string())?;
    file.write_all(&crc32(body).to_le_bytes())
        .map_err(|e| e.to_string())
}

pub(crate) fn append_checksummed(path: &Path, body: &[u8]) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    write_body_and_crc(&mut file, body)?;
    file.sync_data().map_err(|e| e.to_string())
}

pub(crate) fn write_checksummed(path: &Path, body: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    let mut file = File::create(&tmp).map_err(|e| e.to_string())?;
    write_body_and_crc(&mut file, body)?;
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
    check_join_crc(body, expected)?;
    Ok(body.to_vec())
}

pub(crate) struct Checkpoint {
    pub(crate) pages: u32,
    pub(crate) joins: HashMap<String, JoinMaps>,
    pub(crate) ns: Vec<String>,
    pub(crate) identity: HashMap<(u32, u32, u32), LiveMeta>,
    pub(crate) hidden_props: HashMap<(u32, u32, u32), Vec<(u32, u32)>>,
}

impl Checkpoint {
    fn intern_ns(&mut self, name: &str) -> u32 {
        if let Some(i) = self.ns.iter().position(|existing| existing == name) {
            return i as u32;
        }
        let id = u32::try_from(self.ns.len()).expect("namespace intern fits u32");
        self.ns.push(name.to_string());
        id
    }

    pub(crate) fn merge_live(
        &mut self,
        src_ns: Vec<String>,
        identity: HashMap<(u32, u32, u32), LiveMeta>,
        hidden_props: HashMap<(u32, u32, u32), Vec<(u32, u32)>>,
    ) {
        let remap: Vec<u32> = src_ns.iter().map(|name| self.intern_ns(name)).collect();
        for ((ns, kind, key), meta) in identity {
            self.identity.insert((remap[ns as usize], kind, key), meta);
        }
        for ((ns, kind, key), owned) in hidden_props {
            self.hidden_props
                .insert((remap[ns as usize], kind, key), owned);
        }
    }
}
