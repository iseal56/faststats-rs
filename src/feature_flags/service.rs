//! The feature flag service.
//!
//! Notable differences from metrics/error tracking: TTL is
//! service-level, not per-flag; requests are plain uncompressed JSON
//! (no gzip, no `User-Agent`); a failed fetch is always returned as an
//! error rather than silently falling back to the default value; and
//! concurrent `fetch()` calls for the same id share one in-flight
//! request and result via a `watch` channel.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Url;
use serde_json::{Map, Value};
use tokio::sync::{Mutex as AsyncMutex, watch};
use uuid::Uuid;

use super::cache::CachedValue;
use super::flag::FeatureFlag;
use super::value::FlagValue;
use crate::domain::Attributes;
use crate::error::{Error, Result};
use crate::transport::{Transport, resolve_server_url};
use crate::validated::Id;

const CHECK_PATH: &str = "/v1/check";
const OPT_IN_PATH: &str = "/v1/opt-in";
const OPT_OUT_PATH: &str = "/v1/opt-out";
const FLAGS_SERVER_ENV: &str = "FASTSTATS_FLAGS_SERVER";
const DEFAULT_FLAGS_SERVER: &str = "https://flags.faststats.dev";

/// The default cache TTL (5 minutes).
pub const DEFAULT_TTL: Duration = Duration::from_secs(5 * 60);

/// Outcome of an in-flight fetch, shared with every concurrent caller
/// waiting on the same flag id. `None` means not resolved yet.
type FetchOutcome = Option<Result<FlagValue>>;

/// Builds a [`FeatureFlags`] instance.
pub struct Factory {
    flags: Vec<FeatureFlag>,
    attributes: Option<Attributes>,
    ttl: Duration,
}

impl Factory {
    pub fn new() -> Self {
        Factory {
            flags: Vec::new(),
            attributes: None,
            ttl: DEFAULT_TTL,
        }
    }

    /// Registers a flag. Errors if a flag with the same id was already
    /// added.
    pub fn add_flag(mut self, flag: FeatureFlag) -> Result<Self> {
        if self.flags.iter().any(|f| f.id() == flag.id()) {
            return Err(Error::validation(
                "feature flag id",
                format!("flag already added: {}", flag.id()),
            ));
        }
        self.flags.push(flag);
        Ok(self)
    }

    /// Service-level attributes merged into every flag request;
    /// per-flag attributes take precedence on conflict.
    #[must_use]
    pub fn attributes(mut self, attributes: Attributes) -> Self {
        self.attributes = Some(attributes);
        self
    }

    /// Cache TTL shared by every flag this service manages.
    #[must_use]
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    pub fn build(self, transport: Arc<Transport>, server_id: Uuid) -> Result<FeatureFlags> {
        let base = resolve_server_url(FLAGS_SERVER_ENV, DEFAULT_FLAGS_SERVER)?;
        let check_url = join(&base, CHECK_PATH)?;
        let opt_in_url = join(&base, OPT_IN_PATH)?;
        let opt_out_url = join(&base, OPT_OUT_PATH)?;

        let mut definitions = HashMap::new();
        let mut caches = HashMap::new();
        for flag in self.flags {
            let id = flag.id().clone();
            definitions.insert(id.clone(), flag);
            caches.insert(id, AsyncMutex::new(None::<CachedValue>));
        }

        Ok(FeatureFlags {
            transport,
            check_url,
            opt_in_url,
            opt_out_url,
            server_id,
            attributes: self.attributes,
            ttl: self.ttl,
            definitions,
            caches,
            fetches_in_progress: AsyncMutex::new(HashMap::new()),
        })
    }
}

impl Default for Factory {
    fn default() -> Self {
        Factory::new()
    }
}

fn join(base: &Url, path: &str) -> Result<Url> {
    base.join(path).map_err(|e| Error::InvalidServerUrl {
        env_var: FLAGS_SERVER_ENV,
        reason: e.to_string(),
    })
}

