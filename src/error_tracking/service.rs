//! The error tracking submission service: panic hook install/chain/
//! restore for automatic `handled=false` dispatch, anonymization,
//! dedup, ignore rules, and the 30-minute submission scheduler.

use std::error::Error as StdError;
use std::panic::{set_hook, take_hook, PanicHookInfo};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use regex::Regex;
use reqwest::Url;
use serde_json::{Map, Value};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio::time;
use uuid::Uuid;

use super::helper;
use super::tracker::{CauseFrame, IgnoreRule, Tracker, TrackedError};
use crate::domain::{Attributes, SdkInfo};
use crate::error::Result;
use crate::metrics::MetricsEvent;
use crate::transport::{resolve_server_url, Transport};

const ERROR_PATH: &str = "/v1/error";
/// Env var overriding the error-tracker server base URL.
const ERROR_TRACKER_SERVER_ENV: &str = "FASTSTATS_ERROR_TRACKER_SERVER";
const DEFAULT_ERROR_TRACKER_SERVER: &str = "https://metrics.faststats.dev";

/// The interval between error submissions (30 minutes, fixed).
pub const SUBMISSION_PERIOD: Duration = Duration::from_secs(30 * 60);

/// Global slot holding the previously-installed panic hook, so it can
/// be restored on [`ErrorTracker::detach`].
static PREVIOUS_HOOK: OnceLock<Mutex<Option<Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>>>> =
    OnceLock::new();

/// Builds an [`ErrorTracker`].
pub struct Factory {
    project_name: String,
    sdk_info: SdkInfo,
    ignore_rules: Vec<IgnoreRule>,
    extra_patterns: Vec<(Regex, &'static str)>,
    attributes: Option<Attributes>,
}

impl Factory {
    /// Creates a new error-tracking factory. `project_name` identifies
    /// used in project_name); `sdk_info` identifies this SDK itself
    /// (used for `sdk_name`/`sdk_version` and the transport user agent).
    pub fn new(project_name: impl Into<String>, sdk_info: SdkInfo) -> Self {
        Factory {
            project_name: project_name.into(),
            sdk_info,
            ignore_rules: Vec::new(),
            extra_patterns: Vec::new(),
            attributes: None,
        }
    }

    /// Ignore every error of the given exact type.
    #[must_use]
    pub fn ignore_error_type(mut self, error_type: impl Into<String>) -> Self {
        self.ignore_rules.push(IgnoreRule::Type(error_type.into()));
        self
    }

    /// Ignore every error whose message matches `pattern`.
    #[must_use]
    pub fn ignore_error_message(mut self, pattern: Regex) -> Self {
        self.ignore_rules.push(IgnoreRule::MessagePattern(pattern));
        self
    }

    /// Ignore errors that match both `error_type` and `pattern`.
    #[must_use]
    pub fn ignore_error(mut self, error_type: impl Into<String>, pattern: Regex) -> Self {
        self.ignore_rules
            .push(IgnoreRule::TypeAndPattern(error_type.into(), pattern));
        self
    }

    /// Registers an additional anonymization pattern, applied after
    /// the built-in defaults.
    #[must_use]
    pub fn anonymize(mut self, pattern: Regex, replacement: &'static str) -> Self {
        self.extra_patterns.push((pattern, replacement));
        self
    }

    /// Default (per-tracker) attributes merged into every
    /// submission's top-level `context`.
    #[must_use]
    pub fn attributes(mut self, attributes: Attributes) -> Self {
        self.attributes = Some(attributes);
        self
    }

