//! Query-shape recording and a reviewable store-tuning advisor.
//!
//! Clerks record evaluate traffic, review suggestions, and apply catalog
//! changes (schema `indexes` / `sums`). Auto-apply stays inside a memory
//! budget. Denormalized-link copies stay clerk-applied. See ADR 0045.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::compute::{ComputeBackend, ComputeError};
use crate::joins::JoinMaps;
use crate::objectset::{EvaluateRequest, EvaluateResponse, ObjectSet, Predicate};
use crate::schema::SchemaDescriptor;
use crate::store::Store;
use crate::value::PropertyType;

/// Bytes charged for one cached last-hop parent rollup (`count` + `sum`).
pub const MEASURE_CACHE_BYTES: u64 = 16;
/// Bytes charged for a clerk-applied denormalized hop copy, per near identity.
pub const DENORMALIZE_BYTES: u64 = 64;

/// Filter, sort, hop, and measure names observed on one evaluate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryShape {
    pub namespace: String,
    pub root_kind: String,
    /// `(far_kind, join_property, incoming)` in hop order.
    pub hops: Vec<(String, String, bool)>,
    /// `(kind, property)` names from the root predicate/filter and hop predicates.
    pub filter_properties: Vec<(String, String)>,
    /// `(kind, property)` names from `sort` and `then_by`.
    pub sort_properties: Vec<(String, String)>,
    pub sum_kind: String,
    pub sum_property: String,
}

/// Cost of one evaluate: wall time plus scan versus index/cache use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryCost {
    pub latency_ns: u128,
    pub identities_scanned: usize,
    pub used_index: bool,
    pub measure_cached: bool,
}

/// One recorded evaluate.
#[derive(Clone, Debug)]
pub struct QuerySample {
    pub request: EvaluateRequest,
    pub shape: QueryShape,
    pub cost: QueryCost,
}

/// Sliding window of evaluate samples. The caller owns the window length.
#[derive(Clone, Debug, Default)]
pub struct QueryRecorder {
    samples: Vec<QuerySample>,
}

impl QueryRecorder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn samples(&self) -> &[QuerySample] {
        &self.samples
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }

    /// Time `evaluate`, record the shape and cost, and return the response.
    pub fn record<B: ComputeBackend>(
        &mut self,
        store: &Store,
        oss: &ObjectSet<B>,
        request: &EvaluateRequest,
    ) -> Result<EvaluateResponse, ComputeError> {
        let first_use = inspect_cost(store, request).map_err(ComputeError::Load)?;
        let start = Instant::now();
        let response = oss.evaluate(store, request)?;
        let mut cost = inspect_cost(store, request).map_err(ComputeError::Load)?;
        cost.latency_ns = start.elapsed().as_nanos();
        if !first_use.measure_cached {
            cost.measure_cached = false;
            cost.identities_scanned = cost.identities_scanned.max(first_use.identities_scanned);
        }
        self.samples.push(QuerySample {
            request: request.clone(),
            shape: shape_of(request),
            cost,
        });
        Ok(response)
    }
}

/// Catalog change the advisor proposes. Suggestions are reviewable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuggestionKind {
    AddIndex,
    DropIndex,
    CacheMeasure,
    DropMeasure,
    DenormalizeLink,
}

/// One reviewable catalog or clerk change with estimated benefit and memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub kind: SuggestionKind,
    pub namespace: String,
    pub object_kind: String,
    pub property: String,
    /// Far kind for [`SuggestionKind::DenormalizeLink`]; unused otherwise.
    pub far_kind: Option<String>,
    pub estimated_benefit_ns: u128,
    pub estimated_bytes: u64,
    pub reason: String,
}

/// Cap on extra projection bytes auto-apply may consume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryBudget {
    pub max_bytes: u64,
}

/// Result of applying one suggestion, with before/after cost of a probe query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedChange {
    pub suggestion: Suggestion,
    pub before: QueryCost,
    pub after: QueryCost,
    pub rolled_back: bool,
}