/// TTL-cached flag values fetched from `/v1/check`, with opt-in/
/// opt-out support. Flags are fetched on demand; there's no periodic
/// submission loop like metrics/error tracking have.
pub struct FeatureFlags {
    transport: Arc<Transport>,
    check_url: Url,
    opt_in_url: Url,
    opt_out_url: Url,
    server_id: Uuid,
    attributes: Option<Attributes>,
    ttl: Duration,
    definitions: HashMap<Id, FeatureFlag>,
    caches: HashMap<Id, AsyncMutex<Option<CachedValue>>>,
    /// In-flight `fetch()` calls per flag id, so concurrent requests
    /// for the same flag share one network call and one outcome.
    fetches_in_progress: AsyncMutex<HashMap<Id, Arc<watch::Sender<FetchOutcome>>>>,
}

impl FeatureFlags {
    /// The cache TTL shared by every flag on this service.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Returns the currently cached value for `id`, if one exists and
    /// is still within the TTL, without performing a fetch or awaiting
    /// anything. Uses a non-blocking `try_lock`, so it returns `None`
    /// for an unknown id, an empty/expired cache, or a fetch currently
    /// in progress. Callers needing a guaranteed answer should use
    /// `when_ready`/`fetch` instead.
    pub fn cached(&self, id: &Id) -> Option<FlagValue> {
        let cache = self.caches.get(id)?;
        let guard = cache.try_lock().ok()?;
        let cached = guard.as_ref()?;
        if cached.is_valid(self.ttl) {
            Some(cached.value.clone())
        } else {
            None
        }
    }

    /// Returns the cached value if still valid, else performs a
    /// [`FeatureFlags::fetch`].
    pub async fn when_ready(&self, id: &Id) -> Result<FlagValue> {
        if !self.definitions.contains_key(id) {
            return Err(unknown_flag_error(id));
        }

        if let Some(cache) = self.caches.get(id) {
            let guard = cache.lock().await;
            if let Some(cached) = guard.as_ref()
                && cached.is_valid(self.ttl)
            {
                return Ok(cached.value.clone());
            }
        }

        self.fetch(id).await
    }

