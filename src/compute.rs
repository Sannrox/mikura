use crate::acl::AclError;
use crate::objectset::{Aggregate, EvaluateRequest, EvaluateResponse};
use crate::store::Store;

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

fn evaluate_local(
    store: &Store,
    request: &EvaluateRequest,
) -> Result<EvaluateResponse, ComputeError> {
    let hops = hop_triples(request);
    let (two_hop_count, sum_amount) = match &request.filter {
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
    let objects = if request.object_bound == 0 {
        Vec::new()
    } else {
        let filter = request
            .filter
            .as_ref()
            .map(|filter| (filter.property.as_str(), filter.value.as_str()));
        let keys = store.joins().result_keys(&request.root_kind, &hops, filter);
        if keys.len() > request.object_bound {
            return Err(ComputeError::ObjectBound {
                bound: request.object_bound,
                count: keys.len(),
            });
        }
        let kind = result_kind(request);
        let mut objects = Vec::with_capacity(keys.len());
        for key in keys {
            objects.push(
                store
                    .load(kind, &key, &request.acl)
                    .map_err(ComputeError::Load)?,
            );
        }
        objects
    };
    Ok(EvaluateResponse {
        two_hop_count,
        sum_amount,
        objects,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComputeError {
    Acl(AclError),
    UnsupportedBackend { name: &'static str },
    ObjectBound { bound: usize, count: usize },
    Load(String),
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
                if let Some(filter) = &request.filter {
                    request
                        .acl
                        .check(&request.root_kind, &filter.property)
                        .map_err(ComputeError::Acl)?;
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
