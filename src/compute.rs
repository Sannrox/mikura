use crate::acl::AclError;
use crate::objectset::{Aggregate, EvaluateRequest, EvaluateResponse};
use crate::store::Store;

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
                    .check("Shipment", "amount")
                    .map_err(ComputeError::Acl)?;
                Ok(EvaluateResponse {
                    two_hop_count: store.joins().hop_count(),
                    sum_amount: store.joins().sum_amount(),
                })
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
