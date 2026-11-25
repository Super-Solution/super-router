//! The Super Router data plane as a process: the HTTP API, a storage backend and the configured providers.
//! It is configured through environment variables:
//!
//! | Variable | Meaning |
//! | --- | --- |
//! | `SUPERROUTER_API_KEY` | `sr_` plus 64 hexadecimal characters; the bearer token of the single owner key. Required unless `SUPERROUTER_AUTH=database` |
//! | `SUPERROUTER_DIGEST_KEY` | required, at least 32 characters; keys the idempotency digest |
//! | `SUPERROUTER_AUTH` | `database` authenticates bearer tokens against the control plane's keys in PostgreSQL (members, scoped keys, revocation) instead of the single owner key; needs `DATABASE_URL`. Keys are created by the TypeScript control plane |
//! | `DATABASE_URL` | PostgreSQL URL. Unset means the in-memory store: state is lost on restart |
//! | `ADDR` | listen address, default `127.0.0.1:3410` |
//! | `SUPERROUTER_WORKSPACE`, `SUPERROUTER_ENVIRONMENT`, `SUPERROUTER_CUSTOMER` | tenant of the owner key, default `default`, `prod`, `default` |
//! | `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY` | enable the `openai`, `anthropic` and `gemini` providers (`offering.provider` names them) |
//! | `OPENAI_BASE_URL`, `ANTHROPIC_BASE_URL`, `GEMINI_BASE_URL` | where that provider's requests go: the scheme, host and any path prefix, without the trailing `/v1` (or `/v1beta`) the provider adds itself, so `https://llm.internal/openai` becomes `https://llm.internal/openai/v1/responses`. Reaches Azure OpenAI in its v1 form, a gateway or any compatible host. Default: the vendor's own host |
//! | `SUPERROUTER_HEALTH` | `on` keeps an offering out of routing while its own failure repeats (rejected key, empty account, retired model, blocked region, rate limit) and probes it again with one request when its cooldown ends; every candidate cooling fails open. `off` or unset: nothing is skipped |
//! | `SUPERROUTER_HEALTH_RATE_LIMITED_MS`, `_QUOTA_EXHAUSTED_MS`, `_CREDENTIALS_REJECTED_MS`, `_MODEL_UNAVAILABLE_MS`, `_REGION_BLOCKED_MS`, `_MAX_MS`, `_PROBE_LEASE_MS` | the cooldown of each class, the ceiling it doubles up to and the probe lease, in milliseconds (1 to 86400000). Defaults: 5000, 300000, 300000, 600000, 600000, 1800000, 30000. Only valid with `SUPERROUTER_HEALTH=on` |
//! | `SUPERROUTER_MOCK` | `1` enables the deterministic `mock` provider and the scripted `mock-tools` one |

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super_router_contracts::{Principal, Provider};
use super_router_provider_anthropic::Anthropic;
use super_router_provider_gemini::Gemini;
use super_router_provider_openai::OpenAi;
use super_router_router::{Authenticator, DataStore, MockProvider, MockToolProvider, Router};
use super_router_routing::{HealthBook, HealthPolicy};
use super_router_server::{handler, Key, Options, StaticKeys};
use super_router_store_memory::MemoryStore;
use super_router_store_postgres::{KeyRevalidator, Keys, Options as PgOptions, PostgresStore};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// How often expired requests are settled or released, for example after a crash mid-call.
const RECOVER_EVERY: Duration = Duration::from_secs(30);
/// How long in-flight requests get to finish after a shutdown signal.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(30);
const MAX_DB_CONNECTIONS: u32 = 20;

/// Cancels `shutdown` on Ctrl-C or SIGTERM.
pub async fn signal_shutdown(shutdown: CancellationToken) {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => return ctrl_c_only(shutdown).await,
        };
        tokio::select! { _ = ctrl_c => {}, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
    shutdown.cancel();
}

#[cfg(unix)]
async fn ctrl_c_only(shutdown: CancellationToken) {
    let _ = tokio::signal::ctrl_c().await;
    shutdown.cancel();
}

/// `^sr_[a-f0-9]{64}$`
fn valid_api_key(key: &str) -> bool {
    key.strip_prefix("sr_").is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

/// Everything the environment decides, checked before anything is opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub api_key: String,
    pub digest_key: String,
    pub database_auth: bool,
    pub database_url: Option<String>,
    pub addr: String,
    pub workspace: String,
    pub environment: String,
    pub customer: String,
    pub openai_key: Option<String>,
    pub anthropic_key: Option<String>,
    pub gemini_key: Option<String>,
    /// Where each provider's requests go, without the version segment the provider adds; `None` is the vendor's host.
    pub openai_base_url: Option<String>,
    pub anthropic_base_url: Option<String>,
    pub gemini_base_url: Option<String>,
    /// `Some` when `SUPERROUTER_HEALTH=on`: the cooldowns offerings are kept out for.
    pub health: Option<HealthPolicy>,
    pub mock: bool,
}

