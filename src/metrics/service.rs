//! The metrics submission service: platform metrics, custom metrics
//! gated by config, chained `on_flush`, and the 30-minute submission
//! scheduler.

use std::env;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Url;
use serde_json::{Map, Value};
use tokio::sync::broadcast;
use uuid::Uuid;

use super::metric::Metric;
use super::platform;
use crate::error::Result;
use crate::transport::{resolve_server_url, Transport};

/// Cross-service events `Metrics` broadcasts so `ErrorTracker` can react
/// without holding a direct reference to `Metrics`, avoiding a circular
/// `Arc` between sibling services under `Client`.
#[derive(Debug, Clone)]
pub enum MetricsEvent {
    /// A registered custom metric's `compute()` failed. Carries enough
    /// to build an error report: the metric's id and the error text.
    CustomMetricFailed { metric_id: String, error: String },
}

/// The channel capacity for [`MetricsEvent`] broadcasts. Generous
/// relative to how rarely custom metrics fail; a lagging receiver just
/// misses the oldest events rather than blocking the sender.
const EVENT_CHANNEL_CAPACITY: usize = 64;

const COLLECT_PATH: &str = "/v1/collect";
/// Env var overriding the metrics server base URL.
const METRICS_SERVER_ENV: &str = "FASTSTATS_METRICS_SERVER";
const DEFAULT_METRICS_SERVER: &str = "https://metrics.faststats.dev";
/// Env var overriding the initial submission delay (seconds).
const INITIAL_DELAY_ENV: &str = "FASTSTATS_INITIAL_DELAY";
const DEFAULT_INITIAL_DELAY: Duration = Duration::from_secs(30);

/// The interval between metrics submissions (30 minutes, fixed).
pub const SUBMISSION_PERIOD: Duration = Duration::from_secs(30 * 60);

/// Builds a [`Metrics`] instance.
pub struct Factory {
    project_name: String,
    project_version: String,
    metrics: Vec<Metric>,
    flush: Option<Box<dyn Fn() + Send + Sync>>,
}

impl Factory {
    /// Creates a new, empty metrics factory for the given project.
    pub fn new(project_name: impl Into<String>, project_version: impl Into<String>) -> Self {
        Factory {
            project_name: project_name.into(),
            project_version: project_version.into(),
            metrics: Vec::new(),
            flush: None,
        }
    }

    /// Registers a custom metrics. Errors if a metrics with the same id
    /// was already added.
    pub fn add_metric(mut self, metric: Metric) -> Result<Self> {
        if self.metrics.iter().any(|m| m.id() == metric.id()) {
            return Err(crate::error::Error::validation(
                "metrics id",
                format!("metric already added: {}", metric.id()),
            ));
        }
        self.metrics.push(metric);
        Ok(self)
    }

    /// Registers a flush callback, invoked after every accepted
    /// submission. Multiple registrations chain in order.
    #[must_use]
    pub fn on_flush(mut self, flush: impl Fn() + Send + Sync + 'static) -> Self {
        self.flush = Some(match self.flush.take() {
            None => Box::new(flush),
            Some(existing) => Box::new(move || {
                existing();
                flush();
            }),
        });
        self
    }

    /// `additional_metrics` gates whether custom metrics are included;
    /// internal metrics are always present.
    pub fn build(self, transport: Arc<Transport>, server_id: Uuid, additional_metrics: bool) -> Result<Metrics> {
        let url = resolve_server_url(METRICS_SERVER_ENV, DEFAULT_METRICS_SERVER)?.join(COLLECT_PATH).map_err(|e| {
            crate::error::Error::InvalidServerUrl {
                env_var: METRICS_SERVER_ENV,
                reason: e.to_string(),
            }
        })?;
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Ok(Metrics {
            transport,
            url,
            project_name: self.project_name,
            project_version: self.project_version,
            server_id,
            metrics: if additional_metrics { self.metrics } else { Vec::new() },
            flush: self.flush,
            events,
        })
    }
}

