//! Deterministic providers for tests and local runs.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super_router_contracts::{Invocation, Meters, Offering, Provider, ProviderError, ProviderEvent, ProviderStream};
use uuid::Uuid;

/// A deterministic chat provider. `behavior` is `success`, `reject` (a definitive, retryable rejection),
/// `unknown` (outcome lost before any output) or `partial` (output, then loss). Empty means success.
#[derive(Debug, Default)]
pub struct MockProvider {
    behavior: String,
    delay: Option<Duration>,
    calls: AtomicU64,
}

impl MockProvider {
    pub fn new(behavior: &str) -> Self {
        Self { behavior: behavior.to_string(), ..Self::default() }
    }

    /// Makes every call wait before it answers.
    #[must_use]
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }

    /// How many times [`Provider::invoke`] was called.
    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Provider for MockProvider {
    fn invoke(&self, input: &Invocation, _offering: &Offering) -> ProviderStream {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let (behavior, delay) = (self.behavior.clone(), self.delay);
        let is_task = input.input.task.is_some();
        let max_output = input.limits.max_output_tokens;
        Box::pin(async_stream::stream! {
            if is_task {
                yield Err(ProviderError::definitive("unsupported_task", false));
                return;
            }
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            match behavior.as_str() {
                "reject" => {
                    yield Err(ProviderError::definitive("rate_limited", true));
                    return;
                }
                "unknown" => {
                    yield Err(ProviderError::unknown("upstream_failed"));
                    return;
                }
                _ => {}
            }
            yield Ok(ProviderEvent::Delta { text: "Hello from SuperRouter.".to_string() });
            if behavior == "partial" {
                yield Err(ProviderError::unknown("upstream_failed"));
                return;
            }
            yield Ok(ProviderEvent::Usage {
                usage: Meters { input: 10, output: max_output.min(5), ..Meters::default() },
                upstream_id: Uuid::new_v4().to_string(),
            });
            yield Ok(ProviderEvent::Done { completion: None });
        })
    }
}

/// Simulates bounded media tasks and produces a receipt, never real media.
#[derive(Debug, Default)]
pub struct MockTaskProvider {
    behavior: String,
    calls: AtomicU64,
}

impl MockTaskProvider {
    pub fn new(behavior: &str) -> Self {
        Self { behavior: behavior.to_string(), calls: AtomicU64::new(0) }
    }

    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Provider for MockTaskProvider {
    fn invoke(&self, input: &Invocation, offering: &Offering) -> ProviderStream {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let behavior = self.behavior.clone();
        let task = input.input.task.clone();
        let task_kind = offering.task_kind.clone();
        let limit_of = {
            let meter = task.as_ref().map(super_router_contracts::Task::meter_name).unwrap_or_default();
            (input.limits.usage_limit(&meter), offering.max_usage.get(&meter))
        };
        Box::pin(async_stream::stream! {
            let Some(task) = task.filter(|t| task_kind.as_deref() == Some(t.kind.as_str()) && behavior != "reject") else {
                yield Err(ProviderError::definitive("unsupported_task", false));
                return;
            };
            if behavior == "unknown" {
                yield Err(ProviderError::unknown("usage_unknown"));
                return;
            }
            let meter = task.meter_name();
            let (units, allowed) = limit_of;
            if units == 0 || units > allowed {
                yield Err(ProviderError::definitive("meter_limit", false));
                return;
            }
            let partial = behavior == "partial";
            let completed = match (partial, meter.as_str()) {
                (true, "tasks") => 0,
                (true, _) => units / 2,
                (false, _) => units,
            };
            yield Ok(ProviderEvent::Delta { text: format!("Simulated {} task receipt; no media generated.", task.kind) });
            let mut usage = Meters::default();
            match meter.as_str() {
                "images" => usage.images = Some(completed),
                "video_ms" => usage.video_ms = Some(completed),
                "audio_ms" => usage.audio_ms = Some(completed),
                "tasks" => usage.tasks = Some(completed),
                custom => {
                    usage.units.insert(custom.to_string(), completed);
                }
            }
            yield Ok(ProviderEvent::Usage { usage, upstream_id: Uuid::new_v4().to_string() });
            yield Ok(ProviderEvent::Done { completion: Some(if partial { "partial" } else { "succeeded" }.to_string()) });
        })
    }
}
