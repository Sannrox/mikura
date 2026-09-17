use crate::acl::PropertyAcl;
use crate::compute::{ComputeBackend, ComputeError};
use crate::store::Store;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aggregate {
    /// Distinct reachable roots plus sum of a numeric leaf property.
    CountAndSum,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hop {
    pub far_kind: String,
    pub join_property: String,
    /// When true, follow `props[join_property]` on the frontier to `far_kind`.
    /// When false, find `far_kind` rows whose `join_property` equals the frontier key.
    pub incoming: bool,
}

/// Exact equality on a root property. No phrase, prefix, or query language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactMatch {
    pub property: String,
    pub value: String,
}

#[derive(Clone, Debug)]
pub struct EvaluateRequest {
    pub root_kind: String,
    pub hops: Vec<Hop>,
    pub sum_kind: String,
    pub sum_property: String,
    pub aggregate: Aggregate,
    pub acl: PropertyAcl,
    /// When set, only visible roots whose `props[property] == value` survive.
    pub filter: Option<ExactMatch>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvaluateResponse {
    /// Distinct root keys in surviving hop paths. The name is historical;
    /// hop count is `EvaluateRequest.hops.len()`, which may be zero.
    pub two_hop_count: usize,
    pub sum_amount: i64,
}

pub struct ObjectSet<B> {
    backend: B,
}

impl<B: ComputeBackend> ObjectSet<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    pub fn evaluate(
        &self,
        store: &Store,
        request: &EvaluateRequest,
    ) -> Result<EvaluateResponse, ComputeError> {
        self.backend.evaluate(store, request)
    }
}
