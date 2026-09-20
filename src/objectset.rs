use crate::acl::PropertyAcl;
use crate::compute::{ComputeBackend, ComputeError};
use crate::store::{ObjectRecord, Store};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Aggregate {
    /// Distinct reachable roots plus sum of a numeric leaf property.
    #[default]
    CountAndSum,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hop {
    pub far_kind: String,
    pub join_property: String,
    /// When true, follow `props[join_property]` on the frontier to `far_kind`.
    /// When false, find `far_kind` rows whose `join_property` equals the frontier key.
    pub incoming: bool,
    /// Restrict the far set after this hop (ADR 0015). `None` keeps every
    /// visible far identity the join produced.
    pub predicate: Option<Predicate>,
}

/// Exact equality on a root property. No phrase, prefix, or query language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactMatch {
    pub property: String,
    pub value: String,
}

/// Structured selection on the current kind (ADR 0015).
///
/// There is no query language. Unsupported operators are not in this enum;
/// the host wire fails closed on an unknown `op` tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Predicate {
    Eq {
        property: String,
        value: String,
    },
    Neq {
        property: String,
        value: String,
    },
    Range {
        property: String,
        min: Option<String>,
        max: Option<String>,
    },
    Missing {
        property: String,
    },
    And(Vec<Predicate>),
    Or(Vec<Predicate>),
    Not(Box<Predicate>),
}

impl Predicate {
    pub fn eq(property: impl Into<String>, value: impl Into<String>) -> Self {
        Self::Eq {
            property: property.into(),
            value: value.into(),
        }
    }

    pub fn neq(property: impl Into<String>, value: impl Into<String>) -> Self {
        Self::Neq {
            property: property.into(),
            value: value.into(),
        }
    }

    pub fn range(
        property: impl Into<String>,
        min: Option<impl Into<String>>,
        max: Option<impl Into<String>>,
    ) -> Self {
        Self::Range {
            property: property.into(),
            min: min.map(Into::into),
            max: max.map(Into::into),
        }
    }

    pub fn missing(property: impl Into<String>) -> Self {
        Self::Missing {
            property: property.into(),
        }
    }

    pub fn and(args: Vec<Predicate>) -> Self {
        Self::And(args)
    }

    pub fn or(args: Vec<Predicate>) -> Self {
        Self::Or(args)
    }

    pub fn properties(&self) -> Vec<&str> {
        let mut out = Vec::new();
        self.collect_properties(&mut out);
        out
    }

    fn collect_properties<'a>(&'a self, out: &mut Vec<&'a str>) {
        match self {
            Self::Eq { property, .. }
            | Self::Neq { property, .. }
            | Self::Range { property, .. }
            | Self::Missing { property } => out.push(property.as_str()),
            Self::And(args) | Self::Or(args) => {
                for arg in args {
                    arg.collect_properties(out);
                }
            }
            Self::Not(inner) => inner.collect_properties(out),
        }
    }
}

impl std::ops::Not for Predicate {
    type Output = Self;

    fn not(self) -> Self {
        Self::Not(Box::new(self))
    }
}

/// One sort property on the result kind (ADR 0015). Tie-break is `(kind, key)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sort {
    pub property: String,
    /// When true, reverse the typed property order. Keys still tie-break
    /// ascending.
    pub descending: bool,
}

impl Sort {
    pub fn by(property: impl Into<String>) -> Self {
        Self {
            property: property.into(),
            descending: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct EvaluateRequest {
    pub root_kind: String,
    pub hops: Vec<Hop>,
    pub sum_kind: String,
    pub sum_property: String,
    pub aggregate: Aggregate,
    pub acl: PropertyAcl,
    /// When set, only visible roots whose `props[property] == value` survive.
    /// Product-loop shorthand; do not combine with [`Self::predicate`].
    pub filter: Option<ExactMatch>,
    /// Composed selection on `root_kind` (ADR 0015). Evaluated after
    /// visibility and before hops.
    pub predicate: Option<Predicate>,
    /// When greater than zero, return distinct result objects up to this
    /// many identities. Exceeding the bound fails closed. Zero keeps today's
    /// count/sum-only response.
    pub object_bound: usize,
    /// Typed order on the result kind. Absent keeps intern-string key order,
    /// which is not a product sort.
    pub sort: Option<Sort>,
    /// Page length when [`Self::sort`] is set. Zero returns the full bounded
    /// set. Exceeding [`Self::object_bound`] still fails closed.
    pub page_size: usize,
    /// Opaque snapshot cursor. Binds restriction, query, and the live
    /// writer stamp. A write or a different view fails closed.
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvaluateResponse {
    /// Distinct root keys in surviving hop paths. The name is historical;
    /// hop count is `EvaluateRequest.hops.len()`, which may be zero.
    pub two_hop_count: usize,
    pub sum_amount: i64,
    /// Distinct current objects of the last hop's `far_kind`, or of
    /// `root_kind` when there are no hops. Empty when `object_bound` is 0.
    /// With [`EvaluateRequest::sort`] and [`EvaluateRequest::page_size`],
    /// this is one snapshot page.
    pub objects: Vec<ObjectRecord>,
    /// Continuation for the next snapshot page. Absent on the final page
    /// and when paging is not requested.
    pub cursor: Option<String>,
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
