//! The provider cases of `conformance/tools.v1.json`: request bodies and recorded streams, as the reference has them.

use std::sync::Arc;

use super_router_contracts::Provider;
use super_router_provider_openai::OpenAi;

#[tokio::test]
async fn tool_cases_match_the_reference() {
    let make = |base: &str| Arc::new(OpenAi::builder("test-key").base_url(base).build().unwrap()) as Arc<dyn Provider>;
    let failures = super_router_dataplane_test::run_tool_cases("openai", &make).await;
    assert!(failures.is_empty(), "{} cases differ:\n{}", failures.len(), failures.join("\n"));
}