/// The longest a configured cooldown can be: one day.
const MAX_HEALTH_MS: i64 = 86_400_000;

/// A provider base URL: `http` or `https`, a host, an optional path prefix and nothing else. The version segment is the
/// provider's own (`/v1/responses`), so a value that already ends in one is refused instead of producing `/v1/v1/...`.
/// The value is never echoed: a URL can carry credentials.
fn valid_base_url(name: &str, value: &str) -> Result<String, String> {
    let bad =
        |why: &str| format!("{name} {why} (the scheme, host and any path prefix, without the trailing /v1 or /v1beta; value not logged)");
    let rest = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
        .ok_or_else(|| bad("must start with http:// or https://"))?;
    if rest.bytes().any(|b| b.is_ascii_control() || b == b' ' || b == b'?' || b == b'#') {
        return Err(bad("must have no spaces, query or fragment"));
    }
    let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
    if authority.is_empty() || authority.contains('@') {
        return Err(bad("must name a host and carry no credentials"));
    }
    let path = path.trim_end_matches('/');
    if path.rsplit('/').next().is_some_and(|last| last == "v1" || last == "v1beta") {
        return Err(bad("must not end in the version segment, the provider adds it"));
    }
    Ok(value.trim_end_matches('/').to_string())
}

/// `SUPERROUTER_HEALTH` and the policy variables. Policy variables without `on` are an error, not a silent no-op.
fn health_policy(get: &dyn Fn(&str) -> Option<String>) -> Result<Option<HealthPolicy>, String> {
    type Field = fn(&mut HealthPolicy) -> &mut i64;
    let fields: [(&str, Field); 7] = [
        ("SUPERROUTER_HEALTH_RATE_LIMITED_MS", |p| &mut p.rate_limited_ms),
        ("SUPERROUTER_HEALTH_QUOTA_EXHAUSTED_MS", |p| &mut p.quota_exhausted_ms),
        ("SUPERROUTER_HEALTH_CREDENTIALS_REJECTED_MS", |p| &mut p.credentials_rejected_ms),
        ("SUPERROUTER_HEALTH_MODEL_UNAVAILABLE_MS", |p| &mut p.model_unavailable_ms),
        ("SUPERROUTER_HEALTH_REGION_BLOCKED_MS", |p| &mut p.region_blocked_ms),
        ("SUPERROUTER_HEALTH_MAX_MS", |p| &mut p.max_ms),
        ("SUPERROUTER_HEALTH_PROBE_LEASE_MS", |p| &mut p.probe_lease_ms),
    ];
    let on = match get("SUPERROUTER_HEALTH").as_deref() {
        None | Some("off") => false,
        Some("on") => true,
        Some(_) => return Err("SUPERROUTER_HEALTH must be \"on\" or \"off\"".into()),
    };
    let mut policy = HealthPolicy::default();
    let mut set_any = false;
    for (name, field) in fields {
        if let Some(raw) = get(name) {
            set_any = true;
            let ms: i64 = raw
                .parse()
                .ok()
                .filter(|ms| (1..=MAX_HEALTH_MS).contains(ms))
                .ok_or_else(|| format!("{name} must be a whole number of milliseconds from 1 to {MAX_HEALTH_MS}"))?;
            *field(&mut policy) = ms;
        }
    }
    if set_any && !on {
        return Err("SUPERROUTER_HEALTH_*_MS needs SUPERROUTER_HEALTH=on".into());
    }
    Ok(on.then_some(policy))
}

