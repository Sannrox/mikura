use std::cmp::Ordering;
use std::collections::HashSet;

use crate::acl::AclError;
use crate::objectset::{Aggregate, EvaluateRequest, EvaluateResponse, Predicate, Sort};
use crate::page::{decode_cursor, encode_cursor};
use crate::store::{ObjectRecord, Store};
use crate::value::PropertyType;

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

fn property_type(store: &Store, kind: &str, property: &str) -> Result<PropertyType, ComputeError> {
    match store.schema(kind) {
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
        | Predicate::Missing { property } => {
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
    kind: &str,
    property: &str,
    raw: &str,
) -> Result<(), ComputeError> {
    let ty = property_type(store, kind, property)?;
    ty.parse_canonical(raw).map_err(ComputeError::Predicate)?;
    Ok(())
}

fn check_predicate_types(store: &Store, kind: &str, pred: &Predicate) -> Result<(), ComputeError> {
    check_predicate_shape(pred)?;
    match pred {
        Predicate::Eq { property, value } | Predicate::Neq { property, value } => {
            parse_predicate_value(store, kind, property, value)
        }
        Predicate::Range { property, min, max } => {
            let ty = property_type(store, kind, property)?;
            if matches!(ty, PropertyType::Boolean) {
                return Err(ComputeError::Predicate(format!(
                    "range is not defined for boolean {kind}.{property}"
                )));
            }
            if let Some(bound) = min {
                ty.parse_canonical(bound).map_err(ComputeError::Predicate)?;
            }
            if let Some(bound) = max {
                ty.parse_canonical(bound).map_err(ComputeError::Predicate)?;
            }
            Ok(())
        }
        Predicate::Missing { .. } => Ok(()),
        Predicate::And(args) | Predicate::Or(args) => {
            for arg in args {
                check_predicate_types(store, kind, arg)?;
            }
            Ok(())
        }
        Predicate::Not(inner) => check_predicate_types(store, kind, inner),
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
    kind: &str,
    key_id: u32,
    pred: &Predicate,
) -> Result<bool, ComputeError> {
    let maps = store.joins();
    let Some(key) = maps.intern_get(key_id) else {
        return Ok(false);
    };
    match pred {
        Predicate::Eq { property, value } => {
            let Some(stored) = maps.prop(kind, key, property) else {
                return Ok(false);
            };
            let ty = property_type(store, kind, property)?;
            Ok(stored_eq(ty, stored, value))
        }
        Predicate::Neq { property, value } => {
            let Some(stored) = maps.prop(kind, key, property) else {
                return Ok(false);
            };
            let ty = property_type(store, kind, property)?;
            Ok(!stored_eq(ty, stored, value))
        }
        Predicate::Range { property, min, max } => {
            let Some(stored) = maps.prop(kind, key, property) else {
                return Ok(false);
            };
            let ty = property_type(store, kind, property)?;
            ty.in_range(stored, min.as_deref(), max.as_deref())
                .map_err(ComputeError::Predicate)
        }
        Predicate::Missing { property } => Ok(maps.prop(kind, key, property).is_none()),
        Predicate::And(args) => {
            for arg in args {
                if !matches_predicate(store, kind, key_id, arg)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Predicate::Or(args) => {
            for arg in args {
                if matches_predicate(store, kind, key_id, arg)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Predicate::Not(inner) => Ok(!matches_predicate(store, kind, key_id, inner)?),
    }
}

fn filter_ids(
    store: &Store,
    kind: &str,
    ids: HashSet<u32>,
    pred: &Predicate,
) -> Result<HashSet<u32>, ComputeError> {
    let mut kept = HashSet::new();
    for id in ids {
        if matches_predicate(store, kind, id, pred)? {
            kept.insert(id);
        }
    }
    Ok(kept)
}

fn root_ids(store: &Store, request: &EvaluateRequest) -> Result<HashSet<u32>, ComputeError> {
    if request.filter.is_some() && request.predicate.is_some() {
        return Err(ComputeError::Predicate(
            "evaluate accepts filter or predicate, not both".into(),
        ));
    }
    let maps = store.joins();
    let ids = if let Some(filter) = &request.filter {
        maps.matching_root_ids(&request.root_kind, &filter.property, &filter.value)
    } else {
        maps.visible_ids(&request.root_kind)
    };
    let ids = visible_in_view(store, request, &request.root_kind, ids);
    if let Some(pred) = &request.predicate {
        filter_ids(store, &request.root_kind, ids, pred)
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
    let maps = store.joins();
    ids.into_iter()
        .filter(|&id| {
            maps.intern_get(id)
                .is_some_and(|key| request.acl.object_visible(kind, key))
        })
        .collect()
}

fn walk_hops(
    store: &Store,
    request: &EvaluateRequest,
    roots: &HashSet<u32>,
) -> Result<(usize, i64, Vec<u32>), ComputeError> {
    let maps = store.joins();
    let mut paths: Vec<(u32, u32)> = roots.iter().map(|&id| (id, id)).collect();
    let mut frontier_kind = request.root_kind.as_str();
    for hop in &request.hops {
        let mut next = Vec::new();
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
                    if !request.acl.object_visible(&hop.far_kind, key) {
                        continue;
                    }
                }
                if let Some(pred) = &hop.predicate {
                    if !matches_predicate(store, &hop.far_kind, child, pred)? {
                        continue;
                    }
                }
                next.push((*root, child));
            }
        }
        paths = next;
        frontier_kind = hop.far_kind.as_str();
    }
    let mut seen_roots = HashSet::new();
    let mut seen_leaves = HashSet::new();
    let mut count = 0usize;
    let mut total = 0i64;
    let mut leaves = Vec::new();
    for (root, leaf) in paths {
        if seen_roots.insert(root) {
            count += 1;
        }
        if frontier_kind == request.sum_kind {
            if let Some(amount) = maps.leaf_amount(frontier_kind, leaf, &request.sum_property) {
                total += amount;
            }
        }
        if seen_leaves.insert(leaf) {
            leaves.push(leaf);
        }
    }
    Ok((count, total, leaves))
}

fn leaf_keys(store: &Store, ids: Vec<u32>) -> Vec<String> {
    let maps = store.joins();
    let mut keys: Vec<String> = ids
        .into_iter()
        .filter_map(|id| maps.intern_get(id).map(str::to_string))
        .collect();
    keys.sort();
    keys
}

fn evaluate_local(
    store: &Store,
    request: &EvaluateRequest,
) -> Result<EvaluateResponse, ComputeError> {
    let hops = hop_triples(request);
    let (two_hop_count, sum_amount, keys) =
        if request.acl.hides_objects() || has_hop_predicate(request) {
            let roots = root_ids(store, request)?;
            let (count, sum, leaves) = walk_hops(store, request, &roots)?;
            (count, sum, leaf_keys(store, leaves))
        } else if request.predicate.is_some() {
            let roots = root_ids(store, request)?;
            let (count, sum) = store.joins().count_and_sum_roots(
                &request.root_kind,
                &roots,
                &hops,
                &request.sum_kind,
                &request.sum_property,
            );
            let keys = store
                .joins()
                .result_keys_from_roots(&request.root_kind, &roots, &hops);
            (count, sum, keys)
        } else {
            let (count, sum) = match &request.filter {
                None => store.joins().count_and_sum(
                    &request.root_kind,
                    &hops,
                    &request.sum_kind,
                    &request.sum_property,
                ),
                Some(filter) => store.joins().count_and_sum_matching(
                    &request.root_kind,
                    &hops,
                    &request.sum_kind,
                    &request.sum_property,
                    &filter.property,
                    &filter.value,
                ),
            };
            let filter = request
                .filter
                .as_ref()
                .map(|filter| (filter.property.as_str(), filter.value.as_str()));
            let keys = store.joins().result_keys(&request.root_kind, &hops, filter);
            (count, sum, keys)
        };
    let (objects, cursor) = page_objects(store, request, keys)?;
    Ok(EvaluateResponse {
        two_hop_count,
        sum_amount,
        objects,
        cursor,
    })
}

struct RankedKey {
    key: String,
    missing: bool,
    value: String,
}

fn check_page_request(request: &EvaluateRequest) -> Result<(), ComputeError> {
    if request.sort.is_none() && request.page_size == 0 && request.cursor.is_none() {
        return Ok(());
    }
    if request.object_bound == 0 {
        return Err(ComputeError::Page(
            "sort and pages require object_bound".into(),
        ));
    }
    let Some(sort) = &request.sort else {
        return Err(ComputeError::Page(
            "page_size and cursor require sort".into(),
        ));
    };
    if sort.property.is_empty() {
        return Err(ComputeError::Page("sort property must be non-empty".into()));
    }
    if request.cursor.is_some() && request.page_size == 0 {
        return Err(ComputeError::Page("cursor requires page_size".into()));
    }
    Ok(())
}

fn page_objects(
    store: &Store,
    request: &EvaluateRequest,
    keys: Vec<String>,
) -> Result<(Vec<ObjectRecord>, Option<String>), ComputeError> {
    if request.object_bound == 0 {
        return Ok((Vec::new(), None));
    }
    if keys.len() > request.object_bound {
        return Err(ComputeError::ObjectBound {
            bound: request.object_bound,
            count: keys.len(),
        });
    }
    let kind = result_kind(request);
    let ranked = match &request.sort {
        Some(sort) => rank_keys(store, kind, keys, sort)?,
        None => keys
            .into_iter()
            .map(|key| RankedKey {
                key,
                missing: true,
                value: String::new(),
            })
            .collect(),
    };
    let ranked = apply_cursor(store, request, ranked)?;
    let (page, has_more) = split_page(ranked, request.page_size);
    let cursor = if has_more {
        let last = page
            .last()
            .expect("a continuation requires a non-empty page");
        Some(encode_cursor(
            request,
            store.snapshot_stamp(),
            &last.key,
            last.missing,
            &last.value,
        ))
    } else {
        None
    };
    let mut objects = Vec::with_capacity(page.len());
    for row in page {
        objects.push(
            store
                .load(kind, &row.key, &request.acl)
                .map_err(ComputeError::Load)?,
        );
    }
    Ok((objects, cursor))
}

fn rank_keys(
    store: &Store,
    kind: &str,
    keys: Vec<String>,
    sort: &Sort,
) -> Result<Vec<RankedKey>, ComputeError> {
    let ty = property_type(store, kind, &sort.property)?;
    let maps = store.joins();
    let mut ranked = Vec::with_capacity(keys.len());
    for key in keys {
        match maps.prop(kind, &key, &sort.property) {
            None => ranked.push(RankedKey {
                key,
                missing: true,
                value: String::new(),
            }),
            Some(stored) => {
                ty.parse_canonical(stored).map_err(ComputeError::Page)?;
                ranked.push(RankedKey {
                    key,
                    missing: false,
                    value: stored.to_string(),
                });
            }
        }
    }
    ranked.sort_by(|left, right| {
        cmp_ranked(ty, sort.descending, left, right).expect("ranked values were parsed")
    });
    Ok(ranked)
}

fn cmp_ranked(
    ty: PropertyType,
    descending: bool,
    left: &RankedKey,
    right: &RankedKey,
) -> Result<Ordering, String> {
    // Missing values stay last; descending reverses only present values.
    let value_order = match (left.missing, right.missing) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => {
            let order = ty.cmp_canonical(&left.value, &right.value)?;
            if descending {
                order.reverse()
            } else {
                order
            }
        }
    };
    Ok(value_order.then_with(|| left.key.cmp(&right.key)))
}

fn apply_cursor(
    store: &Store,
    request: &EvaluateRequest,
    ranked: Vec<RankedKey>,
) -> Result<Vec<RankedKey>, ComputeError> {
    let Some(token) = request.cursor.as_deref() else {
        return Ok(ranked);
    };
    let cursor =
        decode_cursor(token, request, store.snapshot_stamp()).map_err(ComputeError::Page)?;
    let ty = property_type(
        store,
        result_kind(request),
        &request
            .sort
            .as_ref()
            .expect("cursor requires sort")
            .property,
    )?;
    let descending = request
        .sort
        .as_ref()
        .map(|sort| sort.descending)
        .unwrap_or(false);
    if !cursor.after_missing {
        ty.parse_canonical(&cursor.after_value)
            .map_err(ComputeError::Page)?;
    }
    let after = RankedKey {
        key: cursor.after_key,
        missing: cursor.after_missing,
        value: cursor.after_value,
    };
    let mut kept = Vec::new();
    for row in ranked {
        if cmp_ranked(ty, descending, &row, &after).map_err(ComputeError::Page)?
            == Ordering::Greater
        {
            kept.push(row);
        }
    }
    Ok(kept)
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
    UnsupportedBackend { name: &'static str },
    ObjectBound { bound: usize, count: usize },
    Load(String),
    Predicate(String),
    Page(String),
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
                    check_predicate_types(store, &request.root_kind, pred)?;
                }
                check_page_request(request)?;
                if let Some(sort) = &request.sort {
                    let kind = result_kind(request);
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
                        check_predicate_types(store, &hop.far_kind, pred)?;
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
