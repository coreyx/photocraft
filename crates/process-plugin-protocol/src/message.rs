//! Frame headers: requests, responses, errors and events.
//!
//! Requests carry an `id` chosen by the sender; the response or error to it echoes the id, so
//! either side can have several requests in flight. Events have no id and expect no answer.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::method::Method;

/// The JSON header of a frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Header {
    /// Ask the other side to do something; answered by `Response` or `Error` with the same id.
    Request {
        id: u64,
        method: String,
        #[serde(default)]
        params: Value,
    },
    /// Successful answer to a request.
    Response {
        id: u64,
        #[serde(default)]
        result: Value,
    },
    /// Failed answer to a request.
    Error { id: u64, error: ErrorBody },
    /// One-way notification (document changes, user input on a plug-in panel, ...).
    Event {
        topic: String,
        #[serde(default)]
        data: Value,
    },
}

impl Header {
    /// Build a request for method `M` with typed parameters.
    pub fn request<M: Method>(id: u64, params: &M::Params) -> Result<Header, ErrorBody> {
        let params = serde_json::to_value(params).map_err(|e| ErrorBody::new(ErrorCode::Invalid, e.to_string()))?;
        Ok(Header::Request { id, method: M::NAME.to_owned(), params })
    }

    /// Build a successful response with a typed result.
    pub fn response<T: Serialize>(id: u64, result: &T) -> Result<Header, ErrorBody> {
        let result = serde_json::to_value(result).map_err(|e| ErrorBody::new(ErrorCode::Internal, e.to_string()))?;
        Ok(Header::Response { id, result })
    }

    /// Build an error response.
    pub fn error(id: u64, code: ErrorCode, message: impl Into<String>) -> Header {
        Header::Error { id, error: ErrorBody::new(code, message) }
    }

    /// Build an event with typed data.
    pub fn event<T: Serialize>(topic: &str, data: &T) -> Result<Header, ErrorBody> {
        let data = serde_json::to_value(data).map_err(|e| ErrorBody::new(ErrorCode::Internal, e.to_string()))?;
        Ok(Header::Event { topic: topic.to_owned(), data })
    }

    /// The request id this header carries or answers, if any.
    pub fn id(&self) -> Option<u64> {
        match self {
            Header::Request { id, .. } | Header::Response { id, .. } | Header::Error { id, .. } => Some(*id),
            Header::Event { .. } => None,
        }
    }
}

/// Decode a request's parameters as method `M` expects them.
pub fn params<M: Method>(params: &Value) -> Result<M::Params, ErrorBody> {
    decode_value(params, ErrorCode::Invalid)
}

/// Decode a response's result as method `M` returns it.
pub fn result<M: Method>(result: &Value) -> Result<M::Result, ErrorBody> {
    decode_value(result, ErrorCode::Internal)
}

/// Decode any JSON value into a typed value; a mismatch becomes an error with `code`.
pub fn decode_value<T: DeserializeOwned>(value: &Value, code: ErrorCode) -> Result<T, ErrorBody> {
    // `null` stands for "no parameters", which a struct with all-default fields accepts.
    let value = if value.is_null() { &Value::Object(serde_json::Map::new()) } else { value };
    T::deserialize(value).map_err(|e| ErrorBody::new(code, e.to_string()))
}

/// Why a request failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// A document, layer, panel or other object named in the request doesn't exist (any more).
    NotFound,
    /// The parameters are malformed or out of range.
    Invalid,
    /// The request is understood but not supported here (a colour mode that can't be converted,
    /// an unknown method, a protocol version this side doesn't speak).
    Unsupported,
    /// The target is busy (for example a document locked by a running job); retrying may work.
    Busy,
    /// The data is larger than allowed.
    TooLarge,
    /// The request was cancelled.
    Cancelled,
    /// A bug or unexpected failure on the answering side.
    Internal,
}

/// The body of an `Error` header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
}

impl ErrorBody {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        ErrorBody { code, message: message.into() }
    }
}
