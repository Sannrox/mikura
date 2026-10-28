use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use crate::acl::AclError;
use crate::joins::JoinMaps;
use crate::objectset::{
    Aggregate, Breakdown, EvaluateRequest, EvaluateResponse, Group, Hop, Predicate, Sort,
    MAX_GROUPS,
};
use crate::page::{decode_cursor, encode_cursor};
use crate::store::{ObjectRecord, Store};
use crate::value::{PropertyType, PropertyValue};

fn hop_triples(request: &EvaluateRequest) -> Vec<(&str, &str, bool)> {
    request
        .hops
        .iter()
        .map(|hop| {
            (
                hop.far_kind.as_str(),
                hop.join_property.as_str(),
                hop.incoming,
            )
        })
        .collect()
}

fn result_kind(request: &EvaluateRequest) -> &str {
    request
        .hops
        .last()
        .map(|hop| hop.far_kind.as_str())
        .unwrap_or(request.root_kind.as_str())
}

fn has_hop_predicate(request: &EvaluateRequest) -> bool {
    request.hops.iter().any(|hop| hop.predicate.is_some())
}

fn query_prop<'a>(
    store: &Store,
    maps: &'a JoinMaps,
    namespace: &str,
    kind: &str,
    key: &str,
    property: &str,
) -> Option<&'a str> {
    if store.omits_current_property(namespace, kind, property) {
        None
    } else {
        maps.prop(kind, key, property)
    }
}

fn hop_join_owner<'a>(frontier: &'a str, hop: &'a Hop) -> &'a str {
    if hop.incoming {
        frontier
    } else {
        hop.far_kind.as_str()
    }
}

fn hop_uses_dropped_join(store: &Store, request: &EvaluateRequest) -> bool {
    let mut frontier = request.root_kind.as_str();
    for hop in &request.hops {
        if store.omits_current_property(
            &request.namespace,
            hop_join_owner(frontier, hop),
            &hop.join_property,
        ) {
            return true;
        }
        frontier = hop.far_kind.as_str();
    }
    false
}

fn property_type(
    store: &Store,
    namespace: &str,
    kind: &str,
    property: &str,
) -> Result<PropertyType, ComputeError> {
    match store.schema(namespace, kind) {
        Ok(Some(schema)) => Ok(schema
            .property_type(property)
            .unwrap_or(PropertyType::String)),
        Ok(None) => Ok(PropertyType::String),
        Err(err) => Err(ComputeError::Load(err)),
    }
}

fn check_predicate_shape(pred: &Predicate) -> Result<(), ComputeError> {
    match pred {
        Predicate::Eq { property, .. }
        | Predicate::Neq { property, .. }
        | Predicate::Range { property, .. }
        | Predicate::Missing { property }
        | Predicate::Bbox { property, .. } => {
            if property.is_empty() {
                return Err(ComputeError::Predicate(
                    "predicate property must be non-empty".into(),
                ));
            }
            Ok(())
        }
        Predicate::And(args) | Predicate::Or(args) => {
            if args.is_empty() {
                return Err(ComputeError::Predicate(
                    "boolean predicate requires at least one argument".into(),
                ));
            }
            for arg in args {
                check_predicate_shape(arg)?;
            }
            Ok(())
        }
        Predicate::Not(inner) => check_predicate_shape(inner),
    }
}

fn parse_predicate_value(
    store: &Store,
    namespace: &str,
    kind: &str,
    property: &str,
    raw: &str,
) -> Result<(), ComputeError> {
    let ty = property_type(store, namespace, kind, property)?;
    ty.parse_canonical(raw).map_err(ComputeError::Predicate)?;
    Ok(())
}

fn check_predicate_types(
    store: &Store,
    namespace: &str,
    kind: &str,
    pred: &Predicate,
) -> Result<(), ComputeError> {
    check_predicate_shape(pred)?;
    match pred {
        Predicate::Eq { property, value } | Predicate::Neq { property, value } => {
            parse_predicate_value(store, namespace, kind, property, value)
        }
        Predicate::Range { property, min, max } => {
            let ty = property_type(store, namespace, kind, property)?;
            match ty {
                PropertyType::Boolean
                | PropertyType::Vector { .. }
                | PropertyType::GeoPoint
                | PropertyType::GeoShape
                | PropertyType::Media => {
                    return Err(ComputeError::Predicate(format!(
                        "range is not defined for {} {kind}.{property}",
                        ty.token()
                    )));
                }
                PropertyType::TimeSeries => {
                    let ts = PropertyType::Timestamp;
                    if let Some(bound) = min {
                        ts.parse_canonical(bound).map_err(ComputeError::Predicate)?;
                    }
                    if let Some(bound) = max {
                        ts.parse_canonical(bound).map_err(ComputeError::Predicate)?;
                    }
                    return Ok(());
                }
                PropertyType::String
                | PropertyType::Integer
                | PropertyType::Timestamp
                | PropertyType::Decimal { .. } => {}
            }
            if let Some(bound) = min {
                ty.parse_canonical(bound).map_err(ComputeError::Predicate)?;
            }
            if let Some(bound) = max {
                ty.parse_canonical(bound).map_err(ComputeError::Predicate)?;
            }
            Ok(())
        }
        Predicate::Bbox {
            property,
            min_lat,
            min_lon,
            max_lat,
            max_lon,
        } => {
            let ty = property_type(store, namespace, kind, property)?;
            ty.in_bbox("", min_lat, min_lon, max_lat, max_lon)
                .map(|_| ())
                .map_err(ComputeError::Predicate)
        }
        Predicate::Missing { .. } => Ok(()),
        Predicate::And(args) | Predicate::Or(args) => {
            for arg in args {
                check_predicate_types(store, namespace, kind, arg)?;
            }
            Ok(())
        }
        Predicate::Not(inner) => check_predicate_types(store, namespace, kind, inner),
    }
}

