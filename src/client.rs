//! The public [`Client`]/[`ClientBuilder`] facade: ties [`Config`] +
//! [`crate::validated::Token`] + [`SdkInfo`] + [`Transport`] and the three
//! services (metrics, error tracking, feature flags) together, mirroring
//! Java's `SimpleContext`/`FastStatsContext`.
//!
//! Two things this module owns that don't exist anywhere else in the
//! crate yet:
//!
//! - **Per-service developer toggles ANDed with `Config`.** The developer
//!   decides at `ClientBuilder` time whether a service is even
//!   constructed; the end-user-controlled `Config` decides at `start()`
//!   time whether it actually submits. Both must say yes.
//! - **First-boot notice-only behavior.** The whole resolved `Config` is
//!   always persisted to a local state file; the very first time no such
//!   file exists yet, `start()` logs a short onboarding notice and
//!   returns without submitting or fetching anything. This first-run
//!   skip is process-global (mirroring Java's JVM-system-property gate),
//!   not scoped per `server_id`, and can be overridden by setting
//!   `FASTSTATS_ENABLED=true` explicitly. Subsequent runs (or the same
//!   run once already past first boot) submit normally. See [`state`]
//!   for the file format/location.

use std::sync::Arc;
use tokio::runtime::{Builder, Handle};
use tokio::task::JoinHandle;
use uuid::Uuid;

/// Guarantees a Tokio runtime context for [`Client::start`]
enum RuntimeContext {
    /// Borrowing an already-running external runtime; nothing to own.
    External,
    /// No runtime was running on the calling thread; we made one and need
    /// to keep it alive for as long as this `Client`'s spawned tasks might
    /// run.
    Owned(tokio::runtime::Runtime),
}

impl RuntimeContext {
    /// Ensures a runtime is entered on the calling thread for the
    /// duration of `f`, creating and owning one if none already exists.
    fn ensure_and_run<R>(slot: &mut Option<RuntimeContext>, f: impl FnOnce() -> R) -> R {
        if Handle::try_current().is_ok() {
            *slot = Some(RuntimeContext::External);
            return f();
        }

        let rt = Builder::new_multi_thread()
            .enable_all()
            .thread_name("faststats-rs-background")
            .build()
            .expect("faststats-rs: failed to build background Tokio runtime");
        let _guard = rt.enter();
        let result = f();
        drop(_guard);
        *slot = Some(RuntimeContext::Owned(rt));
        result
    }
}

use crate::domain::{Config, SdkInfo};
use crate::error::Result;
use crate::error_tracking::{ErrorTracker, ErrorTrackerFactory};
use crate::feature_flags::service::{Factory as FeatureFlagsFactory, FeatureFlags};
use crate::metrics::{Factory as MetricsFactory, Metrics};
use crate::transport::Transport;
use crate::validated::Token;

/// Builds a [`Client`].
///
/// Register custom metrics / `on_flush` callbacks / ignore rules /
/// anonymization patterns / feature flags on the corresponding factory
/// before calling [`ClientBuilder::build`], each factory is consumed by
/// `build()`, matching the pattern already established by
/// `metrics::Factory`, `error_tracking::Factory`, and
/// `feature_flags::Factory` individually.
pub struct ClientBuilder {
    config: Config,
    token: Token,
    sdk_info: SdkInfo,
    metrics: MetricsFactory,
    error_tracking: ErrorTrackerFactory,
    feature_flags: FeatureFlagsFactory,
    metrics_enabled: bool,
    error_tracking_enabled: bool,
    feature_flags_enabled: bool,
}

impl ClientBuilder {
    /// Starts building a client.
    pub fn new(
        project_name: impl Into<String>,
        project_version: impl Into<String>,
        token: Token,
    ) -> Result<Self> {
        let name = project_name.into();
        let version = project_version.into();
        let user_agent = format!(
            "FastStats Rust SDK v{} ({name}:{version})",
            env!("CARGO_PKG_VERSION"),
        );
        let Ok(sdk) = SdkInfo::new(
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
            user_agent,
        ) else {
            return Err(crate::error::Error::Initialization(
                "sdk info failure".to_string(),
            ));
        };
        Ok(Self::new_with_sdk_info(name, token, sdk))
    }

    pub fn new_with_sdk_info(
        project_name: impl Into<String>,
        token: Token,
        sdk_info: SdkInfo,
    ) -> Self {
        let project_name = project_name.into();
        let config = Config::from_env(Uuid::nil());
        ClientBuilder {
            config,
            token,
            sdk_info: sdk_info.clone(),
            metrics: MetricsFactory::new(project_name.clone(), sdk_info.version()),
            error_tracking: ErrorTrackerFactory::new(project_name, sdk_info),
            feature_flags: FeatureFlagsFactory::new(),
            metrics_enabled: true,
            error_tracking_enabled: true,
            feature_flags_enabled: true,
        }
    }

