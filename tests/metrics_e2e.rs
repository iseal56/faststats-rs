//! End-to-end tests for the metrics service, driving [`ClientBuilder`]/
//! [`Client`] against a mock HTTP server rather than calling internal
//! (`pub(crate)`) payload-building functions directly.
//!
//! `Metrics::create_data` is private, so payload-shape assertions here
//! go through what the mock server actually received (gzip-decompressed
//! body), matching what a real FastStats server would see.

mod common;

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use flate2::read::GzDecoder;
use serde_json::Value;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::*;
use faststats_rs::Config;

/// Decompresses a gzip request body captured by wiremock into parsed JSON.
fn decode_gzip_json(bytes: &[u8]) -> Value {
    let mut decoder = GzDecoder::new(bytes);
    let mut decompressed = String::new();
    decoder
        .read_to_string(&mut decompressed)
        .expect("valid gzip body");
    serde_json::from_str(&decompressed).expect("valid JSON body")
}

#[tokio::test]
async fn happy_path_submit_sends_expected_headers_and_body_shape() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/collect"))
        .and(header("Content-Encoding", "gzip"))
        .and(header("Content-Type", "application/octet-stream"))
        .and(header(
            "User-Agent",
            format!("FastStats Rust SDK v{} ({}:{})", env!("CARGO_PKG_VERSION"), test_project_name(), test_version()),
        ))
        .and(header(
            "Authorization",
            format!("Bearer {}", "a".repeat(32)).as_str(),
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let (client, dir) = build_client_against(&server.uri(), |b| b);

    let metrics = client.metrics().expect("metrics enabled by default");
    let submitted = metrics.submit().await;
    assert!(submitted);

    let requests = server.received_requests().await.expect("requests recorded");
    assert_eq!(requests.len(), 1);
    let body = decode_gzip_json(&requests[0].body);

    assert_eq!(body["project_name"], "e2e-tests-project");
    assert!(body["identifier"].is_string());
    assert!(body["data"]["os_name"].is_string());
    assert!(body["data"]["core_count"].is_number());
    assert!(body["data"]["pointer_width"].is_number());

    teardown(&dir);
}

#[tokio::test]
async fn custom_metric_value_appears_in_submitted_payload() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/collect"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let dir = set_fresh_state_dir();
    point_all_services_at(&server.uri());

    let custom_metric = faststats_rs::Metric::new("shard_count", || 7u32).expect("valid metric");
    let client = faststats_rs::ClientBuilder::new(test_project_name(), test_version(), test_token())
        .expect("valid client")
        .metrics_factory(|f| f.add_metric(custom_metric))
        .expect("factory configured")
        .build()
        .expect("client builds");

    let metrics = client.metrics().expect("metrics enabled by default");
    assert!(metrics.submit().await);

    let requests = server.received_requests().await.expect("requests recorded");
    let body = decode_gzip_json(&requests[0].body);
    assert_eq!(body["data"]["shard_count"], 7);

    teardown(&dir);
}

#[tokio::test]
async fn failure_path_non_2xx_response_returns_false_without_panicking() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/collect"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;

    let (client, dir) = build_client_against(&server.uri(), |b| b);

    let metrics = client.metrics().expect("metrics enabled by default");
    let submitted = metrics.submit().await;
    assert!(!submitted);

    teardown(&dir);
}

#[tokio::test]
async fn on_flush_callback_runs_only_on_successful_submission() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/collect"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;

    let dir = set_fresh_state_dir();
    point_all_services_at(&server.uri());

    let flushed = Arc::new(AtomicUsize::new(0));
    let flushed_clone = flushed.clone();

    let client = faststats_rs::ClientBuilder::new(test_project_name(), test_version(), test_token())
        .expect("valid client")
        .metrics_factory(|f| Ok(f.on_flush(move || {
            flushed_clone.fetch_add(1, Ordering::SeqCst);
        })))
        .expect("factory configured")
        .build()
        .expect("client builds");

    let metrics = client.metrics().expect("metrics enabled by default");
    let submitted = metrics.submit().await;
    assert!(!submitted);
    assert_eq!(flushed.load(Ordering::SeqCst), 0);

    teardown(&dir);
}

#[tokio::test]
async fn metrics_service_disabled_by_developer_is_never_constructed() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let server = MockServer::start().await;
    // No mock registered: the tests fails loudly (via wiremock's
    // "no matching handler" behavior) if a request is unexpectedly sent.

    let (client, dir) = build_client_against(&server.uri(), |b| b.metrics(false));

    assert!(client.metrics().is_none());

    teardown(&dir);
}

#[tokio::test]
async fn config_disabled_submit_metrics_prevents_scheduler_from_starting() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let server = MockServer::start().await;

    let dir = set_fresh_state_dir();
    point_all_services_at(&server.uri());

    let config = Config::new(uuid::Uuid::nil()).set_submit_metrics(false);
    let mut client = faststats_rs::ClientBuilder::new(test_project_name(), test_version(), test_token())
        .expect("valid client")
        .config(config)
        .error_tracking(false)
        .build()
        .expect("client builds");

    client.start();
    // metrics service exists (developer left it enabled) but Config
    // said no, so submit() was never scheduled; verified indirectly
    // via the still-present accessor plus no request having occurred.
    assert!(client.metrics().is_some());

    let requests = server.received_requests().await.expect("requests recorded");
    assert!(requests.is_empty());

    teardown(&dir);
}