fn check_predicate_acl(
    request: &EvaluateRequest,
    kind: &str,
    pred: &Predicate,
) -> Result<(), ComputeError> {
    for property in pred.properties() {
        request
            .acl
            .check(kind, property)
            .map_err(ComputeError::Acl)?;
    }
    Ok(())
}

fn stored_eq(ty: PropertyType, stored: &str, expected: &str) -> bool {
    match (ty.parse_canonical(stored), ty.parse_canonical(expected)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn matches_predicate(
    store: &Store,
    namespace: &str,
    kind: &str,
    key_id: u32,
    pred: &Predicate,
) -> Result<bool, ComputeError> {
    let maps = store.joins(namespace).map_err(ComputeError::Load)?;
    let Some(key) = maps.intern_get(key_id) else {
        return Ok(false);
    };
    match pred {
        Predicate::Eq { property, value } => {
            let Some(stored) = query_prop(store, maps, namespace, kind, key, property) else {
                return Ok(false);
            };
            let ty = property_type(store, namespace, kind, property)?;
            Ok(stored_eq(ty, stored, value))
        }
        Predicate::Neq { property, value } => {
            let Some(stored) = query_prop(store, maps, namespace, kind, key, property) else {
                return Ok(false);
            };
            let ty = property_type(store, namespace, kind, property)?;
            Ok(!stored_eq(ty, stored, value))
        }
        Predicate::Range { property, min, max } => {
            let Some(stored) = query_prop(store, maps, namespace, kind, key, property) else {
                return Ok(false);
            };
            let ty = property_type(store, namespace, kind, property)?;
            ty.in_range(stored, min.as_deref(), max.as_deref())
                .map_err(ComputeError::Predicate)
        }
        Predicate::Missing { property } => {
            Ok(query_prop(store, maps, namespace, kind, key, property).is_none())
        }
        Predicate::Bbox {
            property,
            min_lat,
            min_lon,
            max_lat,
            max_lon,
        } => {
            let Some(stored) = query_prop(store, maps, namespace, kind, key, property) else {
                return Ok(false);
            };
            let ty = property_type(store, namespace, kind, property)?;
            ty.in_bbox(stored, min_lat, min_lon, max_lat, max_lon)
                .map_err(ComputeError::Predicate)
        }
        Predicate::And(args) => {
            for arg in args {
                if !matches_predicate(store, namespace, kind, key_id, arg)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Predicate::Or(args) => {
            for arg in args {
                if matches_predicate(store, namespace, kind, key_id, arg)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Predicate::Not(inner) => Ok(!matches_predicate(store, namespace, kind, key_id, inner)?),
    }
}

fn filter_ids(
    store: &Store,
    namespace: &str,
    kind: &str,
    ids: HashSet<u32>,
    pred: &Predicate,
) -> Result<HashSet<u32>, ComputeError> {
    let mut kept = HashSet::new();
    for id in ids {
        if matches_predicate(store, namespace, kind, id, pred)? {
            kept.insert(id);
        }
    }
    Ok(kept)
}

fn declared_indexes(
    store: &Store,
    namespace: &str,
    kind: &str,
) -> Result<HashSet<String>, ComputeError> {
    match store.schema(namespace, kind) {
        Ok(Some(schema)) => Ok(schema.indexes.iter().cloned().collect()),
        Ok(None) => Ok(HashSet::new()),
        Err(err) => Err(ComputeError::Load(err)),
    }
}

/// Eq/range/and/or on schema-named indexes. Other operators stay a scan so
/// undeclared names stay observable.
fn pred_is_index_tree(pred: &Predicate, indexed: &HashSet<String>) -> bool {
    match pred {
        Predicate::Eq { property, .. } | Predicate::Range { property, .. } => {
            indexed.contains(property.as_str())
        }
        Predicate::And(args) | Predicate::Or(args) => {
            !args.is_empty() && args.iter().all(|arg| pred_is_index_tree(arg, indexed))
        }
        _ => false,
    }
}

fn index_ids(
    store: &Store,
    maps: &JoinMaps,
    namespace: &str,
    kind: &str,
    pred: &Predicate,
) -> Result<HashSet<u32>, ComputeError> {
    match pred {
        Predicate::Eq { property, value } => Ok(maps.matching_root_ids(kind, property, value)),
        Predicate::Range { property, min, max } => {
            let ty = property_type(store, namespace, kind, property)?;
            maps.matching_range_ids(kind, property, ty, min.as_deref(), max.as_deref())
                .map_err(ComputeError::Predicate)
        }
        Predicate::And(args) => {
            let mut acc: Option<HashSet<u32>> = None;
            for arg in args {
                let next = index_ids(store, maps, namespace, kind, arg)?;
                acc = Some(match acc {
                    None => next,
                    Some(cur) => cur.intersection(&next).copied().collect(),
                });
            }
            Ok(acc.unwrap_or_default())
        }
        Predicate::Or(args) => {
            let mut acc = HashSet::new();
            for arg in args {
                acc.extend(index_ids(store, maps, namespace, kind, arg)?);
            }
            Ok(acc)
        }
        _ => filter_ids(store, namespace, kind, maps.visible_ids(kind), pred),
    }
}

fn root_ids(store: &Store, request: &EvaluateRequest) -> Result<HashSet<u32>, ComputeError> {
    if request.filter.is_some() && request.predicate.is_some() {
        return Err(ComputeError::Predicate(
            "evaluate accepts filter or predicate, not both".into(),
        ));
    }
    let maps = store
        .joins(&request.namespace)
        .map_err(ComputeError::Load)?;
    if let Some(pred) = &request.predicate {
        let indexed = declared_indexes(store, &request.namespace, &request.root_kind)?;
        if pred_is_index_tree(pred, &indexed) {
            let ids = index_ids(store, maps, &request.namespace, &request.root_kind, pred)?;
            return Ok(visible_in_view(store, request, &request.root_kind, ids));
        }
    }
    let ids = if let Some(filter) = &request.filter {
        if store.omits_current_property(&request.namespace, &request.root_kind, &filter.property) {
            HashSet::new()
        } else {
            maps.matching_root_ids(&request.root_kind, &filter.property, &filter.value)
        }
    } else {
        maps.visible_ids(&request.root_kind)
    };
    let ids = visible_in_view(store, request, &request.root_kind, ids);
    if let Some(pred) = &request.predicate {
        filter_ids(store, &request.namespace, &request.root_kind, ids, pred)
    } else {
        Ok(ids)
    }
}

fn visible_in_view(
    store: &Store,
    request: &EvaluateRequest,
    kind: &str,
    ids: HashSet<u32>,
) -> HashSet<u32> {
    if !request.acl.hides_objects() {
        return ids;
    }
    let Ok(maps) = store.joins(&request.namespace) else {
        return HashSet::new();
    };
    ids.into_iter()
        .filter(|&id| {
            maps.intern_get(id).is_some_and(|key| {
                request.acl.object_visible(kind, key, |property| {
                    query_prop(store, maps, &request.namespace, kind, key, property)
                })
            })
        })
        .collect()
}

/// Interned `(root, leaf)` ids of one hop path.
type PathIds = (u32, u32);

/// Paths that survive every hop, its visibility, and its predicate, with the
/// kind the last hop reached.
fn surviving_paths<'r>(
    store: &Store,
    request: &'r EvaluateRequest,
    roots: &HashSet<u32>,
) -> Result<(Vec<PathIds>, &'r str), ComputeError> {
    let maps = store
        .joins(&request.namespace)
        .map_err(ComputeError::Load)?;
    let mut paths: Vec<(u32, u32)> = roots.iter().map(|&id| (id, id)).collect();
    let mut frontier_kind = request.root_kind.as_str();
    for hop in &request.hops {
        let mut next = Vec::new();
        if store.omits_current_property(
            &request.namespace,
            hop_join_owner(frontier_kind, hop),
            &hop.join_property,
        ) {
            paths.clear();
            frontier_kind = hop.far_kind.as_str();
            continue;
        }
        let pushdown = match &hop.predicate {
            Some(pred) => {
                let indexed = declared_indexes(store, &request.namespace, &hop.far_kind)?;
                pred_is_index_tree(pred, &indexed)
            }
            None => false,
        };
        if let (Some(pred), true) = (&hop.predicate, pushdown) {
            let candidates = index_ids(store, maps, &request.namespace, &hop.far_kind, pred)?;
            let mut by_parent: HashMap<u32, Vec<u32>> = HashMap::new();
            for &(root, parent) in &paths {
                by_parent.entry(parent).or_default().push(root);
            }
            for child in candidates {
                if request.acl.hides_objects() {
                    let Some(key) = maps.intern_get(child) else {
                        continue;
                    };
                    let far = hop.far_kind.as_str();
                    if !request.acl.object_visible(far, key, |property| {
                        query_prop(store, maps, &request.namespace, far, key, property)
                    }) {
                        continue;
                    }
                }
                for parent in maps.join_parents(
                    frontier_kind,
                    &hop.far_kind,
                    &hop.join_property,
                    hop.incoming,
                    child,
                ) {
                    if let Some(roots) = by_parent.get(&parent) {
                        for &root in roots {
                            next.push((root, child));
                        }
                    }
                }
            }
        } else {
            for (root, parent) in &paths {
                for child in maps.hop_targets(
                    frontier_kind,
                    *parent,
                    &hop.far_kind,
                    &hop.join_property,
                    hop.incoming,
                ) {
                    if request.acl.hides_objects() {
                        let Some(key) = maps.intern_get(child) else {
                            continue;
                        };
                        let far = hop.far_kind.as_str();
                        if !request.acl.object_visible(far, key, |property| {
                            query_prop(store, maps, &request.namespace, far, key, property)
                        }) {
                            continue;
                        }
                    }
                    if let Some(pred) = &hop.predicate {
                        if !matches_predicate(
                            store,
                            &request.namespace,
                            &hop.far_kind,
                            child,
                            pred,
                        )? {
                            continue;
                        }
                    }
                    next.push((*root, child));
                }
            }
        }
        paths = next;
        frontier_kind = hop.far_kind.as_str();
    }
    Ok((paths, frontier_kind))
}

/// The measure of one path's leaf, when the path ends on the sum kind and
/// the leaf contributes (ADR 0018).
fn path_amount(
    store: &Store,
    maps: &JoinMaps,
    request: &EvaluateRequest,
    frontier_kind: &str,
    leaf: u32,
) -> Option<i64> {
    if frontier_kind != request.sum_kind
        || store.omits_current_property(&request.namespace, frontier_kind, &request.sum_property)
    {
        return None;
    }
    maps.leaf_amount(frontier_kind, leaf, &request.sum_property)
}

fn walk_hops(
    store: &Store,
    request: &EvaluateRequest,
    roots: &HashSet<u32>,
) -> Result<(usize, i64, Vec<u32>), ComputeError> {
    let maps = store
        .joins(&request.namespace)
        .map_err(ComputeError::Load)?;
    let (paths, frontier_kind) = surviving_paths(store, request, roots)?;
    let mut seen_roots = HashSet::new();
    let mut seen_leaves = HashSet::new();
    let mut count = 0usize;
    let mut total = 0i64;
    let mut leaves = Vec::new();
    for (root, leaf) in paths {
        if seen_roots.insert(root) {
            count += 1;
        }
        if let Some(amount) = path_amount(store, maps, request, frontier_kind, leaf) {
            total = total.wrapping_add(amount);
        }
        if seen_leaves.insert(leaf) {
            leaves.push(leaf);
        }
    }
    Ok((count, total, leaves))
}

/// Running aggregates of one group.
#[derive(Default)]
struct GroupFold {
    roots: HashSet<u32>,
    sum: i64,
    /// Exact sum for the average; checked against `i64` once at the end, so
    /// path order cannot change the answer.
    exact: i128,
    n: u64,
    min: Option<i64>,
    max: Option<i64>,
    distinct: HashSet<String>,
}

/// Fold surviving paths into groups keyed by root properties (ADR 0038).
fn breakdown_groups(
    store: &Store,
    request: &EvaluateRequest,
    breakdown: &Breakdown,
) -> Result<Vec<Group>, ComputeError> {
    let maps = store
        .joins(&request.namespace)
        .map_err(ComputeError::Load)?;
    let max_groups = match breakdown.max_groups {
        0 => MAX_GROUPS,
        asked => asked.min(MAX_GROUPS),
    };
    let roots = root_ids(store, request)?;
    let (paths, frontier_kind) = surviving_paths(store, request, &roots)?;
    let root_value = |root: u32, property: &str| -> Option<String> {
        let key = maps.intern_get(root)?;
        query_prop(
            store,
            maps,
            &request.namespace,
            &request.root_kind,
            key,
            property,
        )
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    };
    let mut groups: HashMap<Vec<Option<String>>, GroupFold> = HashMap::new();
    for (root, leaf) in paths {
        let key: Vec<_> = breakdown
            .group_by
            .iter()
            .map(|property| root_value(root, property))
            .collect();
        if !groups.contains_key(&key) && groups.len() == max_groups {
            return Err(ComputeError::TooManyGroups { max: max_groups });
        }
        let fold = groups.entry(key).or_default();
        if fold.roots.insert(root) {
            if let Some(value) = breakdown
                .distinct
                .as_deref()
                .and_then(|property| root_value(root, property))
            {
                fold.distinct.insert(value);
            }
        }
        if let Some(amount) = path_amount(store, maps, request, frontier_kind, leaf) {
            fold.sum = fold.sum.wrapping_add(amount);
            fold.exact += i128::from(amount);
            fold.n += 1;
            fold.min = Some(fold.min.map_or(amount, |min| min.min(amount)));
            fold.max = Some(fold.max.map_or(amount, |max| max.max(amount)));
        }
    }
    let mut out = groups
        .into_iter()
        .map(|(key, fold)| {
            let checked = i64::try_from(fold.exact).map_err(|_| {
                ComputeError::Overflow(format!("average of {} overflows i64", request.sum_property))
            })?;
            Ok(Group {
                key,
                count: fold.roots.len(),
                sum: fold.sum,
                n: fold.n,
                min: fold.min,
                max: fold.max,
                avg: (checked, fold.n),
                distinct: breakdown.distinct.as_ref().map(|_| fold.distinct.len()),
            })
        })
        .collect::<Result<Vec<_>, ComputeError>>()?;
    // Byte order per key part, the absent group last.
    out.sort_by(|a, b| {
        let rank = |key: &[Option<String>]| -> Vec<(bool, String)> {
            key.iter()
                .map(|part| (part.is_none(), part.clone().unwrap_or_default()))
                .collect()
        };
        rank(&a.key).cmp(&rank(&b.key))
    });
    Ok(out)
}

fn evaluate_local(
    store: &Store,
    request: &EvaluateRequest,
) -> Result<EvaluateResponse, ComputeError> {
    if let Some(last) = request.hops.last() {
        if !last.incoming
            && last.far_kind == request.sum_kind
            && last.join_property != request.sum_property
        {
            store
                .ensure_lazy_measure(&request.namespace, &request.sum_kind, &request.sum_property)
                .map_err(ComputeError::Load)?;
        }
    }
    let maps = store
        .joins(&request.namespace)
        .map_err(ComputeError::Load)?;
    let hops = hop_triples(request);
    // A restriction that only reaches the root kind narrows the roots and
    // keeps the set-oriented fold; anything on a hop kind walks each path.
    let hides_on_hops = request
        .hops
        .iter()
        .any(|hop| request.acl.hides_in_kind(&hop.far_kind));
    let dropped_join = hop_uses_dropped_join(store, request);
    let dropped_sum =
        store.omits_current_property(&request.namespace, &request.sum_kind, &request.sum_property);
    let dropped_filter = request.filter.as_ref().is_some_and(|filter| {
        store.omits_current_property(&request.namespace, &request.root_kind, &filter.property)
    });
    let (two_hop_count, sum_amount, ids) =
        if dropped_join || hides_on_hops || has_hop_predicate(request) {
            let roots = root_ids(store, request)?;
            walk_hops(store, request, &roots)?
        } else if request.predicate.is_some() || request.acl.hides_objects() {
            let roots = root_ids(store, request)?;
            let (count, sum) = maps.count_and_sum_roots(
                &request.root_kind,
                &roots,
                &hops,
                &request.sum_kind,
                &request.sum_property,
            );
            let sum = if dropped_sum { 0 } else { sum };
            let ids = if request.object_bound == 0 {
                Vec::new()
            } else {
                maps.result_ids_from_roots(&request.root_kind, &roots, &hops)
            };
            (count, sum, ids)
        } else if dropped_filter {
            (0, 0, Vec::new())
        } else {
            let (count, sum) = match &request.filter {
                None => maps.count_and_sum(
                    &request.root_kind,
                    &hops,
                    &request.sum_kind,
                    &request.sum_property,
                ),
                Some(filter) => maps.count_and_sum_matching(
                    &request.root_kind,
                    &hops,
                    &request.sum_kind,
                    &request.sum_property,
                    &filter.property,
                    &filter.value,
                ),
            };
            let sum = if dropped_sum { 0 } else { sum };
            let filter = request
                .filter
                .as_ref()
                .map(|filter| (filter.property.as_str(), filter.value.as_str()));
            let ids = if request.object_bound == 0 {
                Vec::new()
            } else {
                maps.result_ids(&request.root_kind, &hops, filter)
            };
            (count, sum, ids)
        };
    let (objects, cursor) = page_objects(store, request, ids)?;
    let groups = match &request.breakdown {
        Some(breakdown) => breakdown_groups(store, request, breakdown)?,
        None => Vec::new(),
    };
    Ok(EvaluateResponse {
        two_hop_count,
        sum_amount,
        objects,
        cursor,
        groups,
    })
}

/// A result key with, per sort key, whether its value is missing and the
/// stored value.
struct RankedKey {
    key: String,
    parts: Vec<(bool, String)>,
}

/// At most this many sort keys: `sort` plus two `then_by` (ADR 0039).
const MAX_SORT_KEYS: usize = 3;

/// `sort` then `then_by`, in order.
fn sort_keys(request: &EvaluateRequest) -> Vec<&Sort> {
    request.sort.iter().chain(&request.then_by).collect()
}

/// At most two distinct group properties; every named root property must be
/// readable.
fn check_breakdown(request: &EvaluateRequest, breakdown: &Breakdown) -> Result<(), ComputeError> {
    if breakdown.group_by.len() > 2 {
        return Err(ComputeError::Predicate(
            "breakdown groups by at most 2 properties".into(),
        ));
    }
    if breakdown.group_by.len() == 2 && breakdown.group_by[0] == breakdown.group_by[1] {
        return Err(ComputeError::Predicate(
            "breakdown group properties must differ".into(),
        ));
    }
    for property in breakdown.group_by.iter().chain(&breakdown.distinct) {
        if property.is_empty() {
            return Err(ComputeError::Predicate("empty breakdown property".into()));
        }
        request
            .acl
            .check(&request.root_kind, property)
            .map_err(ComputeError::Acl)?;
    }
    Ok(())
}

fn check_page_request(store: &Store, request: &EvaluateRequest) -> Result<(), ComputeError> {
    if request.sort.is_none()
        && request.then_by.is_empty()
        && request.page_size == 0
        && request.cursor.is_none()
    {
        return Ok(());
    }
    if request.object_bound == 0 {
        return Err(ComputeError::Page(
            "sort and pages require object_bound".into(),
        ));
    }
    if request.sort.is_none() {
        return Err(ComputeError::Page(
            "page_size, cursor, and then_by require sort".into(),
        ));
    }
    let keys = sort_keys(request);
    if keys.len() > MAX_SORT_KEYS {
        return Err(ComputeError::Page(format!(
            "at most {MAX_SORT_KEYS} sort keys"
        )));
    }
    let kind = result_kind(request);
    for (index, sort) in keys.iter().enumerate() {
        if sort.property.is_empty() {
            return Err(ComputeError::Page("sort property must be non-empty".into()));
        }
        if keys[..index]
            .iter()
            .any(|seen| seen.property == sort.property)
        {
            return Err(ComputeError::Page("sort keys must differ".into()));
        }
        if sort.nearest.is_some() {
            if index != 0 {
                return Err(ComputeError::Page("nearest is only valid on sort".into()));
            }
            if !request.then_by.is_empty() || request.cursor.is_some() {
                return Err(ComputeError::Page(
                    "nearest does not combine with then_by or a cursor".into(),
                ));
            }
            if request.page_size == 0 {
                return Err(ComputeError::Page("nearest requires page_size".into()));
            }
        }
        let ty = property_type(store, &request.namespace, kind, &sort.property)?;
        match (ty, sort.nearest.as_deref()) {
            (PropertyType::Vector { .. }, Some(query)) => {
                ty.parse_canonical(query).map_err(ComputeError::Page)?;
            }
            (PropertyType::Vector { .. }, None) => {
                return Err(ComputeError::Page("vector sort requires nearest".into()));
            }
            (_, Some(_)) => {
                return Err(ComputeError::Page(
                    "nearest requires a vector property".into(),
                ));
            }
            _ => {}
        }
    }
    if request.cursor.is_some() && request.page_size == 0 {
        return Err(ComputeError::Page("cursor requires page_size".into()));
    }
    Ok(())
}

/// The requested page of `ids` (distinct interned keys of the result kind)
/// and its continuation cursor.
fn page_objects(
    store: &Store,
    request: &EvaluateRequest,
    ids: Vec<u32>,
) -> Result<(Vec<ObjectRecord>, Option<String>), ComputeError> {
    if request.object_bound == 0 {
        return Ok((Vec::new(), None));
    }
    if ids.len() > request.object_bound {
        return Err(ComputeError::ObjectBound {
            bound: request.object_bound,
            count: ids.len(),
        });
    }
    let maps = store
        .joins(&request.namespace)
        .map_err(ComputeError::Load)?;
    let kind = result_kind(request);
    let (page, has_more) = match top_page(store, maps, request, kind, &ids)? {
        Some(page) => page,
        None => {
            let keys = maps.sorted_keys(ids);
            let ranked = if request.sort.is_some() {
                rank_keys(store, &request.namespace, kind, keys, &sort_keys(request))?
            } else {
                keys.into_iter()
                    .map(|key| RankedKey {
                        key,
                        parts: Vec::new(),
                    })
                    .collect()
            };
            let ranked = apply_cursor(store, request, ranked)?;
            split_page(ranked, request.page_size)
        }
    };
    let nearest = request
        .sort
        .as_ref()
        .is_some_and(|sort| sort.nearest.is_some());
    let cursor = if has_more && !nearest {
        let last = page
            .last()
            .expect("a continuation requires a non-empty page");
        Some(encode_cursor(
            request,
            store.snapshot_stamp(),
            &last.key,
            &last.parts,
        ))
    } else {
        None
    };
    let denied = request
        .acl
        .interned_denies(|token| maps.intern_existing(token));
    let mut objects = Vec::with_capacity(page.len());
    for row in page {
        objects.push(
            store
                .load_in_view(
                    &request.namespace,
                    kind,
                    &row.key,
                    &request.acl,
                    Some(&denied),
                )
                .map_err(ComputeError::Load)?,
        );
    }
    Ok((objects, cursor))
}

fn rank_keys(
    store: &Store,
    namespace: &str,
    kind: &str,
    keys: Vec<String>,
    sorts: &[&Sort],
) -> Result<Vec<RankedKey>, ComputeError> {
    let types = sorts
        .iter()
        .map(|sort| property_type(store, namespace, kind, &sort.property))
        .collect::<Result<Vec<_>, _>>()?;
    let maps = store.joins(namespace).map_err(ComputeError::Load)?;
    let mut ranked = Vec::with_capacity(keys.len());
    for key in keys {
        let mut parts = Vec::with_capacity(sorts.len());
        for (sort, ty) in sorts.iter().zip(&types) {
            match query_prop(store, maps, namespace, kind, &key, &sort.property) {
                None => parts.push((true, String::new())),
                Some(stored) => {
                    ty.parse_canonical(stored).map_err(ComputeError::Page)?;
                    parts.push((false, stored.to_string()));
                }
            }
        }
        ranked.push(RankedKey { key, parts });
    }
    if let (Some(sort), Some(ty)) = (sorts.first(), types.first()) {
        if let Some(query) = sort.nearest.as_deref() {
            ranked.retain(|row| row.parts.first().is_some_and(|(missing, _)| !missing));
            let mut scored = Vec::with_capacity(ranked.len());
            for row in ranked {
                let stored = &row.parts[0].1;
                let distance = ty
                    .vector_distance(stored, query)
                    .map_err(ComputeError::Page)?;
                scored.push((distance, row));
            }
            scored.sort_by(|left, right| {
                let mut order = left.0.partial_cmp(&right.0).unwrap_or(Ordering::Equal);
                if sort.descending {
                    order = order.reverse();
                }
                order.then_with(|| left.1.key.cmp(&right.1.key))
            });
            return Ok(scored.into_iter().map(|(_, row)| row).collect());
        }
    }
    ranked.sort_by(|left, right| {
        cmp_ranked(&types, sorts, left, right).expect("ranked values were parsed")
    });
    Ok(ranked)
}

/// Each sort key in turn, then the key. Missing values stay last on every
/// key; descending reverses only present values.
fn cmp_ranked(
    types: &[PropertyType],
    sorts: &[&Sort],
    left: &RankedKey,
    right: &RankedKey,
) -> Result<Ordering, String> {
    for ((ty, sort), (l, r)) in types
        .iter()
        .zip(sorts)
        .zip(left.parts.iter().zip(&right.parts))
    {
        let order = match (l.0, r.0) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => {
                let order = if let Some(query) = sort.nearest.as_deref() {
                    let left = ty.vector_distance(&l.1, query)?;
                    let right = ty.vector_distance(&r.1, query)?;
                    left.partial_cmp(&right).unwrap_or(Ordering::Equal)
                } else {
                    ty.cmp_canonical(&l.1, &r.1)?
                };
                if sort.descending {
                    order.reverse()
                } else {
                    order
                }
            }
        };
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
    Ok(left.key.cmp(&right.key))
}

/// The request cursor's position, validated against this query and the
/// sort key types. `None` without a cursor.
fn cursor_after(
    store: &Store,
    request: &EvaluateRequest,
) -> Result<Option<RankedKey>, ComputeError> {
    let Some(token) = request.cursor.as_deref() else {
        return Ok(None);
    };
    let cursor =
        decode_cursor(token, request, store.snapshot_stamp()).map_err(ComputeError::Page)?;
    let sorts = sort_keys(request);
    if cursor.after.len() != sorts.len() {
        return Err(ComputeError::Page(
            "cursor does not match this query or restriction".into(),
        ));
    }
    for (sort, (missing, value)) in sorts.iter().zip(&cursor.after) {
        if !missing {
            property_type(
                store,
                &request.namespace,
                result_kind(request),
                &sort.property,
            )?
            .parse_canonical(value)
            .map_err(ComputeError::Page)?;
        }
    }
    Ok(Some(RankedKey {
        key: cursor.after_key,
        parts: cursor.after,
    }))
}

fn apply_cursor(
    store: &Store,
    request: &EvaluateRequest,
    ranked: Vec<RankedKey>,
) -> Result<Vec<RankedKey>, ComputeError> {
    let Some(after) = cursor_after(store, request)? else {
        return Ok(ranked);
    };
    let sorts = sort_keys(request);
    let types = sorts
        .iter()
        .map(|sort| {
            property_type(
                store,
                &request.namespace,
                result_kind(request),
                &sort.property,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut kept = Vec::new();
    for row in ranked {
        if cmp_ranked(&types, &sorts, &row, &after).map_err(ComputeError::Page)?
            == Ordering::Greater
        {
            kept.push(row);
        }
    }
    Ok(kept)
}

/// One row of [`top_page`]: its parsed sort value and stored form (`None`
/// is missing), and its key.
struct TopRow<'m> {
    value: Option<(PropertyValue, &'m str)>,
    key: &'m str,
}

/// Typed order of two present sort values, reversed when descending.
fn cmp_value(
    ty: PropertyType,
    descending: bool,
    left: &PropertyValue,
    right: &PropertyValue,
) -> Ordering {
    // Both sides parsed under `ty`, so the comparison cannot mix types.
    let order = ty.cmp_values(left, right).unwrap_or(Ordering::Equal);
    if descending {
        order.reverse()
    } else {
        order
    }
}

/// Single-key order of [`cmp_ranked`]: present values in typed order,
/// missing last, then the key ascending.
fn cmp_top(ty: PropertyType, descending: bool, left: &TopRow, right: &TopRow) -> Ordering {
    let order = match (&left.value, &right.value) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some((l, _)), Some((r, _))) => cmp_value(ty, descending, l, r),
    };
    order.then_with(|| left.key.cmp(right.key))
}

/// First page of a paged single-key sort without ranking every result key
/// (#316). Same order as [`cmp_ranked`], so the page and cursor equal the
/// full ranking's.
///
/// When the sort property has no more distinct values than there are
/// results, [`walk_values`] reads the property's equality index in value
/// order and stops once the page is full. Otherwise [`scan_top`] keeps the
/// `page_size + 1` lowest results in one pass.
///
/// `None` hands the request to the full ranking: unpaged, multi-key, or
/// nearest sorts, and any stored value or cursor that fails to parse, so
/// errors and their precedence stay the full ranking's.
fn top_page(
    store: &Store,
    maps: &JoinMaps,
    request: &EvaluateRequest,
    kind: &str,
    ids: &[u32],
) -> Result<Option<(Vec<RankedKey>, bool)>, ComputeError> {
    let Some(sort) = request.sort.as_ref() else {
        return Ok(None);
    };
    if request.page_size == 0 || !request.then_by.is_empty() || sort.nearest.is_some() {
        return Ok(None);
    }
    let ty = property_type(store, &request.namespace, kind, &sort.property)?;
    let Ok(after) = cursor_after(store, request) else {
        return Ok(None);
    };
    let after = match &after {
        None => None,
        Some(after) => {
            let (missing, stored) = &after.parts[0];
            let value = if *missing {
                None
            } else {
                match ty.parse_canonical(stored) {
                    Ok(value) => Some((value, stored.as_str())),
                    Err(_) => return Ok(None),
                }
            };
            Some(TopRow {
                value,
                key: after.key.as_str(),
            })
        }
    };
    // `query_prop` by interned ids: a dropped property reads as missing.
    let column = if store.omits_current_property(&request.namespace, kind, &sort.property) {
        None
    } else {
        maps.intern_existing(kind)
            .zip(maps.intern_existing(&sort.property))
    };
    let order = TopOrder {
        ty,
        descending: sort.descending,
        after: after.as_ref(),
        // One past the page shows whether more follow; never more rows than
        // the results hold, so a huge page size allocates nothing extra.
        keep: request.page_size.saturating_add(1).min(ids.len()).max(1),
    };
    let walked = column
        .and_then(|(kind_id, prop_id)| {
            maps.value_index(kind_id, prop_id)
                .filter(|index| index.len() <= ids.len())
                .map(|index| (kind_id, prop_id, index))
        })
        .and_then(|(kind_id, prop_id, index)| {
            walk_values(maps, &order, index, ids, (kind_id, prop_id))
        });
    let Some(mut top) = walked.or_else(|| scan_top(maps, &order, ids, column)) else {
        return Ok(None);
    };
    let has_more = top.len() > request.page_size;
    top.truncate(request.page_size);
    let page = top
        .into_iter()
        .map(|row| RankedKey {
            key: row.key.to_string(),
            parts: vec![match row.value {
                None => (true, String::new()),
                Some((_, stored)) => (false, stored.to_string()),
            }],
        })
        .collect();
    Ok(Some((page, has_more)))
}

/// What [`walk_values`] and [`scan_top`] rank by and how many rows they keep.
struct TopOrder<'a> {
    ty: PropertyType,
    descending: bool,
    /// Only rows strictly after this cursor position count.
    after: Option<&'a TopRow<'a>>,
    keep: usize,
}

impl TopOrder<'_> {
    fn past_cursor(&self, row: &TopRow) -> bool {
        self.after
            .is_none_or(|after| cmp_top(self.ty, self.descending, row, after) == Ordering::Greater)
    }
}

/// The lowest `order.keep` results, in order, from one pass over `ids`.
/// `None` when a result's stored sort value does not parse.
fn scan_top<'m>(
    maps: &'m JoinMaps,
    order: &TopOrder,
    ids: &[u32],
    column: Option<(u32, u32)>,
) -> Option<Vec<TopRow<'m>>> {
    let (ty, descending, keep) = (order.ty, order.descending, order.keep);
    // Bounded insertion pays off for a small page: most rows lose one
    // comparison against the current last. A large page would shift the
    // kept rows on nearly every insert, so it collects and sorts once.
    let insert = keep <= SCAN_TOP_INSERT_LIMIT;
    let mut top: Vec<TopRow> = Vec::with_capacity(if insert { keep + 1 } else { ids.len() });
    for &id in ids {
        let Some(key) = maps.intern_get(id) else {
            continue;
        };
        let stored = column.and_then(|(kind_id, prop_id)| maps.owned_value(kind_id, id, prop_id));
        let value = match stored {
            None => None,
            Some(stored) => Some((ty.parse_canonical(stored).ok()?, stored)),
        };
        let row = TopRow { value, key };
        if !order.past_cursor(&row) {
            continue;
        }
        if !insert {
            top.push(row);
            continue;
        }
        if top.len() == keep && cmp_top(ty, descending, &row, &top[keep - 1]).is_ge() {
            continue;
        }
        let at = top.partition_point(|kept| cmp_top(ty, descending, kept, &row).is_lt());
        top.insert(at, row);
        top.truncate(keep);
    }
    if !insert {
        // `cmp_top` breaks ties by key, so the order is total.
        top.sort_unstable_by(|left, right| cmp_top(ty, descending, left, right));
        top.truncate(keep);
    }
    Some(top)
}

/// Largest page [`scan_top`] ranks by bounded insertion. Tests use a small
/// limit so their pages exercise both the insertion and the sort path.
const SCAN_TOP_INSERT_LIMIT: usize = if cfg!(test) { 8 } else { 1024 };

/// The lowest `order.keep` results, in order, read from the sort
/// property's equality index (value id -> visible keys) one value run at a
/// time, so only the runs the page reaches are ranked. Results without the
/// property sort last and are read only when every run is exhausted.
/// `None` when a stored value does not parse or a result is missing from
/// the index although it holds the property.
fn walk_values<'m>(
    maps: &'m JoinMaps,
    order: &TopOrder,
    index: &'m HashMap<u32, Vec<u32>>,
    ids: &[u32],
    (kind_id, prop_id): (u32, u32),
) -> Option<Vec<TopRow<'m>>> {
    let (ty, descending, keep) = (order.ty, order.descending, order.keep);
    let mut values = Vec::with_capacity(index.len());
    for (&value_id, keys) in index {
        let stored = maps.intern_get(value_id)?;
        values.push((ty.parse_canonical(stored).ok()?, stored, keys));
    }
    values.sort_by(|l, r| cmp_value(ty, descending, &l.0, &r.0));
    // Results not yet met in a run; what is left at the end has no value.
    let mut pending = vec![0u64; maps.intern.len().div_ceil(64)];
    for &id in ids {
        if let Some(word) = pending.get_mut(id as usize / 64) {
            *word |= 1 << (id % 64);
        }
    }
    let mut take = |id: u32| {
        let (word, bit) = (id as usize / 64, 1u64 << (id % 64));
        let hit = pending.get(word).is_some_and(|w| w & bit != 0);
        if hit {
            pending[word] &= !bit;
        }
        hit
    };
    let mut top: Vec<TopRow> = Vec::with_capacity(keep);
    let mut start = 0;
    while start < values.len() && top.len() < keep {
        let run_value = &values[start].0;
        let end = start
            + values[start..]
                .iter()
                .take_while(|(value, _, _)| cmp_value(ty, descending, run_value, value).is_eq())
                .count();
        // Every key of a run shares its value, so the cursor compares once.
        let cursor = order.after.map(|after| match &after.value {
            None => Ordering::Less,
            Some((value, _)) => cmp_value(ty, descending, run_value, value),
        });
        let mut run: Vec<(&str, &str)> = Vec::new();
        for (_, stored, keys) in &values[start..end] {
            for &id in keys.iter() {
                if !take(id) {
                    continue;
                }
                let Some(key) = maps.intern_get(id) else {
                    continue;
                };
                let kept = match cursor {
                    None | Some(Ordering::Greater) => true,
                    Some(Ordering::Equal) => order.after.is_some_and(|after| key > after.key),
                    Some(Ordering::Less) => false,
                };
                if kept {
                    run.push((key, stored));
                }
            }
        }
        let room = keep - top.len();
        if run.len() > room {
            run.select_nth_unstable_by(room - 1, |l, r| l.0.cmp(r.0));
            run.truncate(room);
        }
        run.sort_unstable_by(|l, r| l.0.cmp(r.0));
        for (key, stored) in run {
            top.push(TopRow {
                value: Some((ty.parse_canonical(stored).ok()?, stored)),
                key,
            });
        }
        start = end;
    }
    if top.len() < keep {
        let mut missing = Vec::new();
        for &id in ids {
            if !take(id) {
                continue;
            }
            if maps.owned_value(kind_id, id, prop_id).is_some() {
                return None;
            }
            let Some(key) = maps.intern_get(id) else {
                continue;
            };
            let row = TopRow { value: None, key };
            if order.past_cursor(&row) {
                missing.push(row);
            }
        }
        missing.sort_unstable_by(|l, r| l.key.cmp(r.key));
        top.extend(missing.into_iter().take(keep - top.len()));
    }
    Some(top)
}

