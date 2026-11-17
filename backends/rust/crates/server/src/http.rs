//! Wire helpers: JSON responses, bounded body reads, path decoding and server-sent-event frames.

use axum::body::{Body, Bytes, HttpBody};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use http_body_util::BodyExt;
use serde::de::IgnoredAny;
use serde_json::{Map, Value};
use super_router_contracts::{RouterError, StreamEvent};
use super_router_router::{InferError, StoreError};

use crate::MAX_BODY_BYTES;

/// A failure on its way to the client: the error, the request it belongs to once one exists, and for internal
/// failures the text that is logged but never sent.
#[derive(Debug)]
pub(crate) struct ApiError {
    pub error: RouterError,
    pub request_id: Option<String>,
    pub internal: Option<String>,
}

impl ApiError {
    pub fn new(code: &str, status: u16) -> Self {
        RouterError::new(code, status).into()
    }
}

impl From<RouterError> for ApiError {
    fn from(error: RouterError) -> Self {
        Self { error, request_id: None, internal: None }
    }
}

impl From<StoreError> for ApiError {
    /// Anything that is not a `RouterError` reaches the client as a bare `internal_error`.
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::Router(error) => error.into(),
            StoreError::Internal(text) => Self { error: RouterError::new("internal_error", 500), request_id: None, internal: Some(text) },
        }
    }
}

impl From<InferError> for ApiError {
    fn from(e: InferError) -> Self {
        Self { error: e.error, request_id: e.request_id, internal: None }
    }
}

/// `{"error":{"code","retryable","request_id"?,"details"?}}` with the error's status.
pub(crate) fn error_response(e: &ApiError) -> Response {
    if e.error.status >= 500 {
        tracing::error!(code = %e.error.code, request_id = e.request_id.as_deref().unwrap_or(""), error = e.internal.as_deref().unwrap_or(""), "request failed");
    }
    let mut body = Map::new();
    body.insert("code".into(), Value::String(e.error.code.clone()));
    body.insert("retryable".into(), Value::Bool(e.error.retryable));
    if let Some(id) = e.request_id.as_deref().filter(|id| !id.is_empty()) {
        body.insert("request_id".into(), Value::String(id.to_string()));
    }
    if let Some(details) = &e.error.details {
        body.insert("details".into(), Value::Object(details.clone()));
    }
    json(StatusCode::from_u16(e.error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), &serde_json::json!({ "error": body }))
}

/// A JSON response. The body ends with a newline, like the Go and TypeScript servers' encoders write it.
pub(crate) fn json(status: StatusCode, value: &Value) -> Response {
    let mut raw = serde_json::to_vec(value).unwrap_or_else(|_| br#"{"error":{"code":"internal_error","retryable":false}}"#.to_vec());
    raw.push(b'\n');
    let mut res = Response::new(Body::from(raw));
    *res.status_mut() = status;
    res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    res
}

/// A double the way the reference servers print it: integral values without a fraction.
pub(crate) fn number(x: f64) -> Value {
    if x.fract() == 0.0 && x.abs() < 9_007_199_254_740_992.0 {
        Value::from(x as i64)
    } else {
        serde_json::Number::from_f64(x).map_or(Value::Null, Value::Number)
    }
}

/// The request path with percent-escapes decoded, as Go's `URL.Path` holds it.
pub(crate) fn decode_path(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = |b: u8| char::from(b).to_digit(16);
            if let (Some(hi), Some(lo)) = (bytes.get(i + 1).and_then(|b| hex(*b)), bytes.get(i + 2).and_then(|b| hex(*b))) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn declared_length(headers: &HeaderMap, body: &Body) -> Option<u64> {
    let header = headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
    header.or_else(|| HttpBody::size_hint(body).exact())
}

/// Reads a JSON body of at most [`MAX_BODY_BYTES`]. A declared length over the limit is refused without reading;
/// otherwise the body is read frame by frame and abandoned the moment it passes the limit, so a chunked body of
/// unknown length can never make the process buffer more than the limit.
pub(crate) async fn read_body(headers: &HeaderMap, mut body: Body) -> Result<Bytes, ApiError> {
    let too_large = || ApiError::new("body_too_large", 413);
    if declared_length(headers, &body).is_some_and(|n| n > MAX_BODY_BYTES as u64) {
        return Err(too_large());
    }
    let mut raw: Vec<u8> = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| ApiError::new("invalid_json", 400))?;
        if let Ok(data) = frame.into_data() {
            if raw.len() + data.len() > MAX_BODY_BYTES {
                return Err(too_large());
            }
            raw.extend_from_slice(&data);
        }
    }
    if serde_json::from_slice::<IgnoredAny>(&raw).is_err() {
        return Err(ApiError::new("invalid_json", 400));
    }
    Ok(Bytes::from(raw))
}

/// One server-sent event: `event: <kind>` and a single `data:` line of compact JSON.
pub(crate) fn sse_frame(kind: &str, data: &Map<String, Value>) -> Bytes {
    let raw = serde_json::to_string(data).unwrap_or_else(|_| "{}".to_string());
    Bytes::from(format!("event: {kind}\ndata: {raw}\n\n"))
}

/// The payload of a stream event: its type, request id and sequence, plus whatever the event carries.
pub(crate) fn event_frame(e: &StreamEvent) -> Bytes {
    let mut data = Map::new();
    data.insert("type".into(), Value::String(e.kind.clone()));
    data.insert("request_id".into(), Value::String(e.request_id.clone()));
    data.insert("sequence".into(), Value::from(e.sequence));
    if let Value::Object(extra) = &e.data {
        for (k, v) in extra {
            data.insert(k.clone(), v.clone());
        }
    }
    sse_frame(&e.kind, &data)
}
