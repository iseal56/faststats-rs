# faststats

A Rust SDK for [FastStats](https://faststats.dev): anonymous metrics submission, error/panic tracking, and feature flags.

- Built-in metrics: native-platform analytics: `os`, `arch`, `pointer_width`, `cpu_count`, `debug_assertions`.
- Custom metrics and attributes accept any `Serialize` value, arbitrarily nested.
- Nothing is submitted on the very first run; the SDK only logs what would be sent and how to disable it. Submission begins on the next run.

## Install

```sh
cargo add faststats
```

Enable the `terminal` feature for TUI-specific metrics (terminal size,
detected terminal emulator):

```sh
cargo add faststats --features terminal
```

## Quick start

```rust
use faststats_rs::{Client, ClientBuilder, Config, SdkInfo, Token};
use uuid::Uuid;
use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let token = Token::new("your-32-char-lowercase-alphanumeric-token")?;
    let sdk_info = SdkInfo::new("my-app", env!("CARGO_PKG_VERSION"), None)?;

    let mut client: Client = ClientBuilder::new("my-project", token, sdk_info)
        .config(Config::from_env(Uuid::new_v4()))
        .build()?;

    client.start();
    // ... application logic ...
    client.shutdown().await;
    Ok(())
}
```

See [`examples/basic.rs`](examples/basic.rs) for a fuller walkthrough (custom metrics, `on_flush`, error tracking, feature flags) and [`examples/terminal_app.rs`](examples/terminal_app.rs) for the `terminal` feature.

## Metrics

Register a custom metric with any `Serialize` value: a struct, enum, map, or primitive, nested arbitrarily:

```rust
use faststats_rs::{Metric, ClientBuilder};
use faststats_rs::error::Result;
use serde::Serialize;

#[derive(Serialize)]
struct ServerInfo { region: &'static str, shard_count: u32 }

fn build(builder: ClientBuilder) -> Result<ClientBuilder> {
    builder.metrics_factory(|factory| {
        let metric = Metric::new("server_info", || ServerInfo { region: "us-east", shard_count: 4 })?;
        Ok(factory.add_metric(metric)?.on_flush(|| println!("flushed")))
    })
}
```

Metrics submit every 30 minutes, gated by `Config::submit_metrics`. 
Custom metrics are additionally gated by `Config::additional_metrics`. 
`on_flush` callbacks run after every successful submission.

## Error tracking

```rust
use faststats_rs::{ClientBuilder, error::Result};

async fn f(client: &Client) -> Result<()> {
    if let Some(tracker) = client.error_tracking() {
        tracker.track_error("ConfigWarning", Some("using default config"), &[], None);
    }
    Ok(()) 
}
```

Panics are captured automatically via `std::panic::set_hook` once `Client::start()` installs it, and reported with `handled = false`. 
Manually tracked errors default to `handled = true`. Identical errors (same type + message + stack) are deduplicated into a `count` instead of being sent as separate reports.

Register ignore rules and extra anonymization patterns on the builder:

```rust
use regex::Regex;
use faststats_rs::ClientBuilder;

fn build(builder: ClientBuilder) -> ClientBuilder {
    builder.error_tracking_factory(|factory| {
        factory
            .ignore_error_type("OperationCancelled")
            .anonymize(Regex::new(r"sk-[A-Za-z0-9]{20,}").unwrap(), "[api_key]")
    })
}
```

IPv4/IPv6 addresses, home-directory paths, Discord webhook URLs, and the OS username are anonymized by default, in addition to any patterns you register.

## Feature flags

```rust
use faststats_rs::{FeatureFlag, ClientBuilder, error::Result};

fn build(builder: ClientBuilder) -> Result<ClientBuilder> {
    builder.feature_flags_factory(|factory| {
        Ok(factory.add_flag(FeatureFlag::new("new_ui".try_into()?, false))?)
    })
}
```

```rust
use faststats_rs::{Client, error::Result};

async fn f(client: &Client) -> Result<()> {
    if client.feature_flags_active() {
        let value = client.feature_flags().unwrap().when_ready(&"new_ui".try_into()?).await?;
    }
    Ok(()) 
}
```

`when_ready` returns the cached value if it's still within its TTL (5 minutes by default), otherwise fetches. `fetch` always hits the network. `opt_in`/`opt_out` change targeting and re-fetch.

## Config

All toggles default to `true` and are independently overridable via environment variable although an end-user opt-out always wins over a developer enabling a service:

| Field                | Env var                        |
|----------------------|--------------------------------|
| `enabled`            | `FASTSTATS_ENABLED`            |
| `submit_metrics`     | `FASTSTATS_SUBMIT_METRICS`     |
| `error_tracking`     | `FASTSTATS_ERROR_TRACKING`     |
| `additional_metrics` | `FASTSTATS_ADDITIONAL_METRICS` |
| `debug`              | `FASTSTATS_DEBUG`              |

## Cargo features

| Feature    | Adds                                                                                   |
|------------|----------------------------------------------------------------------------------------|
| `terminal` | `terminal_columns`/`terminal_rows`/`terminal_emulator` metrics fields, via `crossterm` |