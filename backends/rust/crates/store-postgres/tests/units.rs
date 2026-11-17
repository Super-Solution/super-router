//! Custom units end to end, ported from the Go `units_test.go`.

mod common;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde_json::json;
use super_router_contracts::{MeterLimits, Provider};
use super_router_pricing::{Price, UnitRate};
use super_router_router::{publish_credit_rate_version, DataStore, MockTaskProvider, Router};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[tokio::test]
async fn custom_unit_reservation_and_settlement() {
    let Some(store) = common::open().await else { return };
    let mut service = common::conformance_service("chat");
    let kind = Some("task".to_string());
    let sale = Price {
        units: BTreeMap::from([("pages".to_string(), UnitRate { atoms: "7".into(), per: 3, display_unit: "頁".into() })]),
        asset: "credit:demo".into(),
        input: "0".into(),
        cached: "0".into(),
        cache_write: "0".into(),
        output: "0".into(),
        per: 1,
        media: BTreeMap::new(),
    };
    service.task_kind = kind.clone();
    service.price = sale.clone();
    service.offerings.truncate(1);
    service.offerings[0].task_kind = kind;
    service.offerings[0].max_usage = MeterLimits(BTreeMap::from([("pages".to_string(), 10)]));
    service.offerings[0].price = Price { asset: "USD".into(), ..sale };
    let p = common::principal(&format!("units-{}", Uuid::new_v4()), "c1", &["invoke", "service:write", "billing:write", "billing:read"]);

    store.publish(&p, &service, "self-host", None).await.unwrap();
    store.grant(&p, "customer_sale", "credit:demo", "100", "grant", None).await.unwrap();
    let config = json!({"asset": "credit:demo", "units": {"pages": {"units": "3000000000", "credits": "7", "display_unit": "頁"}}});
    publish_credit_rate_version(&*store, &p, "chat", 1, &config, "self-host").await.unwrap();

    let providers: HashMap<String, Arc<dyn Provider>> =
        HashMap::from([(service.offerings[0].provider.clone(), Arc::new(MockTaskProvider::new("")) as Arc<dyn Provider>)]);
    let router = Router::new(store.clone(), providers, "custom-unit-test-key-at-least-thirty-two-characters").unwrap();
    let input = br#"{"service":"chat","customer_id":"c1","input":{"task":{"kind":"task","meter":"pages","prompt":"Print"}},"limits":{"max_usage":{"units":{"pages":4}}}}"#;
    let result = router.infer(&p, input, "custom", None, CancellationToken::new()).await.unwrap();
    assert_eq!(result.billing_status, "settled", "{result:?}");
    // 4 pages at 7 atoms per 3 pages, rounded up, is 10 atoms.
    let rows = store.balance(&p).await.unwrap();
    assert_eq!((rows.len(), rows[0].balance.as_str(), rows[0].held.as_str()), (1, "90", "0"));
}
