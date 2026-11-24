//! The provider cases of `conformance/provider-gemini.v1.json`: request bodies and recorded streams.

use std::sync::Arc;

use super_router_contracts::Provider;
use super_router_provider_gemini::Gemini;

#[tokio::test]
async fn gemini_cases_match_the_oracle() {
    let make = |base: &str| Arc::new(Gemini::builder("test-key").base_url(base).build().unwrap()) as Arc<dyn Provider>;
    let failures = super_router_dataplane_test::run_tool_cases_in("provider-gemini.v1.json", "gemini", &make).await;
    assert!(failures.is_empty(), "{} cases differ:\n{}", failures.len(), failures.join("\n"));
}
