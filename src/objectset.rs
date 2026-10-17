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
    /// Point-in-box or shape-envelope intersection (ADR 0043). Bounds are
    /// canonical numbers; `min` must not exceed `max`.
    Bbox {
        property: String,
        min_lat: String,
        min_lon: String,
        max_lat: String,
        max_lon: String,
    },
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

    pub fn bbox(
        property: impl Into<String>,
        min_lat: impl Into<String>,
        min_lon: impl Into<String>,
        max_lat: impl Into<String>,
        max_lon: impl Into<String>,
    ) -> Self {
        Self::Bbox {
            property: property.into(),
            min_lat: min_lat.into(),
            min_lon: min_lon.into(),
            max_lat: max_lat.into(),
            max_lon: max_lon.into(),
        }
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
            | Self::Missing { property }
            | Self::Bbox { property, .. } => out.push(property.as_str()),
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

/// One sort key on the result kind (ADR 0015). Tie-break is `(kind, key)`;
/// [`EvaluateRequest::then_by`] adds more keys (ADR 0039).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sort {
    pub property: String,
    /// When true, reverse the typed property order. Keys still tie-break
    /// ascending. For [`Self::nearest`], reverse is farthest first.
    pub descending: bool,
    /// Canonical query vector for k-nearest ranking of a `vector:<dim>`
    /// property (ADR 0043). `page_size` is k. Absent, vector sort fails
    /// closed. Does not combine with [`EvaluateRequest::then_by`] or a cursor.
    pub nearest: Option<String>,
}

impl Sort {
    pub fn by(property: impl Into<String>) -> Self {
        Self {
            property: property.into(),
            descending: false,
            nearest: None,
        }
    }

    pub fn nearest(property: impl Into<String>, query: impl Into<String>) -> Self {
        Self {
            property: property.into(),
            descending: false,
            nearest: Some(query.into()),
        }
    }
}

/// Most groups a breakdown may return; a request may ask for fewer
/// (ADR 0038).
pub const MAX_GROUPS: usize = 1000;

/// Group-by and extra aggregates over the evaluate measure (ADR 0038). The
/// measure is the request's `(sum_kind, sum_property)`, taken once per
/// surviving path under the ADR 0018 contribution rules.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Breakdown {
    /// Root properties to group by, at most two. Empty is one group over the
    /// whole set.
    pub group_by: Vec<String>,
    /// More groups than this fails closed. Zero, or more than
    /// [`MAX_GROUPS`], means [`MAX_GROUPS`].
    pub max_groups: usize,
    /// Root property whose distinct values each group counts.
    pub distinct: Option<String>,
}

/// One group of a [`Breakdown`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    /// One part per `group_by` property; `None` is the absent group (absent
    /// or `""`).
    pub key: Vec<Option<String>>,
    /// Distinct visible roots with a surviving path.
    pub count: usize,
    /// ADR 0018 sum: wraps on overflow.
    pub sum: i64,
    /// Contributing measure values, once per path.
    pub n: u64,
    pub min: Option<i64>,
    pub max: Option<i64>,
    /// Exact average as `(sum, n)` with a checked sum; the caller divides.
    pub avg: (i64, u64),
    /// Distinct canonical values of [`Breakdown::distinct`], when asked.
    pub distinct: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct EvaluateRequest {
    pub namespace: String,
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
    /// Further sort keys after [`Self::sort`], at most two, each with its own
    /// direction; ties on every key break on `(kind, key)` (ADR 0039).
    /// Requires `sort`.
    pub then_by: Vec<Sort>,
    /// Page length when [`Self::sort`] is set. Zero returns the full bounded
    /// set. Exceeding [`Self::object_bound`] still fails closed.
    pub page_size: usize,
    /// Opaque snapshot cursor. Binds restriction, query, and the live
    /// writer stamp. A write or a different view fails closed.
    pub cursor: Option<String>,
    /// Groups and extra aggregates; `None` keeps count/sum only.
    pub breakdown: Option<Breakdown>,
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
    /// Groups in key order (`None` parts last) when a breakdown was asked.
    pub groups: Vec<Group>,
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
