//! Demonstrates the `terminal` cargo feature: when enabled, the metrics
//! payload automatically gains `terminal_columns`/`terminal_rows`/
//! `terminal_emulator` fields alongside the built-in platform metrics,
//! with zero extra registration needed on the developer's part.
//!
//! Run with:
//! ```sh
//! cargo run --example terminal_app --features terminal
//! ```

use faststats_rs::{Client, ClientBuilder, Config, Token};
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let token = Token::new("0123456789abcdef0123456789abcdef")?;
    // `project_name` identifies the host application
    let project_name = "faststats-terminal-example";

    let mut client: Client = ClientBuilder::new(project_name, env!("CARGO_PKG_VERSION"), token)
        .expect("valid client")
        .config(Config::from_env(Uuid::new_v4()))
        // No error tracking or feature flags needed for this example.
        .error_tracking(false)
        .feature_flags(false)
        .build()?;

    client.start();

    // With the `terminal` feature enabled, `snapshot()` already
    // includes terminal_columns/terminal_rows/terminal_emulator - no
    // extra registration required, the same way Java's platform
    // modules append their own fields into the default metrics object.
    if let Some(metrics) = client.metrics() {
        let snapshot = metrics.snapshot();
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
    }

    client.shutdown().await;
    Ok(())
}