    /// Overrides the default (env-derived) [`Config`]. `server_id` on
    /// the supplied config is ignored, [`Client`] always resolves
    /// `server_id` itself via the first-boot state file, so every
    /// client sharing a state directory shares one identity.
    #[must_use]
    pub fn config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }

    /// Developer-level toggle for the metrics service. Defaults to
    /// `true`. The service only actually runs if this **and**
    /// `Config::submit_metrics` are both `true`, see the module docs.
    #[must_use]
    pub fn metrics(mut self, enabled: bool) -> Self {
        self.metrics_enabled = enabled;
        self
    }

    /// Developer-level toggle for the error tracking service (including
    /// panic-hook installation). Defaults to `true`. The service only
    /// actually runs if this **and** `Config::error_tracking` are both
    /// `true`.
    #[must_use]
    pub fn error_tracking(mut self, enabled: bool) -> Self {
        self.error_tracking_enabled = enabled;
        self
    }

    /// Developer-level toggle for the feature flags service. Defaults to
    /// `true`. The service only actually runs if this **and**
    /// `Config::enabled` are both `true`, feature flags have no
    /// dedicated `Config` field of their own (see the "Config gating"
    /// note in the crate-level completion notes); the master `enabled`
    /// toggle is what gates them.
    #[must_use]
    pub fn feature_flags(mut self, enabled: bool) -> Self {
        self.feature_flags_enabled = enabled;
        self
    }

    /// Mutably exposes the metrics factory for registration calls, e.g.
    /// `builder.metrics_factory(|f| f.add_metric(...))`.
    pub fn metrics_factory(
        mut self,
        configure: impl FnOnce(MetricsFactory) -> Result<MetricsFactory>,
    ) -> Result<Self> {
        self.metrics = configure(self.metrics)?;
        Ok(self)
    }

    /// Mutably exposes the error-tracking factory for registration
    /// calls (ignore rules, anonymization patterns, attributes).
    pub fn error_tracking_factory(
        mut self,
        configure: impl FnOnce(ErrorTrackerFactory) -> ErrorTrackerFactory,
    ) -> Self {
        self.error_tracking = configure(self.error_tracking);
        self
    }

    /// Mutably exposes the feature-flags factory for registration calls
    /// (adding flags, service-level attributes, TTL).
    pub fn feature_flags_factory(
        mut self,
        configure: impl FnOnce(FeatureFlagsFactory) -> Result<FeatureFlagsFactory>,
    ) -> Result<Self> {
        self.feature_flags = configure(self.feature_flags)?;
        Ok(self)
    }

    /// Builds the [`Client`]. Resolves (or creates) the persisted
    /// `Config`/first-boot marker, ANDs it with the developer-supplied
    /// `Config` (a toggle disabled on either side stays disabled),
    /// constructs the shared [`Transport`], and builds each
    /// developer-enabled service.
    pub fn build(self) -> Result<Client> {
        let state = state::load_or_init(&self.config)?;
        let config = self
            .config
            .clone_with_server_id(state.config.server_id())
            .and(&state.config);

        let token = self.token.clone();
        let sdk_info = self.sdk_info.clone();
        let transport = Arc::new(Transport::new(self.token, self.sdk_info)?);

        let metrics = if self.metrics_enabled {
            Some(Arc::new(self.metrics.build(
                Arc::clone(&transport),
                config.server_id(),
                config.additional_metrics(),
            )?))
        } else {
            None
        };

        let error_tracking = if self.error_tracking_enabled {
            Some(Arc::new(
                self.error_tracking
                    .build(Arc::clone(&transport), config.server_id())?,
            ))
        } else {
            None
        };

        // wires the synchronous half of the metrics/error-tracker cross-wiring here;
        // the async half is deferred to Client::start
        if let (Some(metrics), Some(error_tracking)) = (&metrics, &error_tracking) {
            let metrics_for_snapshot = Arc::clone(metrics);
            error_tracking.set_metrics_snapshot(move || metrics_for_snapshot.snapshot());
        }

        let feature_flags = if self.feature_flags_enabled {
            Some(Arc::new(
                self.feature_flags
                    .build(Arc::clone(&transport), config.server_id())?,
            ))
        } else {
            None
        };

        Ok(Client {
            config,
            token,
            sdk_info,
            is_first_boot: state.is_first_boot,
            ready: false,
            metrics,
            error_tracking,
            feature_flags,
            metrics_task: None,
            error_tracking_task: None,
            runtime_context: None,
        })
    }
}