/// Scan versus index/cache cost for `request` without running evaluate.
pub fn inspect_cost(store: &Store, request: &EvaluateRequest) -> Result<QueryCost, String> {
    let maps = store.joins(&request.namespace)?;
    let (identities_scanned, used_index) = scan_cost(store, maps, request)?;
    Ok(QueryCost {
        latency_ns: 0,
        identities_scanned,
        used_index,
        measure_cached: measure_is_cached(store, maps, request)?,
    })
}

/// Suggest catalog changes from recorded samples against the live store.
pub fn suggest(store: &Store, recorder: &QueryRecorder) -> Result<Vec<Suggestion>, String> {
    let mut out = Vec::new();
    out.extend(suggest_add_indexes(store, recorder)?);
    out.extend(suggest_drop_indexes(store, recorder)?);
    out.extend(suggest_cache_measures(store, recorder)?);
    out.extend(suggest_drop_measures(store, recorder)?);
    out.extend(suggest_denormalize(store, recorder)?);
    Ok(out)
}

/// Apply a catalog suggestion. [`SuggestionKind::DenormalizeLink`] is clerk-applied.
pub fn apply(store: &mut Store, suggestion: &Suggestion) -> Result<(), String> {
    match suggestion.kind {
        SuggestionKind::DenormalizeLink => {
            Err("denormalize copies instance properties; the clerk applies that rewrite".into())
        }
        SuggestionKind::AddIndex => edit_schema(store, suggestion, |schema| {
            if !schema
                .indexes
                .iter()
                .any(|name| name == &suggestion.property)
            {
                schema.indexes.push(suggestion.property.clone());
            }
            Ok(())
        }),
        SuggestionKind::DropIndex => edit_schema(store, suggestion, |schema| {
            schema.indexes.retain(|name| name != &suggestion.property);
            Ok(())
        }),
        SuggestionKind::CacheMeasure => edit_schema(store, suggestion, |schema| {
            if !schema.sums.iter().any(|name| name == &suggestion.property) {
                schema.sums.push(suggestion.property.clone());
            }
            Ok(())
        }),
        SuggestionKind::DropMeasure => edit_schema(store, suggestion, |schema| {
            schema.sums.retain(|name| name != &suggestion.property);
            Ok(())
        }),
    }
}

/// Invert a catalog suggestion (another schema generation).
pub fn rollback(store: &mut Store, suggestion: &Suggestion) -> Result<(), String> {
    apply(store, &invert(suggestion))
}

/// Apply, measure a probe evaluate, and roll back when the change does not help.
pub fn apply_measured<B: ComputeBackend>(
    store: &mut Store,
    oss: &ObjectSet<B>,
    suggestion: &Suggestion,
    probe: &EvaluateRequest,
) -> Result<AppliedChange, ComputeError> {
    let probe = cursor_free(probe);
    let before = timed_cost(store, oss, &probe)?;
    apply(store, suggestion).map_err(ComputeError::Load)?;
    let after = match timed_cost(store, oss, &probe) {
        Ok(cost) => cost,
        Err(err) => {
            rollback(store, suggestion).map_err(ComputeError::Load)?;
            return Err(err);
        }
    };
    let mut change = AppliedChange {
        suggestion: suggestion.clone(),
        before: before.clone(),
        after: after.clone(),
        rolled_back: false,
    };
    if should_rollback(suggestion, &before, &after) {
        rollback(store, suggestion).map_err(ComputeError::Load)?;
        change.after = timed_cost(store, oss, &probe)?;
        change.rolled_back = true;
    }
    Ok(change)
}

