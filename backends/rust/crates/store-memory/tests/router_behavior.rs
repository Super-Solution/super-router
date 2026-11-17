//! Router behaviour the shared scenarios cannot reach, because their mock provider is too well behaved: output
//! followed by a definitive error, usage above the reservation, a vanished client, cancellation mid-call and the
//! stream of events.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::stream;
use super_router_contracts::{
    parse_service, Invocation, Meters, Offering, Principal, Provider, ProviderError, ProviderEvent, ProviderStream, StreamEvent,
};
use super_router_dataplane_test::{load, DIGEST_KEY};
use super_router_router::{DataStore, Emit, EmitClosed, InferError, InferResult, Router, Store};
use super_router_store_memory::MemoryStore;
use tokio_util::sync::CancellationToken;

type Script = Vec<Result<ProviderEvent, ProviderError>>;

/// Plays back a fixed list of events, optionally pausing before each one.
struct Scripted {
    script: Script,
    pause: Duration,
}

impl Provider for Scripted {
    fn invoke(&self, _input: &Invocation, _offering: &Offering) -> ProviderStream {
        let (script, pause) = (self.script.clone(), self.pause);
        Box::pin(async_stream::stream! {
            for item in script {
                if !pause.is_zero() {
                    tokio::time::sleep(pause).await;
                }
                yield item;
            }
        })
    }
}

fn delta(text: &str) -> Result<ProviderEvent, ProviderError> {
    Ok(ProviderEvent::Delta { text: text.to_string() })
}

fn usage(input: i64, output: i64) -> Result<ProviderEvent, ProviderError> {
    Ok(ProviderEvent::Usage { usage: Meters { input, output, ..Meters::default() }, upstream_id: "up-1".to_string() })
}

fn done() -> Result<ProviderEvent, ProviderError> {
    Ok(ProviderEvent::Done { completion: None })
}

struct World {
    store: Arc<MemoryStore>,
    router: Router,
    principal: Principal,
    body: Vec<u8>,
}

async fn world(script: Script, pause: Duration) -> World {
    let data = load();
    let service = parse_service(&serde_json::to_vec(&data["services"]["chat"]).unwrap()).unwrap();
    let principal = Principal {
        workspace: "ws-behaviour".into(),
        environment: "test".into(),
        customer: "c1".into(),
        scopes: ["invoke", "service:write", "billing:write", "billing:read"].map(String::from).to_vec(),
        ..Principal::default()
    };
    let store = Arc::new(MemoryStore::default());
    store.publish(&principal, &service, "", None).await.unwrap();
    store.grant(&principal, "customer_sale", "credit:demo", "100000", "g1", None).await.unwrap();
    let providers: HashMap<String, Arc<dyn Provider>> =
        HashMap::from([("ok".to_string(), Arc::new(Scripted { script, pause }) as Arc<dyn Provider>)]);
    let router = Router::new(store.clone(), providers, DIGEST_KEY).unwrap();
    World { store, router, principal, body: serde_json::to_vec(&data["invocation"]).unwrap() }
}

impl World {
    async fn infer(&self, key: &str, emit: Option<Emit>, cancel: CancellationToken) -> Result<InferResult, InferError> {
        self.router.infer(&self.principal, &self.body, key, emit, cancel).await
    }

    async fn held(&self) -> (String, String) {
        let rows = self.store.balance(&self.principal).await.unwrap();
        (rows[0].balance.clone(), rows[0].held.clone())
    }
}

#[tokio::test]
async fn output_followed_by_a_definitive_error_is_partial_and_never_refunded() {
    // The provider said "nothing happened", but the customer already saw text: the hold must stay.
    let w = world(vec![delta("Hel"), Err(ProviderError::definitive("rate_limited", true))], Duration::ZERO).await;
    let r = w.infer("k1", None, CancellationToken::new()).await.unwrap();
    assert_eq!((r.execution_status.as_str(), r.billing_status.as_str()), ("partial", "pending"));
    let (balance, held) = w.held().await;
    assert_eq!(balance, "100000");
    assert_ne!(held, "0");
    assert_eq!(w.store.inspect(&w.principal).reconciliation.len(), 1);
}

#[tokio::test]
async fn usage_above_the_reservation_goes_to_reconciliation() {
    // 2000 input tokens cost the provider 2000 atoms against a 1056 bound, and the customer 4000 against 2144.
    let w = world(vec![delta("x"), usage(2000, 1), done()], Duration::ZERO).await;
    let r = w.infer("k1", None, CancellationToken::new()).await.unwrap();
    assert_eq!(r.billing_status, "pending");
    assert_eq!(r.usage.as_ref().map(|u| u.status.as_str()), Some("confirmed"));
    assert_eq!(w.held().await.0, "100000", "nothing is charged until somebody reconciles");
}

#[tokio::test]
async fn usage_that_is_negative_or_missing_keeps_the_hold() {
    for (script, name) in [
        (vec![delta("x"), usage(-1, 1), done()], "negative counter"),
        (vec![delta("x"), done()], "no usage at all"),
        (vec![delta("x"), usage(10, 1)], "no done event"),
    ] {
        let w = world(script, Duration::ZERO).await;
        let r = w.infer("k1", None, CancellationToken::new()).await.unwrap();
        assert_eq!(r.billing_status, "pending", "{name}");
        assert_ne!(w.held().await.1, "0", "{name}");
    }
}

