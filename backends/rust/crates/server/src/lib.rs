//! The HTTP API of the Super Router data plane as an [`axum::Router`].
//!
//! [`handler`] returns a plain router, so it mounts on any axum, hyper or tower stack (`Router::nest`,
//! `Router::merge`, `axum::serve`). Authentication is the [`Authenticator`] port and storage is [`DataStore`].
//! Routes, responses and error codes match the Go and TypeScript reference servers: `GET /healthz` needs no
//! credentials, everything else needs a bearer token, and every `POST` needs an `Idempotency-Key`.
//!
//! Requests are routed by hand, as the Go server does, so an unknown path or a wrong method is always the same
//! `resource_not_found` (404) JSON error, after authentication.

mod auth;
mod http;
mod time;

use std::convert::Infallible;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::Response;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use super_router_contracts::{parse_invocation, parse_service, Principal, StreamEvent};
use super_router_router::{
    publish_credit_rate_version, Authenticator, DataStore, Emit, EmitClosed, InferError, InferResult, Router, StoreResult,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, DropGuard};

pub use auth::{Key, StaticKeys};

use crate::http::{decode_path, error_response, event_frame, json, number, read_body, sse_frame, ApiError};
use crate::time::parse_rfc3339_millis;

/// The most one request body may hold.
pub const MAX_BODY_BYTES: usize = 256_000;

/// Reports the session and weekly usage windows of a principal for `GET /v1/windows`. Stores that track them
/// implement it; without one the endpoint answers `resource_not_found`.
#[async_trait]
pub trait WindowsSource: Send + Sync {
    async fn windows_json(&self, p: &Principal) -> StoreResult<Value>;
}

/// Configures [`handler`].
pub struct Options {
    pub router: Arc<Router>,
    pub store: Arc<dyn DataStore>,
    pub auth: Arc<dyn Authenticator>,
    /// `self-host` (the default when empty) or `cloud`. Cloud mode forbids bring-your-own-key offerings and
    /// protects platform pricing from API keys.
    pub mode: String,
    /// Backs `GET /v1/windows`, when the store has windows.
    pub windows: Option<Arc<dyn WindowsSource>>,
}

impl Options {
    pub fn new(router: Arc<Router>, store: Arc<dyn DataStore>, auth: Arc<dyn Authenticator>) -> Self {
        Self { router, store, auth, mode: String::new(), windows: None }
    }
}

struct Shared {
    router: Arc<Router>,
    store: Arc<dyn DataStore>,
    auth: Arc<dyn Authenticator>,
    mode: String,
    windows: Option<Arc<dyn WindowsSource>>,
}

/// Builds the HTTP API.
pub fn handler(options: Options) -> axum::Router {
    let mode = if options.mode.is_empty() { "self-host".to_string() } else { options.mode };
    let shared = Arc::new(Shared { router: options.router, store: options.store, auth: options.auth, mode, windows: options.windows });
    axum::Router::new().fallback(serve).with_state(shared)
}

async fn serve(State(h): State<Arc<Shared>>, req: Request) -> Response {
    if req.method() == Method::GET && req.uri().path() == "/healthz" {
        return json(StatusCode::OK, &json!({ "status": "ok" }));
    }
    match h.route(req).await {
        Ok(response) => response,
        Err(e) => error_response(&e),
    }
}

fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
}

fn require(p: &Principal, scope: &str) -> Result<(), ApiError> {
    if p.has_scope(scope) {
        Ok(())
    } else {
        Err(ApiError::new("scope_denied", 403))
    }
}

