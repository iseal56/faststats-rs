//! FastStats configuration: end-user-controlled toggles.

use std::env;

use uuid::Uuid;

/// End-user-controlled FastStats configuration. All toggles default to
/// `true` and can be overridden via environment variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    server_id: Uuid,
    enabled: bool,
    submit_metrics: bool,
    error_tracking: bool,
    additional_metrics: bool,
    debug: bool,
}

impl Config {
    /// Creates a new [`Config`] with all toggles set to `true`.
    pub fn new(server_id: Uuid) -> Self {
        Config {
            server_id,
            enabled: true,
            submit_metrics: true,
            error_tracking: true,
            additional_metrics: true,
            debug: true,
        }
    }

    /// Applies environment-variable overrides on top of this config.
    #[must_use]
    pub fn with_env_overrides(mut self) -> Self {
        if let Some(value) = read_bool_env("FASTSTATS_ENABLED") {
            self.enabled = value;
        }
        if let Some(value) = read_bool_env("FASTSTATS_SUBMIT_METRICS") {
            self.submit_metrics = value;
        }
        if let Some(value) = read_bool_env("FASTSTATS_ERROR_TRACKING") {
            self.error_tracking = value;
        }
        if let Some(value) = read_bool_env("FASTSTATS_ADDITIONAL_METRICS") {
            self.additional_metrics = value;
        }
        if let Some(value) = read_bool_env("FASTSTATS_DEBUG") {
            self.debug = value;
        }
        self
    }

    /// Shorthand for [`Config::new`] followed by [`Config::with_env_overrides`].
    pub fn from_env(server_id: Uuid) -> Self {
        Config::new(server_id).with_env_overrides()
    }

    /// The server id.
    pub fn server_id(&self) -> Uuid {
        self.server_id
    }

    /// Returns a copy of this config with `server_id` replaced, all
    /// other fields unchanged. Used by [`crate::client::ClientBuilder`]
    /// to graft the persisted, state-file-resolved `server_id` onto a
    /// developer-supplied `Config` (whose own `server_id`, e.g. from
    /// `Config::new(Uuid::nil())`, is only ever a placeholder).
    #[must_use]
    pub fn clone_with_server_id(&self, server_id: Uuid) -> Self {
        Config {
            server_id,
            ..self.clone()
        }
    }

    /// Whether all FastStats features are enabled.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Whether metrics submission is enabled.
    pub fn submit_metrics(&self) -> bool {
        self.submit_metrics
    }

    /// Whether error tracking is enabled.
    pub fn error_tracking(&self) -> bool {
        self.error_tracking
    }

    /// Whether additional (developer-registered custom) metrics are enabled.
    pub fn additional_metrics(&self) -> bool {
        self.additional_metrics
    }

    /// Whether debug logging is enabled.
    pub fn debug(&self) -> bool {
        self.debug
    }

    /// Builder-style setter for `enabled`.
    #[must_use]
    pub fn set_enabled(mut self, value: bool) -> Self {
        self.enabled = value;
        self
    }

    /// Builder-style setter for `submit_metrics`.
    #[must_use]
    pub fn set_submit_metrics(mut self, value: bool) -> Self {
        self.submit_metrics = value;
        self
    }

    /// Builder-style setter for `error_tracking`.
    #[must_use]
    pub fn set_error_tracking(mut self, value: bool) -> Self {
        self.error_tracking = value;
        self
    }

    /// Builder-style setter for `additional_metrics`.
    #[must_use]
    pub fn set_additional_metrics(mut self, value: bool) -> Self {
        self.additional_metrics = value;
        self
    }

    /// Builder-style setter for `debug`.
    #[must_use]
    pub fn set_debug(mut self, value: bool) -> Self {
        self.debug = value;
        self
    }

    /// Combines this config with another by ANDing every boolean toggle
    /// together (a feature is only enabled if both configs enable it).
    /// `server_id` is taken from `self`.
    #[must_use]
    pub fn and(&self, other: &Config) -> Self {
        Config {
            server_id: self.server_id,
            enabled: self.enabled && other.enabled,
            submit_metrics: self.submit_metrics && other.submit_metrics,
            error_tracking: self.error_tracking && other.error_tracking,
            additional_metrics: self.additional_metrics && other.additional_metrics,
            debug: self.debug && other.debug,
        }
    }

    /// Serializes this config to a simple `key=value`-per-line text
    /// format, one line per field, for on-disk persistence.
    pub(crate) fn to_persisted_string(&self) -> String {
        format!(
            "server_id={}\nenabled={}\nsubmit_metrics={}\nerror_tracking={}\nadditional_metrics={}\ndebug={}\n",
            self.server_id, self.enabled, self.submit_metrics, self.error_tracking, self.additional_metrics, self.debug
        )
    }

    /// Parses the `key=value`-per-line format written by
    /// [`Config::to_persisted_string`]. Each field is independently
    /// defaulted (and doesn't fail the whole parse) if its line is
    /// missing or unparseable. A `server_id` is required, since
    /// there's no reasonable default for it.
    pub(crate) fn from_persisted_str(contents: &str, default_server_id: Uuid) -> Self {
        let mut config = Config::new(default_server_id);
        for line in contents.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "server_id" => {
                    if let Ok(id) = Uuid::parse_str(value) {
                        config.server_id = id;
                    }
                }
                "enabled" => {
                    if let Ok(v) = value.parse() {
                        config.enabled = v;
                    }
                }
                "submit_metrics" => {
                    if let Ok(v) = value.parse() {
                        config.submit_metrics = v;
                    }
                }
                "error_tracking" => {
                    if let Ok(v) = value.parse() {
                        config.error_tracking = v;
                    }
                }
                "additional_metrics" => {
                    if let Ok(v) = value.parse() {
                        config.additional_metrics = v;
                    }
                }
                "debug" => {
                    if let Ok(v) = value.parse() {
                        config.debug = v;
                    }
                }
                _ => {}
            }
        }
        config
    }
}