/// The metrics submission service. Owns no scheduling state itself;
/// [`Metrics::start_submitting`] spawns the periodic task.
pub struct Metrics {
    transport: Arc<Transport>,
    url: Url,
    project_name: String,
    project_version: String,
    server_id: Uuid,
    metrics: Vec<Metric>,
    flush: Option<Box<dyn Fn() + Send + Sync>>,
    /// Broadcasts [`MetricsEvent`]s to any interested subscriber, e.g.
    /// `ErrorTracker`. See the module-level doc on [`MetricsEvent`].
    events: broadcast::Sender<MetricsEvent>,
}

impl Metrics {
    /// Subscribes to this service's [`MetricsEvent`] broadcasts.
    /// `ErrorTracker` uses this (wired up in `Client::build`) to learn
    /// about custom-metric failures without `Metrics` holding a direct
    /// reference to it.
    pub fn subscribe(&self) -> broadcast::Receiver<MetricsEvent> {
        self.events.subscribe()
    }

    /// A full snapshot of this service's metrics data as a plain JSON
    /// object, matching what the next scheduled submission would send.
    /// Used by `ErrorTracker` to merge metrics data into a report's
    /// `context`.
    pub fn snapshot(&self) -> Value {
        let mut metrics = Map::new();
        self.append_data(&mut metrics);
        Value::Object(metrics)
    }

    /// Appends internal, feature, then custom metrics into one object.
    fn append_data(&self, metrics: &mut Map<String, Value>) {
        platform::append_internal_data(metrics, self.project_version.clone());

        #[cfg(feature = "terminal")]
        crate::features::terminal::append_terminal_data(metrics);

        self.append_custom_data(metrics);
    }

    /// Appends custom metrics values, skipping (with a warning) any id
    /// collision or failed/absent computation rather than aborting. A
    /// failed computation is also broadcast as
    /// [`MetricsEvent::CustomMetricFailed`].
    fn append_custom_data(&self, metrics: &mut Map<String, Value>) {
        for metric in &self.metrics {
            let id = metric.id().as_str();
            if metrics.contains_key(id) {
                log::warn!("Skipped duplicated metrics entry: {id}");
                continue;
            }
            match metric.compute() {
                Ok(Some(value)) => {
                    metrics.insert(id.to_string(), value);
                }
                Ok(None) => {
                    log::warn!("Ignored illegal null entry in metrics: {id}");
                }
                Err(e) => {
                    log::error!("Failed to append custom metrics data: {id} ({e})");
                    // no subscribers is the common case, so send() erroring is fine to ignore
                    let _ = self.events.send(MetricsEvent::CustomMetricFailed {
                        metric_id: id.to_string(),
                        error: e.to_string(),
                    });
                }
            }
        }
    }

    /// Builds the full submission payload.
    fn create_data(&self) -> Value {
        let mut metrics = Map::new();
        self.append_data(&mut metrics);

        let mut data = Map::new();
        data.insert("project_name".to_string(), Value::from(self.project_name.clone()));
        data.insert("identifier".to_string(), Value::from(self.server_id.to_string()));
        data.insert("data".to_string(), Value::Object(metrics));
        Value::Object(data)
    }

    /// Submits the current payload once; runs `on_flush` callbacks on
    /// success. Never panics; a transport failure is logged and
    /// reported as `false`.
    pub async fn submit(&self) -> bool {
        let data = self.create_data();
        match self.transport.submit(&self.url, &data, "metrics").await {
            Ok(outcome) if outcome.is_successful() => {
                if let Some(flush) = &self.flush {
                    flush();
                }
                true
            }
            Ok(_) => false,
            Err(e) => {
                log::error!("Failed to submit metrics: {e}");
                false
            }
        }
    }

