//! A plain CLI example wiring up all three FastStats services: metrics
//! (with a custom metric and an `on_flush` callback), error tracking
//! (with an ignore rule, a custom anonymization pattern, and one
//! manually tracked error), and a feature flag (read via `when_ready`).
//!
//! Run with:
//! ```sh
//! cargo run --example basic
//! ```
//!
//! This won't actually reach a real FastStats server (there isn't a
//! demo token to use here), the point is to show the shape of the API:
//! build a `Client`, register things, `start()`, do some work, then
//! `shutdown()` for a final flush.

use faststats_rs::{Attributes, Client, ClientBuilder, Config, FeatureFlag, Metric, Token};
use regex::Regex;
use serde::Serialize;
use uuid::Uuid;

/// An example custom metric value - any `Serialize` type works,
/// including nested structs, not just primitives.
#[derive(Serialize)]
struct ServerInfo {
    region: &'static str,
    shard_count: u32,
    beta: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger_init();

    // A real token is a lowercase-alphanumeric 32-char string issued by
    // FastStats; this placeholder is only shape-valid for the example.
    let token = Token::new("0123456789abcdef0123456789abcdef")?;
    // `project_name` identifies the host program
    let project_name = "faststats-basic-example";

    let mut client: Client = ClientBuilder::new(project_name, env!("CARGO_PKG_VERSION"), token)
        .expect("valid client")
        // Config toggles are end-user-controlled and always win over the
        // developer toggles below when they say false
        .config(Config::from_env(Uuid::new_v4()))
        .metrics_factory(|factory| {
            let server_info = Metric::new("server_info", || ServerInfo {
                region: "eu-west-1",
                shard_count: 4,
                beta: false,
            })?;
            Ok(factory
                .add_metric(server_info)?
                .on_flush(|| println!("metrics flushed successfully")))
        })?
        .error_tracking_factory(|factory| {
            factory
                // Exact-type ignore rule: never report cancellation.
                .ignore_error_type("OperationCancelled")
                // Extra anonymization pattern on top of the built-in
                // ones (IPs, home paths, etc.).
                .anonymize(Regex::new(r"sk-[A-Za-z0-9]{20,}").unwrap(), "[api_key]")
        })
        .feature_flags_factory(|factory| {
            Ok(factory.add_flag(FeatureFlag::new("new_ui".try_into()?, false))?)
        })?
        .build()?;

    client.start();

    if client.is_first_boot() {
        println!("First run so nothing has been submitted yet, see the log output above.");
    }

    // Manually track a handled error, with some context attached.
    if let Some(error_tracking) = client.error_tracking() {
        let mut context = Attributes::empty();
        context.put("step", "startup")?;
        error_tracking.track_error(
            "ConfigWarning",
            Some("using default config"),
            &[],
            Some(context),
        );
    }

    // Read a feature flag (falls back to its default until a fetch
    // succeeds, or immediately if `feature_flags_active()` is false).
    if let Some(feature_flags) = client.feature_flags() {
        if client.feature_flags_active() {
            let new_ui = feature_flags.when_ready(&"new_ui".try_into()?).await?;
            println!("new_ui flag: {new_ui:?}");
        }
    }

    // ... application logic would run here ...

    client.shutdown().await;
    Ok(())
}

fn env_logger_init() {
    // Not a real dependency of this example; swap in whatever logger
    // backend your application already uses (the `log` facade is what
    // faststats itself logs through).
    let _ = std::env::var("RUST_LOG");
}
