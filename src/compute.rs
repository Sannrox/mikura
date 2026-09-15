use crate::acl::AclError;
use crate::objectset::{Aggregate, EvaluateRequest, EvaluateResponse};
use crate::store::Store;

fn evaluate_local(store: &Store, request: &EvaluateRequest) -> EvaluateResponse {
    let hops: Vec<(&str, &str)> = request
        .hops
        .iter()
        .map(|hop| (hop.far_kind.as_str(), hop.join_property.as_str()))
        .collect();
    let (two_hop_count, sum_amount) = store.joins().count_and_sum(
        &request.root_kind,
        &hops,
        &request.sum_kind,
        &request.sum_property,
    );
    EvaluateResponse {
        two_hop_count,
        sum_amount,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComputeError {
    Acl(AclError),
    UnsupportedBackend { name: &'static str },
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
                Ok(evaluate_local(store, request))
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