fn split_page(ranked: Vec<RankedKey>, page_size: usize) -> (Vec<RankedKey>, bool) {
    if page_size == 0 || ranked.len() <= page_size {
        return (ranked, false);
    }
    let rest = ranked.len() - page_size;
    let mut page = ranked;
    page.truncate(page_size);
    (page, rest > 0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComputeError {
    Acl(AclError),
    UnsupportedBackend {
        name: &'static str,
    },
    ObjectBound {
        bound: usize,
        count: usize,
    },
    Load(String),
    Predicate(String),
    Page(String),
    /// A breakdown produced more groups than its bound (ADR 0038).
    TooManyGroups {
        max: usize,
    },
    /// A checked breakdown measure overflowed (ADR 0038).
    Overflow(String),
}

pub trait ComputeBackend {
    fn name(&self) -> &'static str;
    fn evaluate(
        &self,
        store: &Store,
        request: &EvaluateRequest,
    ) -> Result<EvaluateResponse, ComputeError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LocalCompute;

impl ComputeBackend for LocalCompute {
    fn name(&self) -> &'static str {
        "local"
    }

    fn evaluate(
        &self,
        store: &Store,
        request: &EvaluateRequest,
    ) -> Result<EvaluateResponse, ComputeError> {
        match request.aggregate {
            Aggregate::CountAndSum => {
                if request.namespace.is_empty() {
                    return Err(ComputeError::Load("namespace required".into()));
                }
                store
                    .joins(&request.namespace)
                    .map_err(ComputeError::Load)?;
                request
                    .acl
                    .check(&request.sum_kind, &request.sum_property)
                    .map_err(ComputeError::Acl)?;
                if request.filter.is_some() && request.predicate.is_some() {
                    return Err(ComputeError::Predicate(
                        "evaluate accepts filter or predicate, not both".into(),
                    ));
                }
                if let Some(filter) = &request.filter {
                    request
                        .acl
                        .check(&request.root_kind, &filter.property)
                        .map_err(ComputeError::Acl)?;
                }
                if let Some(pred) = &request.predicate {
                    check_predicate_acl(request, &request.root_kind, pred)?;
                    check_predicate_types(store, &request.namespace, &request.root_kind, pred)?;
                }
                check_page_request(store, request)?;
                if let Some(breakdown) = &request.breakdown {
                    check_breakdown(request, breakdown)?;
                }
                let kind = result_kind(request);
                for sort in sort_keys(request) {
                    request
                        .acl
                        .check(kind, &sort.property)
                        .map_err(ComputeError::Acl)?;
                }
                let mut frontier_kind = request.root_kind.as_str();
                for hop in &request.hops {
                    let join_kind = if hop.incoming {
                        frontier_kind
                    } else {
                        hop.far_kind.as_str()
                    };
                    request
                        .acl
                        .check(join_kind, &hop.join_property)
                        .map_err(ComputeError::Acl)?;
                    if let Some(pred) = &hop.predicate {
                        check_predicate_acl(request, &hop.far_kind, pred)?;
                        check_predicate_types(store, &request.namespace, &hop.far_kind, pred)?;
                    }
                    frontier_kind = hop.far_kind.as_str();
                }
                evaluate_local(store, request)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SparkCompute;

impl ComputeBackend for SparkCompute {
    fn name(&self) -> &'static str {
        "spark"
    }

    fn evaluate(
        &self,
        _store: &Store,
        _request: &EvaluateRequest,
    ) -> Result<EvaluateResponse, ComputeError> {
        Err(ComputeError::UnsupportedBackend { name: "spark" })
    }
}