/// Apply drops, then adds that fit `budget`. Denormalize is skipped.
/// Rolled-back adds do not consume budget. Auto-apply never exceeds `budget`.
pub fn auto_apply<B: ComputeBackend>(
    store: &mut Store,
    oss: &ObjectSet<B>,
    recorder: &QueryRecorder,
    suggestions: &[Suggestion],
    budget: MemoryBudget,
) -> Result<Vec<AppliedChange>, ComputeError> {
    let mut applied = Vec::new();
    let mut used = 0u64;
    let mut ordered: Vec<&Suggestion> = suggestions.iter().collect();
    ordered.sort_by_key(|suggestion| match suggestion.kind {
        SuggestionKind::DropIndex | SuggestionKind::DropMeasure => 0,
        SuggestionKind::AddIndex | SuggestionKind::CacheMeasure => 1,
        SuggestionKind::DenormalizeLink => 2,
    });
    for suggestion in ordered {
        if suggestion.kind == SuggestionKind::DenormalizeLink {
            continue;
        }
        let drop = matches!(
            suggestion.kind,
            SuggestionKind::DropIndex | SuggestionKind::DropMeasure
        );
        if drop {
            apply(store, suggestion).map_err(ComputeError::Load)?;
            applied.push(AppliedChange {
                suggestion: suggestion.clone(),
                before: QueryCost {
                    latency_ns: 0,
                    identities_scanned: 0,
                    used_index: false,
                    measure_cached: false,
                },
                after: QueryCost {
                    latency_ns: 0,
                    identities_scanned: 0,
                    used_index: false,
                    measure_cached: false,
                },
                rolled_back: false,
            });
            continue;
        }
        let add = matches!(
            suggestion.kind,
            SuggestionKind::AddIndex | SuggestionKind::CacheMeasure
        );
        let bytes = live_estimated_bytes(store, suggestion).unwrap_or(suggestion.estimated_bytes);
        if add && used.saturating_add(bytes) > budget.max_bytes {
            continue;
        }
        let Some(probe) = probe_for(recorder, suggestion) else {
            continue;
        };
        let change = apply_measured(store, oss, suggestion, &probe)?;
        if add && !change.rolled_back {
            used = used.saturating_add(bytes);
        }
        applied.push(change);
    }
    Ok(applied)
}

fn timed_cost<B: ComputeBackend>(
    store: &Store,
    oss: &ObjectSet<B>,
    request: &EvaluateRequest,
) -> Result<QueryCost, ComputeError> {
    let start = Instant::now();
    oss.evaluate(store, request)?;
    let mut cost = inspect_cost(store, request).map_err(ComputeError::Load)?;
    cost.latency_ns = start.elapsed().as_nanos();
    Ok(cost)
}

fn should_rollback(suggestion: &Suggestion, before: &QueryCost, after: &QueryCost) -> bool {
    match suggestion.kind {
        // A partial index on an `and`/`or` tree does not flip `used_index`
        // until every name is declared. Keep it unless scans got worse.
        SuggestionKind::AddIndex => after.identities_scanned > before.identities_scanned,
        SuggestionKind::CacheMeasure => !after.measure_cached,
        SuggestionKind::DropIndex | SuggestionKind::DropMeasure => false,
        SuggestionKind::DenormalizeLink => true,
    }
}

fn invert(suggestion: &Suggestion) -> Suggestion {
    let mut inverted = suggestion.clone();
    inverted.kind = match suggestion.kind {
        SuggestionKind::AddIndex => SuggestionKind::DropIndex,
        SuggestionKind::DropIndex => SuggestionKind::AddIndex,
        SuggestionKind::CacheMeasure => SuggestionKind::DropMeasure,
        SuggestionKind::DropMeasure => SuggestionKind::CacheMeasure,
        SuggestionKind::DenormalizeLink => SuggestionKind::DenormalizeLink,
    };
    inverted
}

fn cursor_free(request: &EvaluateRequest) -> EvaluateRequest {
    let mut request = request.clone();
    request.cursor = None;
    request
}

fn probe_for(recorder: &QueryRecorder, suggestion: &Suggestion) -> Option<EvaluateRequest> {
    recorder.samples.iter().find_map(|sample| {
        if sample.shape.namespace != suggestion.namespace {
            return None;
        }
        let matches = match suggestion.kind {
            SuggestionKind::AddIndex => {
                sample
                    .shape
                    .filter_properties
                    .iter()
                    .any(|(kind, property)| {
                        kind == &suggestion.object_kind && property == &suggestion.property
                    })
            }
            SuggestionKind::CacheMeasure => {
                sample.shape.sum_kind == suggestion.object_kind
                    && sample.shape.sum_property == suggestion.property
                    && request_uses_rollups(&sample.request)
            }
            SuggestionKind::DropIndex | SuggestionKind::DropMeasure => {
                sample.shape.root_kind == suggestion.object_kind
                    || sample.shape.sum_kind == suggestion.object_kind
                    || sample
                        .shape
                        .hops
                        .iter()
                        .any(|(far, _, _)| far == &suggestion.object_kind)
            }
            SuggestionKind::DenormalizeLink => sample.shape.hops.iter().any(|(far, join, _)| {
                far.as_str() == suggestion.far_kind.as_deref().unwrap_or("")
                    && join == &suggestion.property
            }),
        };
        matches.then(|| cursor_free(&sample.request))
    })
}