impl Shared {
    async fn route(&self, req: Request) -> Result<Response, ApiError> {
        let (parts, body) = req.into_parts();
        let authorization = header_text(&parts.headers, "authorization");
        let token = authorization.strip_prefix("Bearer ").unwrap_or(authorization);
        let p = self.auth.authenticate(token).await?;
        let key = header_text(&parts.headers, "idempotency-key").to_string();
        let method = &parts.method;
        if *method == Method::POST && key.is_empty() {
            return Err(ApiError::new("missing_idempotency_key", 400));
        }
        let path = decode_path(parts.uri.path());
        let segments: Vec<&str> = path.trim_matches('/').split('/').collect();

        if *method == Method::GET && path == "/v1/services" {
            require(&p, "invoke")?;
            let data: Vec<Value> = self
                .store
                .list(&p)
                .await?
                .into_iter()
                .map(|s| {
                    let mut entry = Map::new();
                    entry.insert("id".into(), json!(s.id));
                    entry.insert("version".into(), json!(s.version));
                    if !s.tier.is_empty() {
                        entry.insert("tier".into(), json!(s.tier));
                    }
                    entry.insert("max_output_tokens".into(), json!(s.max_output_tokens));
                    entry.insert("price".into(), serde_json::to_value(&s.price).unwrap_or(Value::Null));
                    Value::Object(entry)
                })
                .collect();
            return Ok(json(StatusCode::OK, &json!({ "data": data, "next_cursor": null })));
        }
        if *method == Method::POST && path == "/v1/inferences" {
            let raw = read_body(&parts.headers, body).await?;
            return self.infer(p, raw, key).await;
        }
        if segments.len() >= 3 && segments[0] == "v1" && segments[1] == "requests" && segments.len() <= 4 {
            let id = segments[2];
            if segments.len() == 4 && segments[3] == "cancel" && *method == Method::POST {
                require(&p, "invoke")?;
                self.store.cancel(&p, id).await?;
                return Ok(json(StatusCode::ACCEPTED, &json!({ "id": id, "cancel_requested": true })));
            }
            if segments.len() == 3 && *method == Method::GET {
                require(&p, "invoke")?;
                let rec = self.store.get(&p, id).await?;
                return Ok(json(
                    StatusCode::OK,
                    &json!({ "id": rec.id, "execution_status": rec.execution, "billing_status": rec.billing, "service_version": rec.service.version }),
                ));
            }
            // Anything else under /v1/requests is unknown.
            return Err(ApiError::new("resource_not_found", 404));
        }
        if *method == Method::GET && path == "/v1/windows" {
            if !p.has_scope("credits:read") && !p.has_scope("billing:read") {
                return Err(ApiError::new("scope_denied", 403));
            }
            let Some(windows) = &self.windows else {
                return Err(ApiError::new("resource_not_found", 404));
            };
            return Ok(json(StatusCode::OK, &json!({ "windows": windows.windows_json(&p).await? })));
        }
        if *method == Method::GET && path == "/v1/admin/balance" {
            require(&p, "billing:read")?;
            let rows = self.store.balance(&p).await?;
            return Ok(json(StatusCode::OK, &json!({ "data": rows })));
        }
        if *method == Method::POST
            && segments.len() == 5
            && segments[0] == "v1"
            && segments[1] == "admin"
            && segments[2] == "services"
            && segments[4] == "credit-rates"
        {
            let raw = read_body(&parts.headers, body).await?;
            return self.credit_rates(&p, segments[3], &raw).await;
        }
        if *method == Method::POST && path == "/v1/admin/credits/grants" {
            let raw = read_body(&parts.headers, body).await?;
            return self.grant(&p, &raw, &key).await;
        }
        if *method == Method::POST && path == "/v1/admin/service-versions" {
            let raw = read_body(&parts.headers, body).await?;
            let service = parse_service(&raw)?;
            let published = self.store.publish(&p, &service, &self.mode, None).await?;
            return Ok(json(StatusCode::CREATED, &serde_json::to_value(&published).unwrap_or(Value::Null)));
        }
        if *method == Method::POST && path == "/v1/admin/routing/dry-run" {
            require(&p, "routing:write")?;
            let raw = read_body(&parts.headers, body).await?;
            let input = parse_invocation(&raw)?;
            let service = self.store.get_service(&p, &input.service, input.service_version).await?;
            let (candidates, excluded) = super_router_routing::route(&service, &input)?;
            let candidates: Vec<Value> =
                candidates.iter().map(|c| json!({ "id": c.offering.id, "score": number(c.score), "cost": c.cost.to_string() })).collect();
            return Ok(json(StatusCode::OK, &json!({ "excluded": excluded, "candidates": candidates })));
        }
        Err(ApiError::new("resource_not_found", 404))
    }