/// The public FastStats client. Construct via [`ClientBuilder`].
///
/// Each service is present only if the developer enabled it at
/// `ClientBuilder` time; whether an enabled service actually submits at
/// runtime is additionally gated by the matching [`Config`] flag, both
/// inside this type's own methods and inside each service's `submit`.
pub struct Client {
    config: Config,
    token: Token,
    sdk_info: SdkInfo,
    is_first_boot: bool,
    /// idempotency guard
    ready: bool,
    metrics: Option<Arc<Metrics>>,
    error_tracking: Option<Arc<ErrorTracker>>,
    feature_flags: Option<Arc<FeatureFlags>>,
    metrics_task: Option<JoinHandle<()>>,
    error_tracking_task: Option<JoinHandle<()>>,
    runtime_context: Option<RuntimeContext>,
}

impl Client {
    /// The resolved config (with `server_id` filled in from the
    /// first-boot state file).
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The project token this client was built with.
    pub fn token(&self) -> &Token {
        &self.token
    }

    /// The SDK information this client was built with.
    pub fn sdk_info(&self) -> &SdkInfo {
        &self.sdk_info
    }

    /// The metrics service, if the developer enabled it at builder time.
    /// `None` here means metrics were never constructed at all, this is
    /// distinct from the service being constructed but not submitting
    /// because `Config::submit_metrics` is `false`.
    pub fn metrics(&self) -> Option<&Arc<Metrics>> {
        self.metrics.as_ref()
    }

    /// The error tracker, if the developer enabled it at builder time.
    pub fn error_tracking(&self) -> Option<&Arc<ErrorTracker>> {
        self.error_tracking.as_ref()
    }

    /// The feature flags service, if the developer enabled it at
    /// builder time.
    pub fn feature_flags(&self) -> Option<&Arc<FeatureFlags>> {
        self.feature_flags.as_ref()
    }

    /// Whether this is the very first run for this client's `server_id`
    /// (i.e. [`Client::start`] logged the first-boot notice and returned
    /// without submitting anything).
    pub fn is_first_boot(&self) -> bool {
        self.is_first_boot
    }

    /// Whether [`Client::start`] has been called (and [`Client::shutdown`]
    /// hasn't since undone it). Mirrors Java's `SimpleContext.ready`.
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// Starts the client.
    ///
    /// A no-op (logs a warning) if already started
    pub fn start(&mut self) {
        if self.ready {
            log::warn!("Client::start() was called twice; ignoring.");
            return;
        }
        self.ready = true;

        if self.is_first_boot {
            log_first_boot_notice(self);
            return;
        }

        let metrics_ref = &self.metrics;
        let error_tracking_ref = &self.error_tracking;
        let config_ref = &self.config;
        let mut runtime_context = self.runtime_context.take();
        let (metrics_task, error_tracking_task) = RuntimeContext::ensure_and_run(
            &mut runtime_context,
            || {
                let metrics_task = if let Some(metrics) = metrics_ref {
                    if config_ref.submit_metrics() {
                        Some(Arc::clone(metrics).start_submitting())
                    } else {
                        log::info!(
                            "Metrics service enabled by developer but disabled via Config::submit_metrics; not starting"
                        );
                        None
                    }
                } else {
                    None
                };

                let error_tracking_task = if let Some(error_tracking) = error_tracking_ref {
                    if config_ref.error_tracking() {
                        error_tracking.install_panic_hook();
                        let handle = Arc::clone(error_tracking).start_submitting();
                        // async half of the cross-wiring from ClientBuilder::build
                        if let Some(metrics) = metrics_ref {
                            error_tracking.spawn_metrics_event_listener(metrics.subscribe());
                        }
                        Some(handle)
                    } else {
                        log::info!(
                            "Error tracking service enabled by developer but disabled via Config::error_tracking; not starting"
                        );
                        None
                    }
                } else {
                    None
                };

                (metrics_task, error_tracking_task)
            },
        );
        self.metrics_task = metrics_task;
        self.error_tracking_task = error_tracking_task;
        self.runtime_context = runtime_context;

        // feature flags are pull-based, nothing to spawn here
        if self.feature_flags.is_some() && !self.config.enabled() {
            log::info!(
                "Feature flags service enabled by developer but disabled via Config::enabled; fetches will be skipped by callers checking Client::config()"
            );
        }
    }

    /// Whether the feature flags service is both developer-enabled and
    /// allowed to run per `Config`. Feature flags have no scheduler to
    /// start/stop, so this is the runtime check callers should make
    /// before calling `when_ready`/`fetch` on [`Client::feature_flags`],
    /// mirroring the AND-gating [`Client::start`] applies to metrics and
    /// error tracking automatically.
    pub fn feature_flags_active(&self) -> bool {
        self.feature_flags.is_some() && self.config.enabled()
    }