fn shape_of(request: &EvaluateRequest) -> QueryShape {
    let mut filter_properties = Vec::new();
    if let Some(filter) = &request.filter {
        filter_properties.push((request.root_kind.clone(), filter.property.clone()));
    }
    if let Some(pred) = &request.predicate {
        pred_properties(pred, &request.root_kind, &mut filter_properties);
    }
    for hop in &request.hops {
        if let Some(pred) = &hop.predicate {
            pred_properties(pred, &hop.far_kind, &mut filter_properties);
        }
    }
    let result_kind = request
        .hops
        .last()
        .map(|hop| hop.far_kind.as_str())
        .unwrap_or(request.root_kind.as_str());
    let mut sort_properties = Vec::new();
    if let Some(sort) = &request.sort {
        sort_properties.push((result_kind.to_string(), sort.property.clone()));
    }
    for sort in &request.then_by {
        sort_properties.push((result_kind.to_string(), sort.property.clone()));
    }
    QueryShape {
        namespace: request.namespace.clone(),
        root_kind: request.root_kind.clone(),
        hops: request
            .hops
            .iter()
            .map(|hop| {
                (
                    hop.far_kind.clone(),
                    hop.join_property.clone(),
                    hop.incoming,
                )
            })
            .collect(),
        filter_properties,
        sort_properties,
        sum_kind: request.sum_kind.clone(),
        sum_property: request.sum_property.clone(),
    }
}

fn pred_properties(pred: &Predicate, kind: &str, out: &mut Vec<(String, String)>) {
    match pred {
        Predicate::Eq { property, .. }
        | Predicate::Neq { property, .. }
        | Predicate::Range { property, .. }
        | Predicate::Missing { property }
        | Predicate::Bbox { property, .. } => out.push((kind.to_string(), property.clone())),
        Predicate::And(args) | Predicate::Or(args) => {
            for arg in args {
                pred_properties(arg, kind, out);
            }
        }
        Predicate::Not(inner) => pred_properties(inner, kind, out),
    }
}

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

fn pred_can_index(pred: &Predicate) -> bool {
    match pred {
        Predicate::Eq { .. } | Predicate::Range { .. } => true,
        Predicate::And(args) | Predicate::Or(args) => {
            !args.is_empty() && args.iter().all(pred_can_index)
        }
        _ => false,
    }
}

fn declared_indexes(store: &Store, namespace: &str, kind: &str) -> Result<HashSet<String>, String> {
    match store.schema(namespace, kind)? {
        Some(schema) => Ok(schema.indexes.into_iter().collect()),
        None => Ok(HashSet::new()),
    }
}

fn scan_cost(
    store: &Store,
    maps: &JoinMaps,
    request: &EvaluateRequest,
) -> Result<(usize, bool), String> {
    let mut scanned = 0usize;
    let mut used_index = false;
    let indexed = declared_indexes(store, &request.namespace, &request.root_kind)?;
    if let Some(pred) = &request.predicate {
        if pred_is_index_tree(pred, &indexed) {
            used_index = true;
            scanned +=
                indexed_match_len(store, maps, &request.namespace, &request.root_kind, pred)?;
        } else {
            scanned += maps.visible_ids(&request.root_kind).len();
        }
    } else if let Some(filter) = &request.filter {
        used_index = true;
        scanned += maps
            .matching_root_ids(&request.root_kind, &filter.property, &filter.value)
            .len();
    } else {
        scanned += maps.visible_ids(&request.root_kind).len();
    }
    let mut frontier = scanned;
    for hop in &request.hops {
        if let Some(pred) = &hop.predicate {
            let hop_indexed = declared_indexes(store, &request.namespace, &hop.far_kind)?;
            if pred_is_index_tree(pred, &hop_indexed) {
                used_index = true;
                let n = indexed_match_len(store, maps, &request.namespace, &hop.far_kind, pred)?;
                scanned += n;
                frontier = n;
            } else {
                // Parent-frontier cost, not the child walk. A far-kind index
                // that examines more candidates than this frontier rolls back;
                // counting every visible far identity kept useless indexes.
                scanned += frontier;
            }
        }
    }
    if request_uses_rollups(request)
        && !measure_table_ready(store, maps, request)?
        && !request.sum_property.is_empty()
    {
        scanned += maps.visible_ids(&request.sum_kind).len();
    }
    Ok((scanned, used_index))
}

