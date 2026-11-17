//! Publishing a new customer price from exact units-per-credit rates.

use serde_json::Value;
use super_router_contracts::{kind_meter, Principal, RouterError, Service};
use super_router_pricing::compile_credit_price;

use crate::store::{DataStore, StoreError, StoreResult};

fn fail(code: &str, status: u16) -> StoreError {
    StoreError::Router(RouterError::new(code, status))
}

/// Advances only the customer-sale price of a service to a new version compiled from exact units-per-credit
/// rates. Supply and platform prices stay as the server's snapshot. `config` is a decoded JSON value;
/// `expected_version` must be the service's current version.
pub async fn publish_credit_rate_version(
    store: &dyn DataStore,
    p: &Principal,
    id: &str,
    expected_version: i64,
    config: &Value,
    mode: &str,
) -> StoreResult<Service> {
    if !p.has_scope("service:write") {
        return Err(fail("scope_denied", 403));
    }
    if !(1..(1 << 53) - 1).contains(&expected_version) {
        return Err(fail("invalid_service_version", 400));
    }
    let current = store.get_service(p, id, None).await?;
    if current.version != expected_version {
        return Err(fail("service_version_conflict", 409));
    }
    let price = compile_credit_price(config)?;
    let cfg = config.as_object();
    let has_tokens = cfg.is_some_and(|c| c.contains_key("tokens"));
    let has_media_key = cfg.is_some_and(|c| c.contains_key("media"));
    let media = cfg.and_then(|c| c.get("media")).and_then(Value::as_object);
    let mismatch = match (&current.task_kind, current.price.units.is_empty()) {
        (Some(_), false) => price.units.is_empty() || has_tokens || has_media_key,
        (Some(kind), true) => {
            let meter = kind_meter(kind).unwrap_or_default();
            !media.is_some_and(|m| m.contains_key(meter))
                || has_tokens
                || !price.units.is_empty()
                || media.is_some_and(|m| m.keys().any(|k| k != meter))
        }
        (None, _) => !has_tokens || media.is_some_and(|m| !m.is_empty()) || !price.units.is_empty() || has_media_key,
    };
    if mismatch {
        return Err(fail("credit_rate_kind_mismatch", 422));
    }
    if price.asset != current.price.asset {
        return Err(fail("credit_asset_mismatch", 422));
    }
    let mut next = current;
    next.version += 1;
    next.price = price;
    store.publish(p, &next, mode, Some(expected_version)).await?;
    Ok(next)
}