    pub fn build(self, transport: Arc<Transport>, server_id: Uuid) -> Result<ErrorTracker> {
        let url = resolve_server_url(ERROR_TRACKER_SERVER_ENV, DEFAULT_ERROR_TRACKER_SERVER)?
            .join(ERROR_PATH)
            .map_err(|e| crate::error::Error::InvalidServerUrl {
                env_var: ERROR_TRACKER_SERVER_ENV,
                reason: e.to_string(),
            })?;

        let mut tracker = Tracker::new();
        for rule in self.ignore_rules {
            tracker.add_ignore_rule(rule);
        }

        Ok(ErrorTracker {
            transport,
            url,
            project_name: self.project_name,
            sdk_info: self.sdk_info,
            server_id,
            extra_patterns: self.extra_patterns,
            attributes: self.attributes,
            tracker: Mutex::new(tracker),
            panic_hook_installed: Mutex::new(false),
            metrics_snapshot: Mutex::new(None),
        })
    }
}

/// The error tracking submission service. Owns no scheduling state
/// itself; [`ErrorTracker::start_submitting`] spawns the periodic task.
pub struct ErrorTracker {
    transport: Arc<Transport>,
    url: Url,
    project_name: String,
    sdk_info: SdkInfo,
    server_id: Uuid,
    extra_patterns: Vec<(Regex, &'static str)>,
    attributes: Option<Attributes>,
    tracker: Mutex<Tracker>,
    panic_hook_installed: Mutex<bool>,
    /// A snapshot-provider closure over the sibling `Metrics` service,
    /// installed (if metrics are enabled) by `Client::build`. Kept as
    /// `Option` since error tracking can be enabled without metrics.
    metrics_snapshot: Mutex<Option<Box<dyn Fn() -> Value + Send + Sync>>>,
}

impl ErrorTracker {
    /// Wires this tracker to a sibling `Metrics` service's `snapshot`
    /// closure, so submission-time payloads merge in current metrics
    /// data. Synchronous: safe to call from `ClientBuilder::build()`
    /// regardless of whether a Tokio runtime is active yet. See
    /// [`ErrorTracker::spawn_metrics_event_listener`] for the other
    /// half of this wiring.
    pub(crate) fn set_metrics_snapshot(&self, snapshot: impl Fn() -> Value + Send + Sync + 'static) {
        *self.metrics_snapshot.lock().unwrap_or_else(|p| p.into_inner()) = Some(Box::new(snapshot));
    }