    /// Funds the principal's own customer (or the platform account) from the request body. The
    /// `Idempotency-Key` names the grant, so a retried request never funds twice.
    async fn grant(&self, p: &Principal, raw: &[u8], key: &str) -> Result<Response, ApiError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Body {
            kind: Option<String>,
            asset: Option<String>,
            amount: Option<String>,
            expires_at: Option<String>,
        }
        let invalid = || ApiError::new("invalid_request", 400);
        let body: Body = serde_json::from_slice(raw).map_err(|_| invalid())?;
        let (asset, amount) = match (body.asset.as_deref(), body.amount.as_deref()) {
            (Some(a), Some(n)) if !a.is_empty() && !n.is_empty() => (a, n),
            _ => return Err(invalid()),
        };
        let kind = body.kind.as_deref().filter(|k| !k.is_empty()).unwrap_or("customer_sale");
        if kind != "customer_sale" && kind != "platform_sale" {
            return Err(invalid());
        }
        let expires = match body.expires_at.as_deref() {
            None => None,
            Some(text) => Some(parse_rfc3339_millis(text).ok_or_else(invalid)?),
        };
        let account = self.store.grant(p, kind, asset, amount, &format!("http:{key}"), expires).await?;
        Ok(json(StatusCode::CREATED, &json!({ "account": account })))
    }

    async fn credit_rates(&self, p: &Principal, id: &str, raw: &[u8]) -> Result<Response, ApiError> {
        let doc = match serde_json::from_slice::<Value>(raw) {
            Ok(Value::Object(doc)) => doc,
            _ => return Err(ApiError::new("invalid_credit_rates", 400)),
        };
        if doc.keys().any(|k| k != "expected_version" && k != "config") {
            return Err(ApiError::new("invalid_credit_rates", 400));
        }
        let Some(version) = doc.get("expected_version").and_then(Value::as_i64) else {
            return Err(ApiError::new("invalid_service_version", 400));
        };
        let config = doc.get("config").unwrap_or(&Value::Null);
        let service = publish_credit_rate_version(&*self.store, p, id, version, config, &self.mode).await?;
        Ok(json(StatusCode::CREATED, &serde_json::to_value(&service).unwrap_or(Value::Null)))
    }

    /// Answers a non-streaming request with JSON and a streaming one with server-sent events. A streaming
    /// request only turns into an event stream once the request exists: earlier failures are ordinary JSON errors.
    ///
    /// The inference runs in its own task. If the client goes away, hyper drops this future, which cancels the
    /// provider call through the guard below; the task itself is never aborted, so the books are always settled
    /// or left pending, never lost.
    async fn infer(&self, p: Principal, raw: Bytes, key: String) -> Result<Response, ApiError> {
        let streaming = serde_json::from_slice::<Value>(&raw).ok().and_then(|v| v.get("stream").and_then(Value::as_bool)).unwrap_or(false);
        let cancel = CancellationToken::new();
        let guard = cancel.clone().drop_guard();
        let (events, mut feed) = mpsc::unbounded_channel::<StreamEvent>();
        let emit: Option<Emit> = streaming.then(|| Box::new(move |e| events.send(e).map_err(|_| EmitClosed)) as Emit);
        let router = self.router.clone();
        let mut task: JoinHandle<Result<InferResult, InferError>> =
            tokio::spawn(async move { router.infer(&p, &raw, &key, emit, cancel).await });

        if streaming {
            // The sender lives inside the inference, so the feed ends exactly when the inference does.
            if let Some(first) = feed.recv().await {
                return Ok(event_stream(first, feed, task, guard));
            }
        }
        let result = (&mut task).await.map_err(|_| ApiError::new("internal_error", 500))??;
        drop(guard);
        if result.replayed {
            let mut res = Response::new(Body::empty());
            *res.status_mut() = StatusCode::SEE_OTHER;
            if let Ok(location) = HeaderValue::from_str(&format!("/v1/requests/{}", result.id)) {
                res.headers_mut().insert(header::LOCATION, location);
            }
            return Ok(res);
        }
        let mut res = json(StatusCode::OK, &serde_json::to_value(&result).unwrap_or(Value::Null));
        if let Ok(id) = HeaderValue::from_str(&result.id) {
            res.headers_mut().insert("x-request-id", id);
        }
        Ok(res)
    }
}

/// The event stream of a request that exists. When the inference fails after the stream opened, the failure is
/// the stream's last event. Dropping the body (a client that went away) cancels the provider call and closes the
/// feed, which stops the provider and records the outcome as uncertain.
fn event_stream(
    first: StreamEvent,
    mut feed: mpsc::UnboundedReceiver<StreamEvent>,
    task: JoinHandle<Result<InferResult, InferError>>,
    guard: DropGuard,
) -> Response {
    let request_id = first.request_id.clone();
    let opening = event_frame(&first);
    let body = Body::from_stream(async_stream::stream! {
        let _guard = guard;
        yield Ok::<Bytes, Infallible>(opening);
        while let Some(event) = feed.recv().await {
            yield Ok(event_frame(&event));
        }
        if let Ok(Err(failure)) = task.await {
            let mut data = Map::new();
            data.insert("type".into(), json!("request.failed"));
            data.insert("request_id".into(), json!(request_id));
            data.insert("error".into(), json!({ "code": failure.error.code }));
            yield Ok(sse_frame("request.failed", &data));
        }
    });
    let mut res = Response::new(body);
    let headers = res.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    if let Ok(id) = HeaderValue::from_str(&first.request_id) {
        headers.insert("x-request-id", id);
    }
    res
}