#[tokio::test]
async fn a_client_that_stops_listening_leaves_the_funds_held() {
    let w = world(vec![delta("a"), delta("b"), usage(10, 5), done()], Duration::from_millis(20)).await;
    let seen = Arc::new(Mutex::new(0));
    let counter = seen.clone();
    let emit: Emit = Box::new(move |_| {
        let mut n = counter.lock().unwrap();
        *n += 1;
        if *n >= 3 {
            Err(EmitClosed)
        } else {
            Ok(())
        }
    });
    // started, first delta, then the third event fails: the router stops the provider and records an uncertain
    // outcome. The caller is gone, so the answer is an error that still names the request.
    let err = w.infer("k1", Some(emit), CancellationToken::new()).await.unwrap_err();
    assert_eq!((err.error.code.as_str(), err.error.status), ("execution_unknown", 503));
    let id = err.request_id.expect("the request was admitted, so the error names it");
    let record = w.store.get(&w.principal, &id).await.unwrap();
    assert_eq!((record.execution.as_str(), record.billing.as_str()), ("partial", "pending"));
    assert_ne!(w.held().await.1, "0");
}

#[tokio::test]
async fn cancelling_the_context_aborts_the_provider_but_still_settles_the_books() {
    let w = Arc::new(world(vec![delta("a"), usage(10, 5), done()], Duration::from_millis(300)).await);
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(80)).await;
        trigger.cancel();
    });
    let started = std::time::Instant::now();
    let r = w.infer("k1", None, cancel).await.unwrap();
    assert!(started.elapsed() < Duration::from_millis(250), "the provider call was not abandoned");
    assert_eq!(r.billing_status, "pending", "an aborted dispatch is uncertain, never refunded");
    assert_ne!(w.held().await.1, "0");
}

#[tokio::test]
async fn a_cancel_intent_recorded_in_the_store_stops_a_running_request() {
    let w = Arc::new(world(vec![delta("a"), usage(10, 5), done()], Duration::from_millis(400)).await);
    let (w2, id_slot) = (w.clone(), Arc::new(Mutex::new(None::<String>)));
    let canceller = tokio::spawn({
        let id_slot = id_slot.clone();
        async move {
            // The request id is only known once admitted; find it through the store.
            for _ in 0..50 {
                tokio::time::sleep(Duration::from_millis(20)).await;
                if let Some(id) = w2.store.request_ids(&w2.principal).first() {
                    w2.store.cancel(&w2.principal, id).await.unwrap();
                    *id_slot.lock().unwrap() = Some(id.clone());
                    return;
                }
            }
        }
    });
    let started = std::time::Instant::now();
    let r = w.infer("k1", None, CancellationToken::new()).await.unwrap();
    canceller.await.unwrap();
    assert!(id_slot.lock().unwrap().is_some());
    assert!(started.elapsed() < Duration::from_millis(900), "the cancel intent was not noticed");
    assert_eq!(r.billing_status, "pending");
}

#[tokio::test]
async fn the_stream_reports_started_deltas_and_completion_in_order() {
    let w = world(vec![delta("Hel"), delta("lo"), usage(10, 5), done()], Duration::ZERO).await;
    let events: Arc<Mutex<Vec<StreamEvent>>> = Arc::default();
    let sink = events.clone();
    let emit: Emit = Box::new(move |e| {
        sink.lock().unwrap().push(e);
        Ok(())
    });
    let r = w.infer("k1", Some(emit), CancellationToken::new()).await.unwrap();
    assert_eq!((r.execution_status.as_str(), r.billing_status.as_str()), ("succeeded", "settled"));
    assert_eq!(r.output.as_ref().map(|o| o.content.as_str()), Some("Hello"));
    let events = events.lock().unwrap();
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, ["request.started", "output.delta", "output.delta", "request.completed"]);
    assert_eq!(events.iter().map(|e| e.sequence).collect::<Vec<_>>(), [1, 2, 3, 4]);
    assert!(events.iter().all(|e| e.request_id == r.id));
    assert_eq!(events[3].data["billing_status"], "settled");
}

#[tokio::test]
async fn a_replay_returns_only_identifiers_and_never_calls_the_provider_again() {
    let w = world(vec![delta("x"), usage(10, 5), done()], Duration::ZERO).await;
    let first = w.infer("same", None, CancellationToken::new()).await.unwrap();
    let again = w.infer("same", None, CancellationToken::new()).await.unwrap();
    assert!(again.replayed && again.output.is_none() && again.usage.is_none());
    assert_eq!(again.id, first.id);
    assert_eq!(w.held().await, ("99965".to_string(), "0".to_string()));
}

#[tokio::test]
async fn an_endless_provider_is_cut_off_at_the_output_cap() {
    let chunk = "x".repeat(100_000);
    let script: Script = (0..12).map(|_| delta(&chunk)).collect();
    let w = world(script, Duration::ZERO).await;
    let r = w.infer("k1", None, CancellationToken::new()).await.unwrap();
    assert_eq!(r.billing_status, "pending");
    assert!(r.output.unwrap().content.len() <= 1_100_000);
}

#[test]
fn the_script_helper_compiles_against_a_plain_stream() {
    // Guards the Provider contract: any `Stream` of results is a valid provider answer.
    let s: ProviderStream = Box::pin(stream::iter(vec![delta("x"), done()]));
    drop(s);
}