fn indexed_match_len(
    store: &Store,
    maps: &JoinMaps,
    namespace: &str,
    kind: &str,
    pred: &Predicate,
) -> Result<usize, String> {
    Ok(indexed_match_ids(store, maps, namespace, kind, pred)?.len())
}

fn indexed_match_ids(
    store: &Store,
    maps: &JoinMaps,
    namespace: &str,
    kind: &str,
    pred: &Predicate,
) -> Result<HashSet<u32>, String> {
    match pred {
        Predicate::Eq { property, value } => Ok(maps.matching_root_ids(kind, property, value)),
        Predicate::Range { property, min, max } => {
            let ty = match store.schema(namespace, kind)? {
                Some(schema) => schema
                    .property_type(property)
                    .unwrap_or(PropertyType::String),
                None => PropertyType::String,
            };
            maps.matching_range_ids(kind, property, ty, min.as_deref(), max.as_deref())
        }
        Predicate::And(args) => {
            let mut acc: Option<HashSet<u32>> = None;
            for arg in args {
                let next = indexed_match_ids(store, maps, namespace, kind, arg)?;
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
                acc.extend(indexed_match_ids(store, maps, namespace, kind, arg)?);
            }
            Ok(acc)
        }
        _ => Ok(maps.visible_ids(kind)),
    }
}

/// Last-hop rollups are only folded for an outgoing last hop onto
/// `sum_kind` with no hop predicates (see `evaluate_local`).
fn request_uses_rollups(request: &EvaluateRequest) -> bool {
    let Some(last) = request.hops.last() else {
        return false;
    };
    !last.incoming
        && last.far_kind == request.sum_kind
        && last.join_property != request.sum_property
        && !request.sum_property.is_empty()
        && request.hops.iter().all(|hop| hop.predicate.is_none())
        && !request
            .hops
            .iter()
            .any(|hop| request.acl.hides_in_kind(&hop.far_kind))
}

fn measure_table_ready(
    store: &Store,
    maps: &JoinMaps,
    request: &EvaluateRequest,
) -> Result<bool, String> {
    if request.sum_property.is_empty() {
        return Ok(true);
    }
    if maps.has_measure_sum(&request.sum_kind, &request.sum_property) {
        return Ok(true);
    }
    Ok(store
        .schema(&request.namespace, &request.sum_kind)?
        .is_some_and(|schema| schema.sums.iter().any(|name| name == &request.sum_property)))
}

fn measure_is_cached(
    store: &Store,
    maps: &JoinMaps,
    request: &EvaluateRequest,
) -> Result<bool, String> {
    if !request_uses_rollups(request) {
        return Ok(false);
    }
    measure_table_ready(store, maps, request)
}

fn kind_count(store: &Store, namespace: &str, kind: &str) -> Result<u64, String> {
    Ok(store.joins(namespace)?.visible_ids(kind).len() as u64)
}

fn live_estimated_bytes(store: &Store, suggestion: &Suggestion) -> Result<u64, String> {
    match suggestion.kind {
        SuggestionKind::CacheMeasure => {
            let n = store
                .joins(&suggestion.namespace)?
                .measure_cache_parents(&suggestion.object_kind, &suggestion.property);
            Ok(n.saturating_mul(MEASURE_CACHE_BYTES))
        }
        SuggestionKind::DenormalizeLink => {
            let n = kind_count(store, &suggestion.namespace, &suggestion.object_kind)?;
            Ok(n.saturating_mul(DENORMALIZE_BYTES))
        }
        SuggestionKind::AddIndex | SuggestionKind::DropIndex | SuggestionKind::DropMeasure => Ok(0),
    }
}