    /// Spawns a background task draining a sibling `Metrics` service's
    /// [`MetricsEvent`] broadcasts, reporting each
    /// `CustomMetricFailed` as a tracked error. Requires an active
    /// Tokio runtime, so `Client::start` is the right place to call
    /// this, not `ClientBuilder::build`.
    pub(crate) fn spawn_metrics_event_listener(self: &Arc<Self>, mut events: broadcast::Receiver<MetricsEvent>) {
        let tracker = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(MetricsEvent::CustomMetricFailed { metric_id, error }) => {
                        tracker.track_error(
                            "CustomMetricFailure",
                            Some(&format!("custom metric '{metric_id}' failed: {error}")),
                            &[],
                            None,
                        );
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    /// Records an error directly, with `handled = true`. Use
    /// [`ErrorTracker::track_error_with_source`] instead if the error
    /// has a `std::error::Error::source()` chain worth reporting.
    pub fn track_error(
        &self,
        error_type: impl Into<String>,
        message: Option<&str>,
        stack: &[String],
        context: Option<Attributes>,
    ) {
        self.record(error_type.into(), message, stack, true, context, &[]);
    }

    /// Records an error directly, with `handled = true`, additionally
    /// walking `error.source()` and including each cause in the
    /// report.
    pub fn track_error_with_source(
        &self,
        error_type: impl Into<String>,
        message: Option<&str>,
        stack: &[String],
        context: Option<Attributes>,
        error: &(dyn StdError + 'static),
    ) {
        let causes = self.collect_causes(error);
        self.record(error_type.into(), message, stack, true, context, &causes);
    }

    /// Shared recording path used by [`ErrorTracker::track_error`],
    /// [`ErrorTracker::track_error_with_source`], and the automatic
    /// panic hook.
    fn record(
        &self,
        error_type: String,
        message: Option<&str>,
        stack: &[String],
        handled: bool,
        context: Option<Attributes>,
        raw_causes: &[(String, Option<String>, Vec<String>)],
    ) {
        let anonymized_message = message.map(|m| {
            let anonymized = helper::anonymize(m, &self.extra_patterns);
            helper::truncate_message(&anonymized)
        });
        let anonymized_stack: Vec<String> = stack
            .iter()
            .map(|frame| helper::anonymize(frame, &self.extra_patterns))
            .collect();
        let collapsed_stack = helper::collapse_stack(&anonymized_stack);

        let causes: Vec<CauseFrame> = raw_causes
            .iter()
            .map(|(cause_type, cause_message, cause_stack)| {
                let anonymized_cause_message = cause_message.as_ref().map(|m| {
                    let anonymized = helper::anonymize(m, &self.extra_patterns);
                    helper::truncate_message(&anonymized)
                });
                let anonymized_cause_stack: Vec<String> = cause_stack
                    .iter()
                    .map(|frame| helper::anonymize(frame, &self.extra_patterns))
                    .collect();
                CauseFrame {
                    error_type: cause_type.clone(),
                    message: anonymized_cause_message,
                    stack: helper::collapse_stack(&anonymized_cause_stack),
                }
            })
            .collect();

        let tracked = TrackedError {
            error_type,
            message: anonymized_message,
            stack: collapsed_stack,
            handled,
            context,
            causes,
            count: 1,
        };

        let mut tracker = self.tracker.lock().unwrap_or_else(|p| p.into_inner());
        tracker.track(tracked);
    }

    /// Walks `error.source()` (skipping `error` itself), collecting a `(type, message, stack)` tuple per cause. Each cause's "stack"
    /// is just its own `Display` text as a single-frame placeholder; callers with a real per-cause stack should build `CauseFrame`s
    /// directly instead. A `PartialEq`-based cycle guard caps at a generous depth as a fallback.
    fn collect_causes(&self, error: &(dyn StdError + 'static)) -> Vec<(String, Option<String>, Vec<String>)> {
        const MAX_CAUSE_DEPTH: usize = 16;
        let mut causes = Vec::new();
        let mut seen_messages = Vec::new();
        let mut current = error.source();
        while let Some(cause) = current {
            if causes.len() >= MAX_CAUSE_DEPTH {
                break;
            }
            let message = cause.to_string();
            if seen_messages.contains(&message) {
                // Defends against a cycle of causes that keep reporting an identical Display string, since std::error::Error
                // has no identity/pointer check available generically.
                break;
            }
            seen_messages.push(message.clone());
            causes.push((error_type_name(cause), Some(message), Vec::new()));
            current = cause.source();
        }
        causes
    }

    /// Whether there are zero pending error reports.
    pub fn is_empty(&self) -> bool {
        self.tracker.lock().unwrap_or_else(|p| p.into_inner()).is_empty()
    }

    /// Builds the full submission payload from currently-pending
    /// tracked errors. Draining resets the tracking table for the
    /// next window.
    pub(crate) fn create_data(&self) -> Option<Value> {
        let pending = self.tracker.lock().unwrap_or_else(|p| p.into_inner()).drain();
        if pending.is_empty() {
            return None;
        }

        let mut data = Map::new();
        data.insert("identifier".to_string(), Value::from(self.server_id.to_string()));
        data.insert("language".to_string(), Value::from("rust"));
        data.insert("project_name".to_string(), Value::from(self.project_name.clone()));
        data.insert("sdk_name".to_string(), Value::from(self.sdk_info.name().to_string()));
        data.insert("sdk_version".to_string(), Value::from(self.sdk_info.version().to_string()));

        // Start from this tracker's own registered attributes, then merge in the sibling Metrics service's current snapshot, if wired up.
        let mut context = match &self.attributes {
            Some(attributes) if !attributes.is_empty() => attributes.to_json_map(),
            _ => Map::new(),
        };
        if let Some(snapshot_fn) = &*self.metrics_snapshot.lock().unwrap_or_else(|p| p.into_inner()) {
            if let Value::Object(metrics_map) = snapshot_fn() {
                for (key, value) in metrics_map {
                    context.entry(key).or_insert(value);
                }
            }
        }
        if !context.is_empty() {
            data.insert("context".to_string(), Value::Object(context));
        }

        let errors: Vec<Value> = pending.iter().map(TrackedError::to_json).collect();
        data.insert("errors".to_string(), Value::from(errors));

        Some(Value::Object(data))
    }

    /// Submits currently-pending errors once. Nothing is submitted if there are
    /// zero pending reports.
    pub async fn submit(&self) -> bool {
        let Some(data) = self.create_data() else {
            return true;
        };

        match self.transport.submit(&self.url, &data, "errors").await {
            Ok(outcome) if outcome.is_successful() => true,
            Ok(_) => false,
            Err(e) => {
                log::error!("Failed to submit errors: {e}");
                false
            }
        }
    }

    /// Installs a panic hook that records every panic with
    /// `handled = false`, then chains to whatever hook was previously
    /// installed. Idempotent.
    pub fn install_panic_hook(self: &Arc<Self>) {
        let mut installed = self.panic_hook_installed.lock().unwrap_or_else(|p| p.into_inner());
        if *installed {
            return;
        }

        let previous = take_hook();
        PREVIOUS_HOOK
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .replace(previous);

        let tracker = Arc::clone(self);
        set_hook(Box::new(move |panic_info| {
            let error_type = "panic".to_string();
            let message = panic_info.payload().downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| {
                panic_info
                    .payload()
                    .downcast_ref::<String>()
                    .cloned()
            });
            let location = panic_info
                .location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                .unwrap_or_else(|| "unknown location".to_string());
            let stack = vec![location];

            let thread_name = thread::current()
                .name()
                .unwrap_or("unnamed")
                .to_string();
            let mut context = Attributes::empty();
            let _ = context.put("thread_name", thread_name);

            tracker.record(error_type, message.as_deref(), &stack, false, Some(context), &[]);
            if let Some(previous) = PREVIOUS_HOOK
                .get_or_init(|| Mutex::new(None))
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
            {
                previous(panic_info);
            }
        }));

        *installed = true;
    }

    /// Restores whatever panic hook was installed before
    /// [`ErrorTracker::install_panic_hook`]. A no-op if never installed.
    pub fn detach(&self) {
        let mut installed = self.panic_hook_installed.lock().unwrap_or_else(|p| p.into_inner());
        if !*installed {
            return;
        }

        if let Some(previous) = PREVIOUS_HOOK
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            set_hook(previous);
        } else {
            let _ = take_hook();
        }

        *installed = false;
    }

    /// Spawns the periodic submission task: every 30 minutes. Returns
    /// an abortable join handle.
    pub fn start_submitting(self: Arc<Self>) -> JoinHandle<()> {
        log::info!("Starting error tracking submission task");
        tokio::spawn(async move {
            let mut interval = time::interval(SUBMISSION_PERIOD);
            loop {
                interval.tick().await;
                self.submit().await;
            }
        })
    }

    /// Performs a final best-effort submission on shutdown, then
    /// detaches the panic hook if it was installed.
    pub async fn shutdown(&self) {
        log::info!("Shutting down error tracking submission");
        self.submit().await;
        self.detach();
    }
}

/// A best-effort "type name" for an arbitrary `std::error::Error`
/// cause. Falls back to the cause's own `Display` text truncated to a
/// short prefix when nothing better is available, since every cause
/// still needs some non-empty `error_type` for the wire format.
fn error_type_name(cause: &(dyn StdError + 'static)) -> String {
    let text = cause.to_string();
    let prefix: String = text.chars().take(40).collect();
    if prefix.is_empty() {
        "UnknownCause".to_string()
    } else {
        prefix
    }
}

#[cfg(test)]
mod tests {
    use std::env::{remove_var, set_var};
    use super::*;
    use crate::validated::Token;
    use std::sync::Mutex as StdMutex;

    static ENV_LOCK: StdMutex<()> = StdMutex::new(());

    fn test_transport() -> Arc<Transport> {
        let token = Token::new("a".repeat(32)).expect("valid token");
        let sdk_info = SdkInfo::new("faststats-rs-tests", "0.0.0", "FastStats Rust SDK v0.0.0 (tests-project:0.0.0)").expect("valid sdk info");
        Arc::new(Transport::new(token, sdk_info).expect("transport builds"))
    }

    fn test_sdk_info() -> SdkInfo {
        SdkInfo::new("faststats-rs-tests", "0.0.0", "FastStats Rust SDK v0.0.0 (tests-project:0.0.0)").expect("valid sdk info")
    }

    fn test_server_id() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-000000000002").expect("valid literal uuid")
    }

    #[test]
    fn no_pending_errors_yields_no_payload() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: `ENV_LOCK` serializes every test in this module
        // that touches process env vars.
        unsafe {
            remove_var(ERROR_TRACKER_SERVER_ENV);
        }

        let tracker = Factory::new("tests-project", test_sdk_info())
            .build(test_transport(), test_server_id())
            .expect("builds");

        assert!(tracker.is_empty());
        assert!(tracker.create_data().is_none());
    }

    #[test]
    fn track_error_populates_payload_shape() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: `ENV_LOCK` serializes every test in this module
        // that touches process env vars.
        unsafe {
            remove_var(ERROR_TRACKER_SERVER_ENV);
        }

        let tracker = Factory::new("tests-project", test_sdk_info())
            .build(test_transport(), test_server_id())
            .expect("builds");

        tracker.track_error(
            "CustomError",
            Some("something broke"),
            &["frame_a".to_string(), "frame_b".to_string()],
            None,
        );

        let data = tracker.create_data().expect("payload present");
        assert_eq!(data["identifier"], "00000000-0000-0000-0000-000000000002");
        assert_eq!(data["language"], "rust");
        assert_eq!(data["project_name"], "tests-project");
        assert_eq!(data["sdk_name"], "faststats-rs-tests");
        let errors = data["errors"].as_array().expect("errors array");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["error"], "CustomError");
        assert_eq!(errors[0]["message"], "something broke");
        assert_eq!(errors[0]["handled"], true);
    }
    