    /// The [`std::future::Future`]-returning equivalent of
    /// [`FeatureFlags::when_ready`], for callers that want a `Future`
    /// value to store, pass around, or combine, rather than an
    /// `async fn` awaited immediately. `when_ready`/`fetch` remain the
    /// idiomatic entry points for ordinary use.
    pub fn when_ready_future<'a>(
        &'a self,
        id: &'a Id,
    ) -> Pin<Box<dyn Future<Output = Result<FlagValue>> + Send + 'a>> {
        Box::pin(self.when_ready(id))
    }

    /// The [`std::future::Future`]-returning equivalent of
    /// [`FeatureFlags::fetch`]. See [`FeatureFlags::when_ready_future`].
    pub fn fetch_future<'a>(
        &'a self,
        id: &'a Id,
    ) -> Pin<Box<dyn Future<Output = Result<FlagValue>> + Send + 'a>> {
        Box::pin(self.fetch(id))
    }

    /// Always hits the network for the current value, updating the
    /// cache on success. Concurrent calls for the same `id` share one
    /// in-flight request and one outcome; a failed fetch never
    /// overwrites the cache.
    pub async fn fetch(&self, id: &Id) -> Result<FlagValue> {
        let flag = self
            .definitions
            .get(id)
            .ok_or_else(|| unknown_flag_error(id))?;

        enum Role {
            // The leader drives its own fetch and reports the outcome
            // on the channel.
            Leader,
            Follower(watch::Receiver<FetchOutcome>),
        }

        let role = {
            let mut in_progress = self.fetches_in_progress.lock().await;
            match in_progress.get(id) {
                Some(sender) => Role::Follower(sender.subscribe()),
                None => {
                    let (sender, _receiver) = watch::channel(None);
                    in_progress.insert(id.clone(), Arc::new(sender));
                    Role::Leader
                }
            }
        };

        match role {
            Role::Leader => {
                let outcome = self.fetch_uncached(flag).await;

                if let Ok(value) = &outcome
                    && let Some(cache) = self.caches.get(id)
                {
                    let mut guard = cache.lock().await;
                    *guard = Some(CachedValue::new(value.clone()));
                }

                let sender = {
                    let mut in_progress = self.fetches_in_progress.lock().await;
                    in_progress.remove(id)
                };
                if let Some(sender) = sender {
                    let _ = sender.send(Some(clone_result(&outcome)));
                }

                outcome
            }
            Role::Follower(mut receiver) => {
                // Falls back to a fresh fetch if the leader's sender
                // is dropped without ever sending, rather than hanging.
                loop {
                    let ready = (*receiver.borrow()).as_ref().map(clone_result);
                    if let Some(outcome) = ready {
                        return outcome;
                    }
                    if receiver.changed().await.is_err() {
                        return self.fetch_uncached(flag).await;
                    }
                }
            }
        }
    }

    /// Performs the `/v1/check` network call for `flag`, without
    /// touching the cache. A non-2xx status, an unparseable body, a
    /// missing `"value"`, or a value that doesn't match the flag's
    /// declared type are all reported as an error.
    async fn fetch_uncached(&self, flag: &FeatureFlag) -> Result<FlagValue> {
        let request = self.build_request(flag.id(), flag.attributes_ref());

        let outcome = self
            .transport
            .submit_json(&self.check_url, &request, "feature flag check")
            .await?;

        if !outcome.is_successful() {
            return Err(Error::validation(
                "feature flag response",
                format!(
                    "unexpected response status: {} ({})",
                    outcome.status,
                    outcome.body.as_deref().unwrap_or_default()
                ),
            ));
        }

        let body = outcome
            .body
            .ok_or_else(|| Error::validation("feature flag response", "response had no body"))?;
        let parsed: Value = serde_json::from_str(&body).map_err(|e| {
            Error::validation(
                "feature flag response",
                format!("unexpected response body: {body} ({e})"),
            )
        })?;

        let raw_value = parsed.get("value").ok_or_else(|| {
            Error::validation(
                "feature flag response",
                format!("missing or invalid 'value' in response: {parsed}"),
            )
        })?;

        flag.default().parse_matching(raw_value).ok_or_else(|| {
            Error::validation(
                "feature flag response",
                format!(
                    "value did not match expected type for flag {}: {raw_value}",
                    flag.id()
                ),
            )
        })
    }

    /// Opts in to the given flag's targeting, then triggers a
    /// follow-up fetch.
    pub async fn opt_in(&self, id: &Id) -> Result<FlagValue> {
        self.set_targeting(id, OptDirection::In).await
    }

    /// Opts out of the given flag's targeting, then triggers a
    /// follow-up fetch.
    pub async fn opt_out(&self, id: &Id) -> Result<FlagValue> {
        self.set_targeting(id, OptDirection::Out).await
    }

    /// Shared opt-in/opt-out implementation: POST to the opt path,
    /// and only on success perform the follow-up fetch.
    async fn set_targeting(&self, id: &Id, direction: OptDirection) -> Result<FlagValue> {
        let flag = self
            .definitions
            .get(id)
            .ok_or_else(|| unknown_flag_error(id))?;

        let url = match direction {
            OptDirection::In => &self.opt_in_url,
            OptDirection::Out => &self.opt_out_url,
        };
        let request = self.build_request(id, flag.attributes_ref());

        let outcome = self
            .transport
            .submit_json(url, &request, "feature flag targeting")
            .await?;

        if !outcome.is_successful() {
            return Err(Error::validation(
                "feature flag opt request",
                format!(
                    "opt request failed with status {} ({})",
                    outcome.status,
                    outcome.body.as_deref().unwrap_or_default()
                ),
            ));
        }

        self.fetch(id).await
    }

    /// Builds the `{ "identifier", "key", "attributes"? }` request body
    /// shared by `/v1/check`, `/v1/opt-in`, and `/v1/opt-out`.
    fn build_request(&self, id: &Id, per_flag: Option<&Attributes>) -> Value {
        let mut body = Map::new();
        body.insert(
            "identifier".to_string(),
            Value::from(self.server_id.to_string()),
        );
        body.insert("key".to_string(), Value::from(id.as_str()));

        let merged = Attributes::join(self.attributes.as_ref(), per_flag);
        if !merged.is_empty() {
            body.insert(
                "attributes".to_string(),
                Value::Object(merged.into_json_map()),
            );
        }

        Value::Object(body)
    }
}