fn suggest_add_indexes(store: &Store, recorder: &QueryRecorder) -> Result<Vec<Suggestion>, String> {
    let mut hits: HashMap<(String, String, String), u128> = HashMap::new();
    for sample in &recorder.samples {
        // Exact-match `filter` already looks up `by_prop`. Schema indexes
        // only change Eq/Range/And/Or trees; Neq/Missing/Not/Bbox stay scans.
        let mut properties = Vec::new();
        if let Some(pred) = &sample.request.predicate {
            if pred_can_index(pred) {
                pred_properties(pred, &sample.request.root_kind, &mut properties);
            }
        }
        for hop in &sample.request.hops {
            if let Some(pred) = &hop.predicate {
                if pred_can_index(pred) {
                    pred_properties(pred, &hop.far_kind, &mut properties);
                }
            }
        }
        for (kind, property) in properties {
            let indexed = declared_indexes(store, &sample.shape.namespace, &kind)?;
            if indexed.contains(&property) {
                continue;
            }
            *hits
                .entry((sample.shape.namespace.clone(), kind, property))
                .or_default() += sample.cost.latency_ns;
        }
    }
    let mut out = Vec::new();
    for ((namespace, kind, property), latency) in hits {
        let Some(schema) = store.schema(&namespace, &kind)? else {
            continue;
        };
        if !schema.properties.iter().any(|name| name == &property) {
            continue;
        }
        if schema.indexes.iter().any(|name| name == &property) {
            continue;
        }
        out.push(Suggestion {
            kind: SuggestionKind::AddIndex,
            namespace,
            object_kind: kind,
            property,
            far_kind: None,
            estimated_benefit_ns: latency,
            estimated_bytes: 0,
            reason: "predicate scans a property that is not a schema index".into(),
        });
    }
    Ok(out)
}

fn suggest_drop_indexes(
    store: &Store,
    recorder: &QueryRecorder,
) -> Result<Vec<Suggestion>, String> {
    let mut used: HashSet<(String, String, String)> = HashSet::new();
    let mut seen_kinds: HashSet<(String, String)> = HashSet::new();
    for sample in &recorder.samples {
        seen_kinds.insert((
            sample.shape.namespace.clone(),
            sample.shape.root_kind.clone(),
        ));
        for (far, _, _) in &sample.shape.hops {
            seen_kinds.insert((sample.shape.namespace.clone(), far.clone()));
        }
        for (kind, property) in sample
            .shape
            .filter_properties
            .iter()
            .chain(sample.shape.sort_properties.iter())
        {
            used.insert((
                sample.shape.namespace.clone(),
                kind.clone(),
                property.clone(),
            ));
        }
    }
    let mut out = Vec::new();
    for (namespace, kind) in seen_kinds {
        let Some(schema) = store.schema(&namespace, &kind)? else {
            continue;
        };
        for property in schema.indexes {
            if used.contains(&(namespace.clone(), kind.clone(), property.clone())) {
                continue;
            }
            out.push(Suggestion {
                kind: SuggestionKind::DropIndex,
                namespace: namespace.clone(),
                object_kind: kind.clone(),
                property,
                far_kind: None,
                estimated_benefit_ns: 0,
                estimated_bytes: 0,
                reason: "no recorded evaluate used this index in the window".into(),
            });
        }
    }
    Ok(out)
}