impl Config {
    /// Reads and validates the environment. Values are never echoed in errors.
    pub fn from_env(getenv: &dyn Fn(&str) -> Option<String>) -> Result<Self, String> {
        let get = |name: &str| getenv(name).filter(|v| !v.is_empty());
        let or = |name: &str, fallback: &str| get(name).unwrap_or_else(|| fallback.to_string());
        let auth = get("SUPERROUTER_AUTH");
        let database_auth = auth.as_deref() == Some("database");
        if auth.is_some() && !database_auth {
            return Err("SUPERROUTER_AUTH must be empty or \"database\"".into());
        }
        let database_url = get("DATABASE_URL");
        if database_auth && database_url.is_none() {
            return Err("SUPERROUTER_AUTH=database needs DATABASE_URL".into());
        }
        let (api_key, digest_key) = (get("SUPERROUTER_API_KEY").unwrap_or_default(), get("SUPERROUTER_DIGEST_KEY").unwrap_or_default());
        if (!database_auth && !valid_api_key(&api_key)) || digest_key.len() < 32 {
            return Err("set SUPERROUTER_API_KEY (sr_ plus 64 random hexadecimal characters; not needed with SUPERROUTER_AUTH=database) and SUPERROUTER_DIGEST_KEY (at least 32 characters); values are not logged".into());
        }
        Ok(Self {
            api_key,
            digest_key,
            database_auth,
            database_url,
            addr: or("ADDR", "127.0.0.1:3410"),
            workspace: or("SUPERROUTER_WORKSPACE", "default"),
            environment: or("SUPERROUTER_ENVIRONMENT", "prod"),
            customer: or("SUPERROUTER_CUSTOMER", "default"),
            openai_key: get("OPENAI_API_KEY"),
            anthropic_key: get("ANTHROPIC_API_KEY"),
            gemini_key: get("GEMINI_API_KEY"),
            openai_base_url: get("OPENAI_BASE_URL").map(|v| valid_base_url("OPENAI_BASE_URL", &v)).transpose()?,
            anthropic_base_url: get("ANTHROPIC_BASE_URL").map(|v| valid_base_url("ANTHROPIC_BASE_URL", &v)).transpose()?,
            gemini_base_url: get("GEMINI_BASE_URL").map(|v| valid_base_url("GEMINI_BASE_URL", &v)).transpose()?,
            health: health_policy(&get)?,
            mock: get("SUPERROUTER_MOCK").as_deref() == Some("1"),
        })
    }
}

/// Starts the server and returns once `shutdown` is cancelled and in-flight requests are done (or the grace
/// period ends). `ready`, when set, receives the bound address.
pub async fn run(cfg: Config, shutdown: CancellationToken, ready: Option<oneshot::Sender<SocketAddr>>) -> Result<(), String> {
    let mut windows = None;
    let (store, auth): (Arc<dyn DataStore>, Option<Arc<dyn Authenticator>>) = match &cfg.database_url {
        Some(url) => {
            let revalidate = cfg.database_auth.then(|| Arc::new(KeyRevalidator) as Arc<dyn super_router_store_postgres::Revalidate>);
            let pg = PostgresStore::connect(url, MAX_DB_CONNECTIONS, PgOptions { clock: None, revalidate })
                .await
                .map_err(|e| format!("database: {e}"))?;
            pg.migrate().await.map_err(|e| format!("migrate: {e}"))?;
            let pg = Arc::new(pg);
            let auth = cfg.database_auth.then(|| Arc::new(Keys::new(pg.pool().clone())) as Arc<dyn Authenticator>);
            windows = Some(Arc::new(PgWindows(pg.clone())) as Arc<dyn super_router_server::WindowsSource>);
            (pg, auth)
        }
        None => {
            tracing::warn!("DATABASE_URL is not set: using the in-memory store, state is lost on restart");
            (Arc::new(MemoryStore::default()), None)
        }
    };
    let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
    if let Some(key) = &cfg.openai_key {
        let mut builder = OpenAi::builder(key.clone());
        if let Some(base) = &cfg.openai_base_url {
            builder = builder.base_url(base.clone());
        }
        providers.insert("openai".into(), Arc::new(builder.build().map_err(|e| format!("openai provider: {e}"))?));
    }
    if let Some(key) = &cfg.anthropic_key {
        let mut builder = Anthropic::builder(key.clone());
        if let Some(base) = &cfg.anthropic_base_url {
            builder = builder.base_url(base.clone());
        }
        providers.insert("anthropic".into(), Arc::new(builder.build().map_err(|e| format!("anthropic provider: {e}"))?));
    }
    if let Some(key) = &cfg.gemini_key {
        let mut builder = Gemini::builder(key.clone());
        if let Some(base) = &cfg.gemini_base_url {
            builder = builder.base_url(base.clone());
        }
        providers.insert("gemini".into(), Arc::new(builder.build().map_err(|e| format!("gemini provider: {e}"))?));
    }
    if cfg.mock {
        providers.insert("mock".into(), Arc::new(MockProvider::new("success")));
        providers.insert("mock-tools".into(), Arc::new(MockToolProvider::new("success")));
    }
    if providers.is_empty() {
        tracing::warn!("no provider is configured: set OPENAI_API_KEY, ANTHROPIC_API_KEY, GEMINI_API_KEY or SUPERROUTER_MOCK=1");
    }
    let provider_count = providers.len();
    let mut router = Router::new(store.clone(), providers, &cfg.digest_key).map_err(str::to_string)?;
    if let Some(policy) = &cfg.health {
        router = router.with_health(Arc::new(Mutex::new(HealthBook::new(policy.clone()))));
    }
    let router = Arc::new(router);
    let auth = auth.unwrap_or_else(|| {
        let owner = Principal {
            workspace: cfg.workspace.clone(),
            environment: cfg.environment.clone(),
            customer: cfg.customer.clone(),
            scopes: ["invoke", "service:write", "billing:write", "billing:read", "operations:read", "routing:write"]
                .map(String::from)
                .to_vec(),
            ..Principal::default()
        };
        Arc::new(StaticKeys::new(vec![Key { secret: cfg.api_key.clone(), principal: owner }]))
    });
    let mut options = Options::new(router, store.clone(), auth);
    options.windows = windows;
    let app = handler(options);
    let listener = tokio::net::TcpListener::bind(&cfg.addr).await.map_err(|e| format!("listen on {}: {e}", cfg.addr))?;
    let local = listener.local_addr().map_err(|e| e.to_string())?;
    tokio::spawn(recover_loop(store, shutdown.clone()));
    tracing::info!(addr = %local, store = if cfg.database_url.is_some() { "postgres" } else { "memory" }, providers = provider_count, health = cfg.health.is_some(), "listening");
    if let Some(ready) = ready {
        let _ = ready.send(local);
    }
    let stopping = shutdown.clone();
    let serve = axum::serve(nodelay(listener), app).with_graceful_shutdown(async move { stopping.cancelled().await });
    let mut serve = Box::pin(async move { serve.await.map_err(|e| e.to_string()) });
    tokio::select! {
        result = &mut serve => result,
        () = shutdown.cancelled() => {
            // No write timeout exists on purpose: streaming responses legitimately stay open. Give them a grace period.
            match tokio::time::timeout(SHUTDOWN_GRACE, serve).await {
                Ok(result) => result,
                Err(_) => Ok(()),
            }
        }
    }
}