enum OptDirection {
    In,
    Out,
}

fn unknown_flag_error(id: &Id) -> Error {
    Error::validation(
        "feature flag id",
        format!("no flag registered for id: {id}"),
    )
}

/// Clones a `Result<FlagValue>`; [`Error`] isn't `Clone`, so a failure
/// is re-described as a fresh [`Error::Validation`] carrying the
/// original's `Display` text.
fn clone_result(result: &Result<FlagValue>) -> Result<FlagValue> {
    match result {
        Ok(value) => Ok(value.clone()),
        Err(e) => Err(Error::validation("feature flag fetch", e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::SdkInfo;
    use crate::validated::Token;
    use std::env::{remove_var, set_var};
    use std::sync::Mutex as StdMutex;

    static ENV_LOCK: StdMutex<()> = StdMutex::new(());

    fn test_transport() -> Arc<Transport> {
        let token = Token::new("a".repeat(32)).expect("valid token");
        let sdk_info = SdkInfo::new(
            "faststats-rs-tests",
            "0.0.0",
            "FastStats Rust SDK v0.0.0 (tests-project:0.0.0)",
        )
        .expect("valid sdk info");
        Arc::new(Transport::new(token, sdk_info).expect("transport builds"))
    }

    fn test_server_id() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-000000000003").expect("valid literal uuid")
    }

    fn test_id(s: &str) -> Id {
        Id::new(s).expect("valid id")
    }

    #[test]
    fn factory_rejects_duplicate_flag_ids() {
        let first = FeatureFlag::new(test_id("dup"), "a");
        let second = FeatureFlag::new(test_id("dup"), "b");

        let factory = Factory::new().add_flag(first).expect("first add ok");
        assert!(factory.add_flag(second).is_err());
    }

    #[test]
    fn factory_defaults_to_five_minute_ttl() {
        let factory = Factory::new();
        assert_eq!(factory.ttl, Duration::from_secs(5 * 60));
    }

    #[tokio::test]
    async fn when_ready_errors_for_unknown_flag() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            remove_var(FLAGS_SERVER_ENV);
        }

        let flags = Factory::new()
            .build(test_transport(), test_server_id())
            .expect("builds");
        assert!(flags.when_ready(&test_id("nonexistent")).await.is_err());
    }

    #[tokio::test]
    async fn when_ready_returns_cached_value_without_network_when_valid() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // Deliberately unreachable: if when_ready() hit the network
        // despite a valid cache entry, this tests would fail.
        unsafe {
            set_var(FLAGS_SERVER_ENV, "http://127.0.0.1:1");
        }

        let flag = FeatureFlag::new(test_id("cached_flag"), "default-val");
        let flags = Factory::new()
            .add_flag(flag)
            .expect("add ok")
            .build(test_transport(), test_server_id())
            .expect("builds");

        {
            let cache = flags
                .caches
                .get(&test_id("cached_flag"))
                .expect("cache entry exists");
            let mut guard = cache.lock().await;
            *guard = Some(CachedValue::new(FlagValue::from("cached-val")));
        }

        let value = flags
            .when_ready(&test_id("cached_flag"))
            .await
            .expect("cached value returned");
        assert_eq!(value, FlagValue::from("cached-val"));

        unsafe {
            remove_var(FLAGS_SERVER_ENV);
        }
    }

    #[tokio::test]
    async fn when_ready_propagates_fetch_failure_rather_than_falling_back() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            set_var(FLAGS_SERVER_ENV, "http://127.0.0.1:1");
        }

        let flag = FeatureFlag::new(test_id("unreachable_flag"), "default-val");
        let flags = Factory::new()
            .add_flag(flag)
            .expect("add ok")
            .build(test_transport(), test_server_id())
            .expect("builds");

        assert!(
            flags
                .when_ready(&test_id("unreachable_flag"))
                .await
                .is_err()
        );

        unsafe {
            remove_var(FLAGS_SERVER_ENV);
        }
    }

    #[tokio::test]
    async fn fetch_against_unreachable_server_returns_err_not_panic() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            set_var(FLAGS_SERVER_ENV, "http://127.0.0.1:1");
        }

        let flag = FeatureFlag::new(test_id("some_flag"), true);
        let flags = Factory::new()
            .add_flag(flag)
            .expect("add ok")
            .build(test_transport(), test_server_id())
            .expect("builds");

        assert!(flags.fetch(&test_id("some_flag")).await.is_err());

        unsafe {
            remove_var(FLAGS_SERVER_ENV);
        }
    }

    #[tokio::test]
    async fn fetch_unknown_flag_id_returns_err() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            remove_var(FLAGS_SERVER_ENV);
        }

        let flags = Factory::new()
            .build(test_transport(), test_server_id())
            .expect("builds");
        assert!(flags.fetch(&test_id("nonexistent")).await.is_err());
    }

    #[tokio::test]
    async fn opt_in_unknown_flag_returns_err() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            remove_var(FLAGS_SERVER_ENV);
        }

        let flags = Factory::new()
            .build(test_transport(), test_server_id())
            .expect("builds");
        assert!(flags.opt_in(&test_id("nonexistent")).await.is_err());
    }

    #[tokio::test]
    async fn opt_in_against_unreachable_server_does_not_touch_cache() {
        // On a failed opt POST, the follow-up fetch never happens and
        // the cache is untouched.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            set_var(FLAGS_SERVER_ENV, "http://127.0.0.1:1");
        }

        let flag = FeatureFlag::new(test_id("opt_flag"), "default-val");
        let flags = Factory::new()
            .add_flag(flag)
            .expect("add ok")
            .build(test_transport(), test_server_id())
            .expect("builds");

        {
            let cache = flags
                .caches
                .get(&test_id("opt_flag"))
                .expect("cache entry exists");
            let mut guard = cache.lock().await;
            *guard = Some(CachedValue::new(FlagValue::from("stale-val")));
        }

        assert!(flags.opt_in(&test_id("opt_flag")).await.is_err());

        let cache = flags
            .caches
            .get(&test_id("opt_flag"))
            .expect("cache entry exists");
        let guard = cache.lock().await;
        assert_eq!(
            guard.as_ref().map(|c| c.value.clone()),
            Some(FlagValue::from("stale-val"))
        );

        unsafe {
            remove_var(FLAGS_SERVER_ENV);
        }
    }

    #[test]
    fn build_request_merges_service_and_per_flag_attributes() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            remove_var(FLAGS_SERVER_ENV);
        }

        let mut service_attrs = Attributes::empty();
        service_attrs.put("env", "prod").expect("valid attribute");
        service_attrs
            .put("shared", "service")
            .expect("valid attribute");

        let mut flag_attrs = Attributes::empty();
        flag_attrs.put("shared", "flag").expect("valid attribute");
        flag_attrs.put("cohort", "beta").expect("valid attribute");

        let flag = FeatureFlag::new(test_id("attributed_flag"), true).attributes(flag_attrs);
        let flags = Factory::new()
            .attributes(service_attrs)
            .add_flag(flag)
            .expect("add ok")
            .build(test_transport(), test_server_id())
            .expect("builds");

        let request = flags.build_request(
            &test_id("attributed_flag"),
            flags
                .definitions
                .get(&test_id("attributed_flag"))
                .unwrap()
                .attributes_ref(),
        );
        assert_eq!(
            request["identifier"],
            "00000000-0000-0000-0000-000000000003"
        );
        assert_eq!(request["key"], "attributed_flag");
        assert_eq!(request["attributes"]["env"], "prod");
        // Per-flag wins over service-level on conflict.
        assert_eq!(request["attributes"]["shared"], "flag");
        assert_eq!(request["attributes"]["cohort"], "beta");
    }

    #[test]
    fn build_request_omits_attributes_when_none_registered() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            remove_var(FLAGS_SERVER_ENV);
        }

        let flags = Factory::new()
            .build(test_transport(), test_server_id())
            .expect("builds");
        let request = flags.build_request(&test_id("plain_flag"), None);
        assert!(request.get("attributes").is_none());
    }

    #[tokio::test]
    async fn concurrent_fetches_for_same_id_share_one_network_call() {
        // No mock server is available, so this asserts the weaker
        // property that both concurrent calls resolve to the same
        // kind of outcome via the shared watch-channel path.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            set_var(FLAGS_SERVER_ENV, "http://127.0.0.1:1");
        }

        let flag = FeatureFlag::new(test_id("shared_flag"), true);
        let flags = Arc::new(
            Factory::new()
                .add_flag(flag)
                .expect("add ok")
                .build(test_transport(), test_server_id())
                .expect("builds"),
        );

        let flags_a = flags.clone();
        let flags_b = flags.clone();
        let (result_a, result_b) = tokio::join!(
            tokio::spawn(async move { flags_a.fetch(&test_id("shared_flag")).await }),
            tokio::spawn(async move { flags_b.fetch(&test_id("shared_flag")).await }),
        );

        assert!(result_a.expect("task a completes").is_err());
        assert!(result_b.expect("task b completes").is_err());

        unsafe {
            remove_var(FLAGS_SERVER_ENV);
        }
    }

    #[tokio::test]
    async fn cached_returns_none_for_unregistered_flag() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            remove_var(FLAGS_SERVER_ENV);
        }

        let flags = Factory::new()
            .build(test_transport(), test_server_id())
            .expect("builds");
        assert_eq!(flags.cached(&test_id("nonexistent")), None);
    }

    #[tokio::test]
    async fn cached_returns_none_when_nothing_fetched_yet() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            remove_var(FLAGS_SERVER_ENV);
        }

        let flag = FeatureFlag::new(test_id("never_fetched"), "default-val");
        let flags = Factory::new()
            .add_flag(flag)
            .expect("add ok")
            .build(test_transport(), test_server_id())
            .expect("builds");

        assert_eq!(flags.cached(&test_id("never_fetched")), None);
    }

    #[tokio::test]
    async fn cached_returns_value_without_network_when_valid() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // Deliberately unreachable: cached() must never touch the
        // network regardless of cache state.
        unsafe {
            set_var(FLAGS_SERVER_ENV, "http://127.0.0.1:1");
        }

        let flag = FeatureFlag::new(test_id("sync_cached_flag"), "default-val");
        let flags = Factory::new()
            .add_flag(flag)
            .expect("add ok")
            .build(test_transport(), test_server_id())
            .expect("builds");

        {
            let cache = flags
                .caches
                .get(&test_id("sync_cached_flag"))
                .expect("cache entry exists");
            let mut guard = cache.lock().await;
            *guard = Some(CachedValue::new(FlagValue::from("cached-sync-val")));
        }

        assert_eq!(
            flags.cached(&test_id("sync_cached_flag")),
            Some(FlagValue::from("cached-sync-val"))
        );

        unsafe {
            remove_var(FLAGS_SERVER_ENV);
        }
    }

    #[tokio::test]
    async fn when_ready_future_resolves_like_when_ready() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            set_var(FLAGS_SERVER_ENV, "http://127.0.0.1:1");
        }

        let flag = FeatureFlag::new(test_id("future_flag"), "default-val");
        let flags = Factory::new()
            .add_flag(flag)
            .expect("add ok")
            .build(test_transport(), test_server_id())
            .expect("builds");

        assert!(
            flags
                .when_ready_future(&test_id("future_flag"))
                .await
                .is_err()
        );

        unsafe {
            remove_var(FLAGS_SERVER_ENV);
        }
    }

    #[tokio::test]
    async fn fetch_future_resolves_like_fetch() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            set_var(FLAGS_SERVER_ENV, "http://127.0.0.1:1");
        }

        let flag = FeatureFlag::new(test_id("fetch_future_flag"), "default-val");
        let flags = Factory::new()
            .add_flag(flag)
            .expect("add ok")
            .build(test_transport(), test_server_id())
            .expect("builds");

        assert!(
            flags
                .fetch_future(&test_id("fetch_future_flag"))
                .await
                .is_err()
        );

        unsafe {
            remove_var(FLAGS_SERVER_ENV);
        }
    }
}