    #[test]
    fn track_error_anonymizes_message_and_stack() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: `ENV_LOCK` serializes every test in this module
        // that touches process env vars.
        unsafe {
            remove_var(ERROR_TRACKER_SERVER_ENV);
        }

        let tracker = Factory::new("tests-project", test_sdk_info())
            .build(test_transport(), test_server_id())
            .expect("builds");

        tracker.track_error(
            "IoError",
            Some("failed to reach 10.0.0.5"),
            &["at /home/tests/app.rs:10".to_string()],
            None,
        );

        let data = tracker.create_data().expect("payload present");
        let errors = data["errors"].as_array().unwrap();
        assert_eq!(errors[0]["message"], "failed to reach [ipv4]");
        // home_path_pattern keeps the OS-specific prefix and only
        // redacts the username itself.
        assert_eq!(errors[0]["stack"][0], "at /home/[home]/app.rs:10");
    }

    #[test]
    fn ignored_error_type_is_never_recorded() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: `ENV_LOCK` serializes every test in this module
        // that touches process env vars.
        unsafe {
            remove_var(ERROR_TRACKER_SERVER_ENV);
        }

        let tracker = Factory::new("tests-project", test_sdk_info())
            .ignore_error_type("NoisyError")
            .build(test_transport(), test_server_id())
            .expect("builds");