fn suggest_cache_measures(
    store: &Store,
    recorder: &QueryRecorder,
) -> Result<Vec<Suggestion>, String> {
    let mut hits: HashMap<(String, String, String), u128> = HashMap::new();
    for sample in &recorder.samples {
        if sample.shape.sum_property.is_empty() || !request_uses_rollups(&sample.request) {
            continue;
        }
        *hits
            .entry((
                sample.shape.namespace.clone(),
                sample.shape.sum_kind.clone(),
                sample.shape.sum_property.clone(),
            ))
            .or_default() += sample.cost.latency_ns;
    }
    let mut out = Vec::new();
    for ((namespace, kind, property), latency) in hits {
        let Some(schema) = store.schema(&namespace, &kind)? else {
            continue;
        };
        if !schema.properties.iter().any(|name| name == &property) {
            continue;
        }
        if schema.sums.iter().any(|name| name == &property) {
            continue;
        }
        if let Some(ty) = schema.property_type(&property) {
            if !matches!(
                ty,
                PropertyType::String | PropertyType::Integer | PropertyType::Decimal { .. }
            ) {
                continue;
            }
        }
        let n = store
            .joins(&namespace)?
            .measure_cache_parents(&kind, &property);
        out.push(Suggestion {
            kind: SuggestionKind::CacheMeasure,
            namespace,
            object_kind: kind,
            property,
            far_kind: None,
            estimated_benefit_ns: latency,
            estimated_bytes: n.saturating_mul(MEASURE_CACHE_BYTES),
            reason: "evaluate uses an undeclared last-hop measure".into(),
        });
    }
    Ok(out)
}

fn suggest_drop_measures(
    store: &Store,
    recorder: &QueryRecorder,
) -> Result<Vec<Suggestion>, String> {
    let mut used: HashSet<(String, String, String)> = HashSet::new();
    let mut seen_kinds: HashSet<(String, String)> = HashSet::new();
    for sample in &recorder.samples {
        seen_kinds.insert((
            sample.shape.namespace.clone(),
            sample.shape.sum_kind.clone(),
        ));
        used.insert((
            sample.shape.namespace.clone(),
            sample.shape.sum_kind.clone(),
            sample.shape.sum_property.clone(),
        ));
    }
    let mut out = Vec::new();
    for (namespace, kind) in seen_kinds {
        let Some(schema) = store.schema(&namespace, &kind)? else {
            continue;
        };
        for property in schema.sums {
            if used.contains(&(namespace.clone(), kind.clone(), property.clone())) {
                continue;
            }
            out.push(Suggestion {
                kind: SuggestionKind::DropMeasure,
                namespace: namespace.clone(),
                object_kind: kind.clone(),
                property,
                far_kind: None,
                estimated_benefit_ns: 0,
                estimated_bytes: 0,
                reason: "no recorded evaluate used this cached measure in the window".into(),
            });
        }
    }
    Ok(out)
}

fn suggest_denormalize(store: &Store, recorder: &QueryRecorder) -> Result<Vec<Suggestion>, String> {
    let mut hits: HashMap<(String, String, String, String), (u128, u64)> = HashMap::new();
    for sample in &recorder.samples {
        if sample.shape.hops.is_empty() {
            continue;
        }
        let mut near = sample.shape.root_kind.as_str();
        for (far, join, _) in &sample.shape.hops {
            let n = kind_count(store, &sample.shape.namespace, near)?;
            let entry = hits
                .entry((
                    sample.shape.namespace.clone(),
                    near.to_string(),
                    join.clone(),
                    far.clone(),
                ))
                .or_default();
            entry.0 = entry.0.saturating_add(sample.cost.latency_ns);
            entry.1 = entry.1.max(n);
            near = far.as_str();
        }
    }
    Ok(hits
        .into_iter()
        .map(
            |((namespace, object_kind, property, far_kind), (latency, n))| Suggestion {
                kind: SuggestionKind::DenormalizeLink,
                namespace,
                object_kind,
                property,
                far_kind: Some(far_kind),
                estimated_benefit_ns: latency,
                estimated_bytes: n.saturating_mul(DENORMALIZE_BYTES),
                reason: "frequent hop; a denormalized far copy is clerk-applied".into(),
            },
        )
        .collect())
}

fn edit_schema(
    store: &mut Store,
    suggestion: &Suggestion,
    edit: impl FnOnce(&mut SchemaDescriptor) -> Result<(), String>,
) -> Result<(), String> {
    let mut schema = store
        .schema(&suggestion.namespace, &suggestion.object_kind)?
        .ok_or_else(|| {
            format!(
                "no schema for {}/{}",
                suggestion.namespace, suggestion.object_kind
            )
        })?;
    edit(&mut schema)?;
    schema.version = 0;
    schema.migration = Default::default();
    store.append(schema.to_record(suggestion.namespace.clone())?)
}
