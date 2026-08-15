//! Shared helpers for the `tests/*_e2e.rs` suites.
//!
//! Each file directly under `tests/` compiles as its own separate
//! crate (its own tests binary), so there's no single shared compiled
//! crate these could all depend on. Living under `tests/common/`
//! (rather than directly under `tests/`) keeps Cargo from treating
//! this file as a tests binary of its own; each suite instead pulls it
//! in with a plain `mod common;` declaration.

use std::env;
use std::fs::remove_dir_all;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use faststats_rs::{Attributes, Client, ClientBuilder, Id, Token};
use uuid::Uuid;

/// Serializes every e2e tests that touches process-global env vars.
pub static ENV_LOCK: Mutex<()> = Mutex::new(());

/// A valid 32-char lowercase-alphanumeric tests token.
pub fn test_token() -> Token {
    Token::new("a".repeat(32)).expect("valid token")
}

/// The project name for e2e tests, standing in for the host
/// plugin/mod's own name.
/// Kept distinct from [`test_sdk_info`]'s name so payload-shape
/// assertions can catch the two being conflated.
pub fn test_project_name() -> &'static str {
    "e2e-tests-project"
}

pub fn test_version() -> &'static str {
    "0.0.0"
}

/// A validated [`Id`] for metrics/feature-flag ids in tests.
pub fn test_id(value: &str) -> Id {
    Id::new(value).expect("valid id")
}

/// A fresh, unique `FASTSTATS_STATE_DIR` path, so tests don't share
/// persisted state.
pub fn fresh_state_dir() -> PathBuf {
    env::temp_dir().join(format!("faststats-e2e-{}", Uuid::new_v4()))
}

/// Sets `FASTSTATS_STATE_DIR` to a fresh directory and returns it.
/// Callers are responsible for removing it (and the env var) at the
/// end of their tests, e.g. via [`cleanup_state_dir`].
pub fn set_fresh_state_dir() -> PathBuf {
    let dir = fresh_state_dir();
    // SAFETY: callers are required to serialize env-mutating tests via
    // `ENV_LOCK`, so no other thread observes/mutates the environment
    // concurrently with this call.
    unsafe { env::set_var("FASTSTATS_STATE_DIR", &dir) };
    dir
}

/// Removes the `FASTSTATS_STATE_DIR` env var and deletes the directory.
pub fn cleanup_state_dir(dir: &Path) {
    // SAFETY: see `set_fresh_state_dir`.
    unsafe { env::remove_var("FASTSTATS_STATE_DIR") };
    let _ = remove_dir_all(dir);
}

/// Points every FastStats service's server env var at `base_url`
/// (a running [`wiremock::MockServer`]'s `.uri()`), and sets
/// `FASTSTATS_ENABLED=true` to skip the first-boot gate for this run.
pub fn point_all_services_at(base_url: &str) {
    // SAFETY: see `set_fresh_state_dir`.
    unsafe {
        env::set_var("FASTSTATS_METRICS_SERVER", base_url);
        env::set_var("FASTSTATS_ERROR_TRACKER_SERVER", base_url);
        env::set_var("FASTSTATS_FLAGS_SERVER", base_url);
        env::set_var("FASTSTATS_ENABLED", "true");
    }
}

/// Removes every env var [`point_all_services_at`] set.
pub fn unset_all_service_env() {
    // SAFETY: see `set_fresh_state_dir`.
    unsafe {
        env::remove_var("FASTSTATS_METRICS_SERVER");
        env::remove_var("FASTSTATS_ERROR_TRACKER_SERVER");
        env::remove_var("FASTSTATS_FLAGS_SERVER");
        env::remove_var("FASTSTATS_ENABLED");
    }
}

/// Builds a not-yet-started [`Client`] pointed at `base_url`, with a
/// fresh state dir, past the first-boot gate. Returns the client and
/// the state dir path for later cleanup. `configure` is applied to the
/// [`ClientBuilder`] before `.build()`.
pub fn build_client_against(
    base_url: &str,
    configure: impl FnOnce(ClientBuilder) -> ClientBuilder,
) -> (Client, PathBuf) {
    let dir = set_fresh_state_dir();
    point_all_services_at(base_url);

    let builder = ClientBuilder::new(test_project_name(), test_version(), test_token()).expect("valid client");
    let client = configure(builder).build().expect("client builds");

    (client, dir)
}

/// Tears down everything [`build_client_against`] set up.
pub fn teardown(dir: &Path) {
    unset_all_service_env();
    cleanup_state_dir(dir);
}

/// A single-key [`Attributes`] convenience builder for request-shape
/// assertions.
pub fn attrs_with(key: &str, value: &str) -> Attributes {
    let mut attrs = Attributes::empty();
    attrs.put(key, value).expect("valid attribute");
    attrs
}