    /// Spawns the periodic submission task: an initial delay, then
    /// every 30 minutes. Returns an abortable join handle.
    pub fn start_submitting(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        log::info!("Starting metrics submission task");
        let initial_delay = initial_delay_from_env();
        tokio::spawn(async move {
            tokio::time::sleep(initial_delay).await;
            let mut interval = tokio::time::interval(SUBMISSION_PERIOD);
            loop {
                interval.tick().await;
                self.submit().await;
            }
        })
    }

    /// Performs a final best-effort submission on shutdown.
    pub async fn shutdown(&self) {
        log::info!("Shutting down metrics submission");
        self.submit().await;
    }
}

/// Reads `FASTSTATS_INITIAL_DELAY` (seconds), falling back to 30s.
fn initial_delay_from_env() -> Duration {
    env::var(INITIAL_DELAY_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_INITIAL_DELAY)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::SdkInfo;
    use crate::validated::Token;
    use serde::Serialize;
    use std::env;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn test_transport() -> Arc<Transport> {
        let token = Token::new("a".repeat(32)).expect("valid token");
        let sdk_info = SdkInfo::new("faststats-rs-tests", "0.0.0", "FastStats Rust SDK v0.0.0 (tests-project:0.0.0)").expect("valid sdk info");
        Arc::new(Transport::new(token, sdk_info).expect("transport builds"))
    }

    fn test_server_id() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("valid literal uuid")
    }

    #[test]
    fn factory_rejects_duplicate_metric_ids() {
        let first = Metric::new("dup", || 1).expect("valid metrics");
        let second = Metric::new("dup", || 2).expect("valid metrics");

        let factory = Factory::new("tests-project", "0.0.0").add_metric(first).expect("first add ok");
        assert!(factory.add_metric(second).is_err());
    }

    #[test]
    fn builds_payload_with_internal_and_custom_metrics() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: `ENV_LOCK` serializes every test in this module
        // that touches process env vars.
        unsafe {
            env::remove_var(METRICS_SERVER_ENV);
        }

        #[derive(Serialize)]
        struct ServerInfo {
            region: String,
            shard_count: u32,
        }

        let custom = Metric::new("server_info", || ServerInfo {
            region: "eu-west".to_string(),
            shard_count: 3,
        })
            .expect("valid metrics");

        let factory = Factory::new("tests-project", "0.0.0").add_metric(custom).expect("add ok");
        let metrics = factory
            .build(test_transport(), test_server_id(), true)
            .expect("builds");

        let data = metrics.create_data();
        assert_eq!(data["project_name"], "tests-project");
        assert_eq!(
            data["identifier"],
            "00000000-0000-0000-0000-000000000001"
        );
        assert!(data["data"]["os_name"].is_string());
        assert!(data["data"]["core_count"].is_number());
        assert_eq!(data["data"]["server_info"]["region"], "eu-west");
        assert_eq!(data["data"]["server_info"]["shard_count"], 3);
    }

    #[test]
    fn custom_metrics_excluded_when_additional_metrics_disabled() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: `ENV_LOCK` serializes every test in this module
        // that touches process env vars.
        unsafe {
            env::remove_var(METRICS_SERVER_ENV);
        }

        let custom = Metric::new("should_be_excluded", || 1).expect("valid metrics");
        let factory = Factory::new("tests-project", "0.0.0").add_metric(custom).expect("add ok");
        let metrics = factory
            .build(test_transport(), test_server_id(), false)
            .expect("builds");

        let data = metrics.create_data();
        assert!(data["data"].get("should_be_excluded").is_none());
        assert!(data["data"]["os_name"].is_string());
    }

    #[test]
    fn duplicate_id_against_internal_metric_is_skipped_not_fatal() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: `ENV_LOCK` serializes every test in this module
        // that touches process env vars.
        unsafe {
            env::remove_var(METRICS_SERVER_ENV);
        }

        let colliding = Metric::new("os", || "should-not-appear").expect("valid metrics");
        let factory = Factory::new("tests-project", "0.0.0").add_metric(colliding).expect("add ok");
        let metrics = factory
            .build(test_transport(), test_server_id(), true)
            .expect("builds");

        let data = metrics.create_data();
        assert_ne!(data["data"]["os_name"], "should-not-appear");
    }

    #[test]
    fn failing_custom_metric_does_not_prevent_other_data() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: `ENV_LOCK` serializes every test in this module
        // that touches process env vars.
        unsafe {
            env::remove_var(METRICS_SERVER_ENV);
        }

        let failing: Metric =
            Metric::try_new("failing", || Err::<Option<i32>, _>(crate::error::Error::validation("t", "boom")))
                .expect("valid metrics");
        let factory = Factory::new("tests-project", "0.0.0").add_metric(failing).expect("add ok");
        let metrics = factory
            .build(test_transport(), test_server_id(), true)
            .expect("builds");

        let data = metrics.create_data();
        assert!(data["data"].get("failing").is_none());
        assert!(data["data"]["os_name"].is_string());
    }

    #[test]
    fn on_flush_callbacks_chain_in_registration_order() {
        let calls = Arc::new(Mutex::new(Vec::<u8>::new()));
        let calls_a = calls.clone();
        let calls_b = calls.clone();

        let factory = Factory::new("tests-project", "0.0.0")
            .on_flush(move || calls_a.lock().unwrap().push(1))
            .on_flush(move || calls_b.lock().unwrap().push(2));

        let flush = factory.flush.expect("flush registered");
        flush();
        assert_eq!(*calls.lock().unwrap(), vec![1, 2]);
    }

    #[test]
    fn initial_delay_defaults_to_30_seconds_when_unset() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: `ENV_LOCK` serializes every test in this module
        // that touches process env vars.
        unsafe {
            env::remove_var(INITIAL_DELAY_ENV);
        }
        assert_eq!(initial_delay_from_env(), Duration::from_secs(30));
    }

    #[test]
    fn initial_delay_respects_env_override() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: `ENV_LOCK` serializes every test in this module
        // that touches process env vars.
        unsafe {
            env::set_var(INITIAL_DELAY_ENV, "5");
        }
        assert_eq!(initial_delay_from_env(), Duration::from_secs(5));
        // SAFETY: `ENV_LOCK` serializes every test in this module
        // that touches process env vars.
        unsafe {
            env::remove_var(INITIAL_DELAY_ENV);
        }
    }

    #[test]
    fn initial_delay_falls_back_on_invalid_value() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: `ENV_LOCK` serializes every test in this module
        // that touches process env vars.
        unsafe {
            env::set_var(INITIAL_DELAY_ENV, "not-a-number");
        }
        assert_eq!(initial_delay_from_env(), Duration::from_secs(30));
        // SAFETY: `ENV_LOCK` serializes every test in this module
        // that touches process env vars.
        unsafe {
            env::remove_var(INITIAL_DELAY_ENV);
        }
    }

    #[tokio::test]
    async fn submit_against_unreachable_server_returns_false_not_panic() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: `ENV_LOCK` serializes every test in this module
        // that touches process env vars.
        unsafe {
            env::set_var(METRICS_SERVER_ENV, "http://127.0.0.1:1");
        }

        let factory = Factory::new("tests-project", "0.0.0");
        let metrics = factory
            .build(test_transport(), test_server_id(), true)
            .expect("builds");

        let flushed = Arc::new(AtomicUsize::new(0));
        let flushed_clone = flushed.clone();
        let metrics = Metrics {
            flush: Some(Box::new(move || {
                flushed_clone.fetch_add(1, Ordering::SeqCst);
            })),
            ..metrics
        };

        let result = metrics.submit().await;
        assert!(!result);
        assert_eq!(flushed.load(Ordering::SeqCst), 0);

        // SAFETY: `ENV_LOCK` serializes every test in this module
        // that touches process env vars.
        unsafe {
            env::remove_var(METRICS_SERVER_ENV);
        }
    }
}