        tracker.track_error("NoisyError", Some("ignored"), &[], None);
        assert!(tracker.is_empty());
    }

    #[test]
    fn duplicate_errors_are_deduped_with_a_count() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: `ENV_LOCK` serializes every test in this module
        // that touches process env vars.
        unsafe {
            remove_var(ERROR_TRACKER_SERVER_ENV);
        }

        let tracker = Factory::new("tests-project", test_sdk_info())
            .build(test_transport(), test_server_id())
            .expect("builds");

        tracker.track_error("E", Some("m"), &["f".to_string()], None);
        tracker.track_error("E", Some("m"), &["f".to_string()], None);

        let data = tracker.create_data().expect("payload present");
        let errors = data["errors"].as_array().unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["count"], 2);
    }

    #[test]
    fn create_data_drains_pending_errors() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: `ENV_LOCK` serializes every test in this module
        // that touches process env vars.
        unsafe {
            remove_var(ERROR_TRACKER_SERVER_ENV);
        }

        let tracker = Factory::new("tests-project", test_sdk_info())
            .build(test_transport(), test_server_id())
            .expect("builds");

        tracker.track_error("E", None, &[], None);
        assert!(tracker.create_data().is_some());
        assert!(tracker.is_empty());
        assert!(tracker.create_data().is_none());
    }

    #[test]
    fn tracker_level_attributes_appear_as_context() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: `ENV_LOCK` serializes every test in this module
        // that touches process env vars.
        unsafe {
            remove_var(ERROR_TRACKER_SERVER_ENV);
        }

        let mut attrs = Attributes::empty();
        attrs.put("environment", "staging").expect("valid attribute");

        let tracker = Factory::new("tests-project", test_sdk_info())
            .attributes(attrs)
            .build(test_transport(), test_server_id())
            .expect("builds");

        tracker.track_error("E", None, &[], None);
        let data = tracker.create_data().expect("payload present");
        assert_eq!(data["context"]["environment"], "staging");
    }

    #[test]
    fn per_error_attributes_appear_on_the_error_entry() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: `ENV_LOCK` serializes every test in this module
        // that touches process env vars.
        unsafe {
            remove_var(ERROR_TRACKER_SERVER_ENV);
        }

        let mut attrs = Attributes::empty();
        attrs.put("user_id", 42).expect("valid attribute");

        let tracker = Factory::new("tests-project", test_sdk_info())
            .build(test_transport(), test_server_id())
            .expect("builds");

        tracker.track_error("E", None, &[], Some(attrs));
        let data = tracker.create_data().expect("payload present");
        assert_eq!(data["errors"][0]["context"]["user_id"], 42);
    }

    #[tokio::test]
    async fn submit_against_unreachable_server_returns_false_not_panic() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: `ENV_LOCK` serializes every test in this module
        // that touches process env vars.
        unsafe {
            set_var(ERROR_TRACKER_SERVER_ENV, "http://127.0.0.1:1");
        }

        let tracker = Factory::new("tests-project", test_sdk_info())
            .build(test_transport(), test_server_id())
            .expect("builds");
        tracker.track_error("E", None, &[], None);

        let result = tracker.submit().await;
        assert!(!result);

        // SAFETY: `ENV_LOCK` serializes every test in this module
        // that touches process env vars.
        unsafe {
            remove_var(ERROR_TRACKER_SERVER_ENV);
        }
    }

    #[tokio::test]
    async fn submit_with_no_pending_errors_returns_true_without_network() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // Deliberately unreachable: if submit() tried to send a
        // request despite having nothing pending, this would fail.
        // SAFETY: `ENV_LOCK` serializes every test in this module
        // that touches process env vars.
        unsafe {
            set_var(ERROR_TRACKER_SERVER_ENV, "http://127.0.0.1:1");
        }

        let tracker = Factory::new("tests-project", test_sdk_info())
            .build(test_transport(), test_server_id())
            .expect("builds");

        assert!(tracker.submit().await);

        // SAFETY: `ENV_LOCK` serializes every test in this module
        // that touches process env vars.
        unsafe {
            remove_var(ERROR_TRACKER_SERVER_ENV);
        }
    }
}