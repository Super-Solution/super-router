use std::collections::HashMap;
use std::sync::Arc;
use super_router_contracts::{Principal, Provider};
use super_router_provider_openai::OpenAi;
use super_router_router::{DataStore, MockProvider, Router};
use super_router_server::{handler, Key, Options, StaticKeys};
use super_router_store_memory::MemoryStore;
use axum::serve::ListenerExt;

#[tokio::main]
async fn main() {
    let upstream = std::env::var("UPSTREAM").unwrap_or("http://127.0.0.1:3499".into());
    let store: Arc<dyn DataStore> = match std::env::var("DATABASE_URL") {
        Ok(url) => {
            let pg = super_router_store_postgres::PostgresStore::connect(&url, 20, super_router_store_postgres::Options { clock: None, revalidate: None }).await.unwrap();
            pg.migrate().await.unwrap();
            Arc::new(pg)
        }
        Err(_) => Arc::new(MemoryStore::default()),
    };
    let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
    providers.insert("openai".into(), Arc::new(OpenAi::builder("sk-fake").base_url(upstream).build().unwrap()));
    providers.insert("mock".into(), Arc::new(MockProvider::new("success")));
    let router = Arc::new(Router::new(store.clone(), providers, "bench-digest-key-0123456789abcdef-0123").unwrap());
    let owner = Principal {
        workspace: "ws".into(), environment: "prod".into(), customer: "c1".into(),
        scopes: ["invoke","service:write","billing:write","billing:read","operations:read","routing:write"].map(String::from).to_vec(),
        ..Principal::default()
    };
    let key = format!("sr_{}", "a".repeat(64));
    let auth = Arc::new(StaticKeys::new(vec![Key { secret: key, principal: owner }]));
    let app = handler(Options::new(router, store, auth));
    let addr = std::env::var("ADDR").unwrap_or("127.0.0.1:3410".into());
    let l = tokio::net::TcpListener::bind(&addr).await.unwrap();
    if std::env::var("NODELAY").is_ok() { axum::serve(l.tap_io(|t| { let _ = t.set_nodelay(true); }), app).await.unwrap(); } else { axum::serve(l, app).await.unwrap(); }
}