    /// Shuts the client down: performs a final best-effort flush of
    /// metrics and errors, aborts the submission tasks, and detaches the
    /// panic hook. A no-op for services the developer never enabled, and
    /// a no-op entirely if [`Client::start`] was never called (or a
    /// prior `shutdown()` already ran), mirrors Java's
    /// `SimpleContext.shutdown()` guard.
    pub async fn shutdown(&mut self) {
        if !self.ready {
            return;
        }
        self.ready = false;

        if let Some(handle) = self.metrics_task.take() {
            handle.abort();
        }
        if let Some(handle) = self.error_tracking_task.take() {
            handle.abort();
        }

        if let Some(metrics) = &self.metrics {
            metrics.shutdown().await;
        }
        if let Some(error_tracking) = &self.error_tracking {
            error_tracking.shutdown().await;
        }

        // If `start()` had to create its own background runtime (because
        // no runtime was already running on the caller's thread), tear it
        // down now that its tasks are aborted/flushed, so its thread pool
        // doesn't outlive the Client. Dropping a `tokio::runtime::Runtime`
        // from inside another async context would panic, so this is
        // deferred to a blocking thread.
        if let Some(RuntimeContext::Owned(rt)) = self.runtime_context.take() {
            let _ = tokio::task::spawn_blocking(move || drop(rt)).await;
        }
    }
}

/// Logs the first-boot notice: what FastStats is, what would be sent,
/// and exactly how to disable each category.
fn log_first_boot_notice(client: &Client) {
    log::info!(
        "FastStats is collecting anonymous usage data to help improve this project. \
         This is the first run, so nothing is submitted yet, submission begins on the next run."
    );
    if client.metrics.is_some() {
        log::info!(
            "  - Metrics (platform info plus any custom metrics registered by this application) \
             will be sent every 30 minutes. Disable with FASTSTATS_SUBMIT_METRICS=false or by editing the config file."
        );
    }
    if client.error_tracking.is_some() {
        log::info!(
            "  - Error reports (panics and manually tracked errors) will be sent every 30 minutes. \
             Disable with FASTSTATS_ERROR_TRACKING=false or by editing the config file."
        );
    }
    if client.feature_flags.is_some() {
        log::info!(
            "  - Feature flag values will be fetched on demand. \
             Disable all FastStats data collection with FASTSTATS_ENABLED=false or by editing the config file."
        );
    }
    log::info!(
        "  - To disable FastStats entirely, set the FASTSTATS_ENABLED=false environment variable. \
         To start submitting immediately instead of waiting for the next run, set FASTSTATS_ENABLED=true."
    );
}

/// First-boot state tracking: the whole resolved [`Config`] is always
/// persisted to a local state file, read back and AND-merged with
/// whatever `Config` the developer supplied.
mod state {
    use std::env;
    use std::fs;
    use std::path::{Path, PathBuf};
    #[cfg(test)]
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use uuid::Uuid;

    use crate::domain::Config;
    use crate::error::{Error, Result};

    const STATE_DIR_ENV: &str = "FASTSTATS_STATE_DIR";
    const STATE_FILE_NAME: &str = "config.properties";
    const DEFAULT_STATE_DIR: &str = "faststats";
    /// Explicit opt-in that forces submission to start even on first
    /// boot, overriding the process-global first-run skip.
    const FORCE_ENABLED_ENV: &str = "FASTSTATS_ENABLED";

    /// Process-global first-run gate
    static FIRST_RUN_HANDLED: AtomicBool = AtomicBool::new(false);

    /// Resets the process-global first-run gate. Test-only: real
    /// processes never need to un-claim this gate.
    #[cfg(test)]
    pub(crate) fn reset_first_run_gate_for_tests() {
        FIRST_RUN_HANDLED.store(false, Ordering::SeqCst);
    }

    /// Serializes every tests in the crate that touches
    /// `FASTSTATS_STATE_DIR`/`FASTSTATS_ENABLED`/the process-global
    /// first-run gate, since all of those are process-wide state shared
    /// across `state::tests` and the outer `client::tests` module.
    #[cfg(test)]
    pub(crate) static STATE_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// The result of resolving first-boot state: the persisted
    /// [`Config`] (as read from, or just written to, the state file),
    /// and whether this call is the one that should skip submission for
    /// this run.
    pub struct State {
        pub config: Config,
        pub is_first_boot: bool,
    }