/// Reads an environment variable and parses it as a boolean.
fn read_bool_env(name: &str) -> Option<bool> {
    let value = env::var(name).ok()?;
    match value.trim().to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Env var tests share process-global state, so serialize them.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn fixed_uuid() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-000000000001")
            .expect("valid literal UUID")
    }

    #[test]
    fn defaults_are_all_true() {
        let config = Config::new(fixed_uuid());
        assert!(config.enabled());
        assert!(config.submit_metrics());
        assert!(config.error_tracking());
        assert!(config.additional_metrics());
        assert!(config.debug());
    }

    #[test]
    fn server_id_round_trips() {
        let id = fixed_uuid();
        let config = Config::new(id);
        assert_eq!(config.server_id(), id);
    }

    #[test]
    fn clone_with_server_id_replaces_only_server_id() {
        let original = Config::new(fixed_uuid()).set_debug(false);
        let replaced_id = Uuid::parse_str("00000000-0000-0000-0000-000000000002")
            .expect("valid literal uuid");
        let updated = original.clone_with_server_id(replaced_id);

        assert_eq!(updated.server_id(), replaced_id);
        assert!(!updated.debug());
        assert!(updated.enabled());
    }

    #[test]
    fn builder_setters_override_individually() {
        let config = Config::new(fixed_uuid())
            .set_submit_metrics(false)
            .set_debug(false);
        assert!(config.enabled());
        assert!(!config.submit_metrics());
        assert!(config.error_tracking());
        assert!(config.additional_metrics());
        assert!(!config.debug());
    }

    #[test]
    fn env_override_disables_submit_metrics() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // SAFETY: `ENV_LOCK` serializes every test in this module that
        // touches process env vars.
        unsafe {
            env::set_var("FASTSTATS_SUBMIT_METRICS", "false");
        }
        let config = Config::new(fixed_uuid()).with_env_overrides();
        assert!(!config.submit_metrics());
        // SAFETY: see the lock note above.
        unsafe {
            env::remove_var("FASTSTATS_SUBMIT_METRICS");
        }
    }

    #[test]
    fn env_override_is_case_insensitive() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // SAFETY: see the lock note above.
        unsafe {
            env::set_var("FASTSTATS_ERROR_TRACKING", "FALSE");
        }
        let config = Config::new(fixed_uuid()).with_env_overrides();
        assert!(!config.error_tracking());
        // SAFETY: see the lock note above.
        unsafe {
            env::remove_var("FASTSTATS_ERROR_TRACKING");
        }
    }

    #[test]
    fn unset_env_var_leaves_default() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // SAFETY: see the lock note above.
        unsafe {
            env::remove_var("FASTSTATS_ADDITIONAL_METRICS");
        }
        let config = Config::new(fixed_uuid()).with_env_overrides();
        assert!(config.additional_metrics());
    }

    #[test]
    fn invalid_env_value_leaves_default() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // SAFETY: see the lock note above.
        unsafe {
            env::set_var("FASTSTATS_DEBUG", "not-a-bool");
        }
        let config = Config::new(fixed_uuid()).with_env_overrides();
        assert!(config.debug());
        // SAFETY: see the lock note above.
        unsafe {
            env::remove_var("FASTSTATS_DEBUG");
        }
    }

    #[test]
    fn and_keeps_a_toggle_disabled_if_either_side_disables_it() {
        let developer_config = Config::new(fixed_uuid()).set_submit_metrics(true);
        let file_config = Config::new(fixed_uuid()).set_submit_metrics(false);
        let merged = developer_config.and(&file_config);
        assert!(!merged.submit_metrics());
    }

    #[test]
    fn and_keeps_a_toggle_enabled_only_if_both_sides_enable_it() {
        let a = Config::new(fixed_uuid()).set_debug(true);
        let b = Config::new(fixed_uuid()).set_debug(true);
        assert!(a.and(&b).debug());
    }

    #[test]
    fn and_takes_server_id_from_self() {
        let self_id = fixed_uuid();
        let other_id = Uuid::parse_str("00000000-0000-0000-0000-000000000002").expect("valid uuid");
        let merged = Config::new(self_id).and(&Config::new(other_id));
        assert_eq!(merged.server_id(), self_id);
    }

    #[test]
    fn persisted_string_round_trips() {
        let original = Config::new(fixed_uuid())
            .set_submit_metrics(false)
            .set_debug(false);
        let persisted = original.to_persisted_string();
        let parsed = Config::from_persisted_str(&persisted, Uuid::nil());
        assert_eq!(parsed, original);
    }

    #[test]
    fn from_persisted_str_defaults_missing_fields() {
        let parsed = Config::from_persisted_str("enabled=false\n", fixed_uuid());
        assert_eq!(parsed.server_id(), fixed_uuid());
        assert!(!parsed.enabled());
        assert!(parsed.submit_metrics());
    }

    #[test]
    fn from_persisted_str_ignores_unparseable_lines() {
        let parsed = Config::from_persisted_str("debug=not-a-bool\nenabled=false", fixed_uuid());
        assert!(parsed.debug());
        assert!(!parsed.enabled());
    }
}