/// Sets `TCP_NODELAY` on every accepted connection. A streamed answer is many small writes (one per event); with
/// Nagle's algorithm on, the second write waits for the peer's delayed ACK, so the first events of a stream arrive
/// roughly 40 ms late on a loopback or LAN connection. Measured: 44 ms against 0.46 ms at the median
/// (`scripts/bench`).
pub fn nodelay(listener: tokio::net::TcpListener) -> impl axum::serve::Listener<Io = tokio::net::TcpStream, Addr = SocketAddr> {
    use axum::serve::ListenerExt;
    listener.tap_io(|tcp| {
        if let Err(e) = tcp.set_nodelay(true) {
            tracing::debug!(error = %e, "could not set TCP_NODELAY");
        }
    })
}

/// Settles or releases requests whose deadline passed, for example after a crash mid-call.
async fn recover_loop(store: Arc<dyn DataStore>, shutdown: CancellationToken) {
    let mut tick = tokio::time::interval(RECOVER_EVERY);
    tick.tick().await;
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            _ = tick.tick() => match store.recover().await {
                Ok(0) => {}
                Ok(count) => tracing::info!(count, "recovered expired requests"),
                Err(e) => tracing::error!(error = %e, "recover failed"),
            },
        }
    }
}

/// Serves `GET /v1/windows` from the PostgreSQL store.
struct PgWindows(Arc<PostgresStore>);

#[async_trait::async_trait]
impl super_router_server::WindowsSource for PgWindows {
    async fn windows_json(&self, p: &Principal) -> super_router_router::StoreResult<serde_json::Value> {
        let rows = self.0.windows(p).await?;
        let at = |ms: Option<i64>| {
            ms.map_or(serde_json::Value::Null, |ms| serde_json::Value::String(super_router_store_postgres::iso_millis(ms)))
        };
        Ok(serde_json::Value::Array(
            rows.iter()
                .map(|r| {
                    serde_json::json!({
                        "subscription": r.subscription, "window": r.window, "hours": r.hours, "limit": r.limit, "spent": r.spent,
                        "held": r.held, "remaining": r.remaining, "window_start": at(r.window_start), "resets_at": at(r.resets_at),
                    })
                })
                .collect(),
        ))
    }
}
