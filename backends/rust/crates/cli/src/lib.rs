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
//! | `OPENAI_API_KEY`, `ANTHROPIC_API_KEY` | enable the `openai` and `anthropic` providers (`offering.provider` names them) |
//! | `SUPERROUTER_MOCK` | `1` enables the deterministic `mock` provider and the scripted `mock-tools` one |

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use super_router_contracts::{Principal, Provider};
use super_router_provider_anthropic::Anthropic;
use super_router_provider_openai::OpenAi;
use super_router_router::{Authenticator, DataStore, MockProvider, MockToolProvider, Router};
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
    pub mock: bool,
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
        providers.insert("openai".into(), Arc::new(OpenAi::builder(key.clone()).build().map_err(|e| format!("openai provider: {e}"))?));
    }
    if let Some(key) = &cfg.anthropic_key {
        providers
            .insert("anthropic".into(), Arc::new(Anthropic::builder(key.clone()).build().map_err(|e| format!("anthropic provider: {e}"))?));
    }
    if cfg.mock {
        providers.insert("mock".into(), Arc::new(MockProvider::new("success")));
        providers.insert("mock-tools".into(), Arc::new(MockToolProvider::new("success")));
    }
    if providers.is_empty() {
        tracing::warn!("no provider is configured: set OPENAI_API_KEY, ANTHROPIC_API_KEY or SUPERROUTER_MOCK=1");
    }
    let provider_count = providers.len();
    let router = Arc::new(Router::new(store.clone(), providers, &cfg.digest_key).map_err(str::to_string)?);
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
    tracing::info!(addr = %local, store = if cfg.database_url.is_some() { "postgres" } else { "memory" }, providers = provider_count, "listening");
    if let Some(ready) = ready {
        let _ = ready.send(local);
    }
    let stopping = shutdown.clone();
    let serve = axum::serve(listener, app).with_graceful_shutdown(async move { stopping.cancelled().await });
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
