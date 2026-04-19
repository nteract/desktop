//! Error conversion helpers.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum NodeError {
    #[error("{0}")]
    Message(String),
}

impl From<NodeError> for napi::Error {
    fn from(e: NodeError) -> Self {
        napi::Error::from_reason(e.to_string())
    }
}

pub fn to_napi_err<E: std::fmt::Display>(e: E) -> napi::Error {
    napi::Error::from_reason(e.to_string())
}