    /// Loads the persisted [`Config`] if a state file already exists,
    /// else writes `default_config` (with a freshly generated
    /// `server_id`) as the new state file.
    pub fn load_or_init(default_config: &Config) -> Result<State> {
        let path = state_file_path();

        let (config, file_existed) = match fs::read_to_string(&path) {
            Ok(contents) => (Config::from_persisted_str(&contents, Uuid::new_v4()), true),
            Err(_) => (default_config.clone_with_server_id(Uuid::new_v4()), false),
        };

        if let Err(e) = write_state_file(&path, &config) {
            log::warn!(
                "Could not persist FastStats state file at {}: {e}. \
                 Every run will be treated as first boot until this is writable.",
                path.display()
            );
        }

        let is_first_boot = if !file_existed {
            resolve_first_boot()
        } else {
            false
        };

        Ok(State {
            config,
            is_first_boot,
        })
    }

    /// Applies the process-global first-run gate: if some earlier
    /// `Client` in this process already went through its own first run,
    /// this run is not first-boot either.
    fn resolve_first_boot() -> bool {
        if FIRST_RUN_HANDLED.swap(true, Ordering::SeqCst) {
            return false;
        }
        !force_enabled_override()
    }

    /// Whether `FASTSTATS_ENABLED` was explicitly set to `true`.
    fn force_enabled_override() -> bool {
        env::var(FORCE_ENABLED_ENV)
            .map(|v| v.trim().eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    }

    fn write_state_file(path: &Path, config: &Config) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(Error::from)?;
        }
        fs::write(path, config.to_persisted_string()).map_err(Error::from)?;
        Ok(())
    }

    fn state_file_path() -> PathBuf {
        let dir = env::var(STATE_DIR_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_STATE_DIR));
        dir.join(STATE_FILE_NAME)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn reset_first_run_gate() {
            FIRST_RUN_HANDLED.store(false, Ordering::SeqCst);
        }

        #[test]
        fn first_call_in_fresh_dir_is_first_boot_and_persists() {
            let _guard = STATE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            reset_first_run_gate();
            // SAFETY: `_guard` (STATE_TEST_LOCK) serializes all env-mutating
            // tests in this module.
            unsafe { env::remove_var(FORCE_ENABLED_ENV) };
            let dir = env::temp_dir().join(format!("faststats-tests-{}", Uuid::new_v4()));
            unsafe { env::set_var(STATE_DIR_ENV, &dir) };

            let default_config = Config::new(Uuid::nil());
            let first = load_or_init(&default_config).expect("loads");
            assert!(first.is_first_boot);

            let second = load_or_init(&default_config).expect("loads again");
            assert!(!second.is_first_boot);
            assert_eq!(second.config.server_id(), first.config.server_id());

            unsafe { env::remove_var(STATE_DIR_ENV) };
            let _ = fs::remove_dir_all(&dir);
        }

        #[test]
        fn corrupt_state_file_is_treated_as_first_boot() {
            let _guard = STATE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            reset_first_run_gate();
            // SAFETY: see `first_call_in_fresh_dir_is_first_boot_and_persists`.
            unsafe { env::remove_var(FORCE_ENABLED_ENV) };
            let dir = env::temp_dir().join(format!("faststats-tests-{}", Uuid::new_v4()));
            fs::create_dir_all(&dir).expect("create tests dir");
            fs::write(
                dir.join(STATE_FILE_NAME),
                "not a config file at all, no lines parse",
            )
            .expect("write corrupt file");
            unsafe { env::set_var(STATE_DIR_ENV, &dir) };

            // a file that exists but fails to parse still counts as "existed"
            let result =
                load_or_init(&Config::new(Uuid::nil())).expect("loads despite corrupt file");
            assert!(!result.is_first_boot);

            unsafe { env::remove_var(STATE_DIR_ENV) };
            let _ = fs::remove_dir_all(&dir);
        }

        #[test]
        fn first_run_gate_is_process_global_not_per_directory() {
            let _guard = STATE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            reset_first_run_gate();
            // SAFETY: see `first_call_in_fresh_dir_is_first_boot_and_persists`.
            unsafe { env::remove_var(FORCE_ENABLED_ENV) };
            let dir_a = env::temp_dir().join(format!("faststats-tests-{}", Uuid::new_v4()));
            let dir_b = env::temp_dir().join(format!("faststats-tests-{}", Uuid::new_v4()));

            unsafe { env::set_var(STATE_DIR_ENV, &dir_a) };
            let first = load_or_init(&Config::new(Uuid::nil())).expect("loads");
            assert!(first.is_first_boot);

            // a separate state directory still isn't first-boot; the gate is process-wide
            unsafe { env::set_var(STATE_DIR_ENV, &dir_b) };
            let second = load_or_init(&Config::new(Uuid::nil())).expect("loads");
            assert!(!second.is_first_boot);

            unsafe { env::remove_var(STATE_DIR_ENV) };
            let _ = fs::remove_dir_all(&dir_a);
            let _ = fs::remove_dir_all(&dir_b);
        }

        #[test]
        fn force_enabled_env_overrides_first_boot_skip() {
            let _guard = STATE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            reset_first_run_gate();
            let dir = env::temp_dir().join(format!("faststats-tests-{}", Uuid::new_v4()));
            // SAFETY: see `first_call_in_fresh_dir_is_first_boot_and_persists`.
            unsafe {
                env::set_var(STATE_DIR_ENV, &dir);
                env::set_var(FORCE_ENABLED_ENV, "true");
            }

            let result = load_or_init(&Config::new(Uuid::nil())).expect("loads");
            assert!(!result.is_first_boot);

            unsafe {
                env::remove_var(STATE_DIR_ENV);
                env::remove_var(FORCE_ENABLED_ENV);
            }
            let _ = fs::remove_dir_all(&dir);
        }

        #[test]
        fn config_file_is_always_written_back() {
            let _guard = STATE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            reset_first_run_gate();
            // SAFETY: see `first_call_in_fresh_dir_is_first_boot_and_persists`.
            unsafe { env::remove_var(FORCE_ENABLED_ENV) };
            let dir = env::temp_dir().join(format!("faststats-tests-{}", Uuid::new_v4()));
            unsafe { env::set_var(STATE_DIR_ENV, &dir) };

            load_or_init(&Config::new(Uuid::nil())).expect("loads");
            assert!(dir.join(STATE_FILE_NAME).exists());

            unsafe { env::remove_var(STATE_DIR_ENV) };
            let _ = fs::remove_dir_all(&dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;

    fn fresh_state_dir() -> PathBuf {
        env::temp_dir().join(format!("faststats-client-tests-{}", Uuid::new_v4()))
    }

    fn test_token() -> Token {
        Token::new("a".repeat(32)).expect("valid token")
    }

    fn test_project_name() -> &'static str {
        "tests-project"
    }

    fn test_version() -> &'static str {
        "0.0.0"
    }

    #[test]
    fn fresh_state_dir_produces_first_boot_client() {
        let _guard = state::STATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        state::reset_first_run_gate_for_tests();
        let dir = fresh_state_dir();
        unsafe {
            env::set_var("FASTSTATS_STATE_DIR", &dir);
        }

        let client = ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid builder")
            .build()
            .expect("builds");
        assert!(client.is_first_boot());

        unsafe {
            env::remove_var("FASTSTATS_STATE_DIR");
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn force_enabled_env_makes_first_client_not_first_boot() {
        let _guard = state::STATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        state::reset_first_run_gate_for_tests();
        let dir = fresh_state_dir();
        unsafe {
            env::set_var("FASTSTATS_STATE_DIR", &dir);
            env::set_var("FASTSTATS_ENABLED", "true");
        }

        let client = ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid builder")
            .build()
            .expect("builds");
        assert!(!client.is_first_boot());

        unsafe {
            env::remove_var("FASTSTATS_STATE_DIR");
            env::remove_var("FASTSTATS_ENABLED");
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn developer_config_and_persisted_file_config_are_anded_together() {
        let _guard = state::STATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        state::reset_first_run_gate_for_tests();
        let dir = fresh_state_dir();
        unsafe {
            env::set_var("FASTSTATS_STATE_DIR", &dir);
        }

        // first run: developer config disables submit_metrics and persists it
        let disabling_config = Config::new(Uuid::nil()).set_submit_metrics(false);
        let _ = ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid builder")
            .config(disabling_config)
            .build()
            .expect("builds");

        // second run: persisted file still has it disabled, and wins via AND
        let enabling_config = Config::new(Uuid::nil()).set_submit_metrics(true);
        let client = ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid builder")
            .config(enabling_config)
            .build()
            .expect("builds");
        assert!(!client.config().submit_metrics());

        unsafe {
            env::remove_var("FASTSTATS_STATE_DIR");
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn state_file_is_written_under_the_configured_directory() {
        let _guard = state::STATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        state::reset_first_run_gate_for_tests();
        let dir = fresh_state_dir();
        unsafe {
            env::set_var("FASTSTATS_STATE_DIR", &dir);
        }

        let _ = ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid builder")
            .build()
            .expect("builds");
        assert!(dir.join("config.properties").exists());

        unsafe {
            env::remove_var("FASTSTATS_STATE_DIR");
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pre_existing_state_dir_produces_non_first_boot_client() {
        let _guard = state::STATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        state::reset_first_run_gate_for_tests();
        let dir = fresh_state_dir();
        unsafe {
            env::set_var("FASTSTATS_STATE_DIR", &dir);
        }

        // first client creates the state file
        let _ = ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid builder")
            .build()
            .expect("builds");
        // second client (same state dir) sees it as pre-existing
        let client = ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid builder")
            .build()
            .expect("builds");
        assert!(!client.is_first_boot());

        unsafe {
            env::remove_var("FASTSTATS_STATE_DIR");
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_boot_start_does_not_spawn_submission_tasks() {
        let _guard = state::STATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        state::reset_first_run_gate_for_tests();
        let dir = fresh_state_dir();
        unsafe {
            env::set_var("FASTSTATS_STATE_DIR", &dir);
            env::set_var("FASTSTATS_METRICS_SERVER", "http://127.0.0.1:1");
            env::set_var("FASTSTATS_ERROR_TRACKER_SERVER", "http://127.0.0.1:1");
        }

        let mut client = ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid builder")
            .build()
            .expect("builds");
        client.start();

        assert!(client.metrics_task.is_none());
        assert!(client.error_tracking_task.is_none());

        unsafe {
            env::remove_var("FASTSTATS_STATE_DIR");
            env::remove_var("FASTSTATS_METRICS_SERVER");
            env::remove_var("FASTSTATS_ERROR_TRACKER_SERVER");
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn start_called_twice_is_idempotent() {
        let _guard = state::STATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        state::reset_first_run_gate_for_tests();
        let dir = fresh_state_dir();
        unsafe {
            env::set_var("FASTSTATS_STATE_DIR", &dir);
            env::set_var("FASTSTATS_METRICS_SERVER", "http://127.0.0.1:1");
            env::set_var("FASTSTATS_ERROR_TRACKER_SERVER", "http://127.0.0.1:1");
        }

        // pre-create the state file so this run isn't first-boot
        let _ = ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid builder")
            .build()
            .expect("builds");

        let mut client = ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid builder")
            .build()
            .expect("builds");
        assert!(!client.is_ready());
        client.start();
        assert!(client.is_ready());
        let first_task_present = client.metrics_task.is_some();

        // calling start() again must not replace the task or panic
        client.start();
        assert_eq!(client.metrics_task.is_some(), first_task_present);

        unsafe {
            env::remove_var("FASTSTATS_STATE_DIR");
            env::remove_var("FASTSTATS_METRICS_SERVER");
            env::remove_var("FASTSTATS_ERROR_TRACKER_SERVER");
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn shutdown_before_start_is_a_no_op() {
        let _guard = state::STATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        state::reset_first_run_gate_for_tests();
        let dir = fresh_state_dir();
        unsafe {
            env::set_var("FASTSTATS_STATE_DIR", &dir);
        }

        let mut client = ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid builder")
            .build()
            .expect("builds");
        assert!(!client.is_ready());
        // never started, shutdown() should just return
        client.shutdown().await;
        assert!(!client.is_ready());

        unsafe {
            env::remove_var("FASTSTATS_STATE_DIR");
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn developer_enabled_but_config_disabled_service_stays_off() {
        let _guard = state::STATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        state::reset_first_run_gate_for_tests();
        let dir = fresh_state_dir();
        unsafe {
            env::set_var("FASTSTATS_STATE_DIR", &dir);
        }

        // pre-create the state file so this run isn't first-boot
        let _ = ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid builder")
            .build()
            .expect("builds");

        let config = Config::new(Uuid::nil()).set_submit_metrics(false);
        let mut client = ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid builder")
            .config(config)
            .metrics(true)
            // error tracking isn't under tests here and needs a runtime this tests doesn't have
            .error_tracking(false)
            .build()
            .expect("builds");

        client.start();
        // submit_metrics(false) wins: no task spawned
        assert!(client.metrics_task.is_none());
        assert!(client.metrics().is_some());

        unsafe {
            env::remove_var("FASTSTATS_STATE_DIR");
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn developer_disabled_service_is_never_constructed_even_if_config_enables_it() {
        let _guard = state::STATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        state::reset_first_run_gate_for_tests();
        let dir = fresh_state_dir();
        unsafe {
            env::set_var("FASTSTATS_STATE_DIR", &dir);
        }

        let _ = ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid builder")
            .build()
            .expect("builds");

        let config = Config::new(Uuid::nil()).set_submit_metrics(true);
        let client = ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid builder")
            .config(config)
            .metrics(false)
            .build()
            .expect("builds");

        // nothing to enable: the service was never constructed
        assert!(client.metrics().is_none());

        unsafe {
            env::remove_var("FASTSTATS_STATE_DIR");
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn feature_flags_active_requires_both_developer_and_config_enable() {
        let _guard = state::STATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        state::reset_first_run_gate_for_tests();
        let dir = fresh_state_dir();
        unsafe {
            env::set_var("FASTSTATS_STATE_DIR", &dir);
        }

        let _ = ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid builder")
            .build()
            .expect("builds");

        let config = Config::new(Uuid::nil()).set_enabled(false);
        let client = ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid builder")
            .config(config)
            .feature_flags(true)
            .build()
            .expect("builds");

        assert!(client.feature_flags().is_some());
        assert!(!client.feature_flags_active());

        unsafe {
            env::remove_var("FASTSTATS_STATE_DIR");
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn shutdown_on_a_never_started_client_does_not_panic() {
        let _guard = state::STATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        state::reset_first_run_gate_for_tests();
        let dir = fresh_state_dir();
        unsafe {
            env::set_var("FASTSTATS_STATE_DIR", &dir);
            env::set_var("FASTSTATS_METRICS_SERVER", "http://127.0.0.1:1");
            env::set_var("FASTSTATS_ERROR_TRACKER_SERVER", "http://127.0.0.1:1");
        }

        let mut client = ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid builder")
            .build()
            .expect("builds");
        client.shutdown().await;

        unsafe {
            env::remove_var("FASTSTATS_STATE_DIR");
            env::remove_var("FASTSTATS_METRICS_SERVER");
            env::remove_var("FASTSTATS_ERROR_TRACKER_SERVER");
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn error_tracking_context_includes_metrics_snapshot_once_wired() {
        // build() wires the metrics-snapshot half synchronously, before start()
        let _guard = state::STATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        state::reset_first_run_gate_for_tests();
        let dir = fresh_state_dir();
        unsafe {
            env::set_var("FASTSTATS_STATE_DIR", &dir);
        }

        unsafe {
            env::set_var("FASTSTATS_ENABLED", "true");
        } // skip first-boot for this run

        // pre-create the state file so this run isn't first-boot.
        let _ = ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid builder")
            .build()
            .expect("builds");

        let client = ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid builder")
            .build()
            .expect("builds");

        let error_tracking = client
            .error_tracking()
            .expect("error tracking enabled by default");
        error_tracking.track_error("E", Some("boom"), &[], None);

        let data = error_tracking.create_data().expect("payload present");
        // platform metrics should have merged into the error report's context
        assert!(data["context"]["os_name"].is_string());

        unsafe {
            env::remove_var("FASTSTATS_STATE_DIR");
            env::remove_var("FASTSTATS_ENABLED");
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn failing_custom_metric_is_reported_as_tracked_error_after_start() {
        // regression tests for audit items 7/8: a custom metric that
        // fails to compute should show up in error tracking once
        // Client::start has spawned the MetricsEvent listener task
        let _guard = state::STATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        state::reset_first_run_gate_for_tests();
        let dir = fresh_state_dir();
        unsafe {
            env::set_var("FASTSTATS_STATE_DIR", &dir);
            env::set_var("FASTSTATS_ENABLED", "true");
            env::set_var("FASTSTATS_METRICS_SERVER", "http://127.0.0.1:1");
            env::set_var("FASTSTATS_ERROR_TRACKER_SERVER", "http://127.0.0.1:1");
        }

        // long enough that the scheduled submission loop itself won't
        // fire during this tests.
        unsafe {
            env::set_var("FASTSTATS_INITIAL_DELAY", "3600");
        }

        let _ = ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid builder")
            .build()
            .expect("builds");

        let failing_metric = crate::metrics::Metric::try_new("always_fails", || {
            Err::<Option<i32>, _>(crate::error::Error::validation(
                "t",
                "computed metrics boom",
            ))
        })
        .expect("valid metric definition");

        let mut client = ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid builder")
            .metrics_factory(|f| f.add_metric(failing_metric))
            .expect("factory configured")
            .build()
            .expect("builds");

        client.start();

        // trigger metric computation directly rather than waiting on
        // the scheduler, start() has already spawned the listener
        // task that will pick up the resulting MetricsEvent.
        let _ = client.metrics().expect("metrics enabled").snapshot();

        // give the spawned listener task a chance to run.
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        let error_tracking = client.error_tracking().expect("error tracking enabled");
        assert!(!error_tracking.is_empty());

        unsafe {
            env::remove_var("FASTSTATS_STATE_DIR");
            env::remove_var("FASTSTATS_ENABLED");
            env::remove_var("FASTSTATS_METRICS_SERVER");
            env::remove_var("FASTSTATS_ERROR_TRACKER_SERVER");
            env::remove_var("FASTSTATS_INITIAL_DELAY");
        }

        let _ = fs::remove_dir_all(&dir);
    }
}
