//! End-to-end tests for the error tracking service, driving
//! [`ClientBuilder`]/[`Client`]/[`faststats-rs::ErrorTracker`] against a
//! mock HTTP server. `ErrorTracker::create_data` is `pub(crate)`, so
//! payload-shape assertions here go through what the mock server
//! actually received (gzip-decompressed body).

mod common;

use flate2::read::GzDecoder;
use serde_json::Value;
use std::io::Read;
use std::panic::catch_unwind;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::*;

fn decode_gzip_json(bytes: &[u8]) -> Value {
    let mut decoder = GzDecoder::new(bytes);
    let mut decompressed = String::new();
    decoder
        .read_to_string(&mut decompressed)
        .expect("valid gzip body");
    serde_json::from_str(&decompressed).expect("valid JSON body")
}

#[tokio::test]
async fn happy_path_tracked_error_is_submitted_with_expected_shape() {
    let _guard = ENV_LOCK.lock().await;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/error"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let (client, dir) = build_client_against(&server.uri(), |b| b);

    let error_tracking = client
        .error_tracking()
        .expect("error tracking enabled by default");
    error_tracking.track_error("CustomError", Some("something broke"), &[], None);

    assert!(error_tracking.submit().await);

    let requests = server.received_requests().await.expect("requests recorded");
    assert_eq!(requests.len(), 1);
    let body = decode_gzip_json(&requests[0].body);

    assert_eq!(body["project_name"], "e2e-tests-project");
    assert_eq!(body["language"], "rust");
    assert_eq!(body["sdk_name"], "faststats-rs");
    let errors = body["errors"].as_array().expect("errors array present");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["error"], "CustomError");
    assert_eq!(errors[0]["message"], "something broke");
    assert_eq!(errors[0]["handled"], true);

    teardown(&dir);
}

#[tokio::test]
async fn deduplicated_errors_carry_a_literal_count_on_submission() {
    let _guard = ENV_LOCK.lock().await;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/error"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let (client, dir) = build_client_against(&server.uri(), |b| b);

    let error_tracking = client
        .error_tracking()
        .expect("error tracking enabled by default");
    error_tracking.track_error("E", Some("m"), &["f".to_string()], None);
    error_tracking.track_error("E", Some("m"), &["f".to_string()], None);
    error_tracking.track_error("E", Some("m"), &["f".to_string()], None);

    assert!(error_tracking.submit().await);

    let requests = server.received_requests().await.expect("requests recorded");
    let body = decode_gzip_json(&requests[0].body);
    let errors = body["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["count"], 3);

    teardown(&dir);
}

#[tokio::test]
async fn ignore_rule_prevents_matching_error_from_ever_being_submitted() {
    let _guard = ENV_LOCK.lock().await;

    let server = MockServer::start().await;
    // No mock registered on purpose: if a request were sent (it
    // shouldn't be, since submit() with nothing pending never hits
    // the network) wiremock fails the tests for an unmatched request.

    let dir = set_fresh_state_dir();
    point_all_services_at(&server.uri());

    let client =
        faststats_rs::ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid client")
            .error_tracking_factory(|f| f.ignore_error_type("NoisyError"))
            .build()
            .expect("client builds");

    let error_tracking = client
        .error_tracking()
        .expect("error tracking enabled by default");
    error_tracking.track_error("NoisyError", Some("ignored"), &[], None);

    assert!(error_tracking.is_empty());
    // nothing pending: submit() returns true without sending a request.
    assert!(error_tracking.submit().await);

    let requests = server.received_requests().await.expect("requests recorded");
    assert!(requests.is_empty());

    teardown(&dir);
}

#[tokio::test]
async fn anonymization_end_to_end_redacts_ip_and_home_path_before_submission() {
    let _guard = ENV_LOCK.lock().await;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/error"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let (client, dir) = build_client_against(&server.uri(), |b| b);

    let error_tracking = client
        .error_tracking()
        .expect("error tracking enabled by default");
    error_tracking.track_error(
        "IoError",
        Some("failed to reach 10.0.0.5"),
        &["at /home/tests/app.rs:10".to_string()],
        None,
    );

    assert!(error_tracking.submit().await);

    let requests = server.received_requests().await.expect("requests recorded");
    let body = decode_gzip_json(&requests[0].body);
    let errors = body["errors"].as_array().unwrap();
    assert_eq!(errors[0]["message"], "failed to reach [ipv4]");
    assert_eq!(errors[0]["stack"][0], "at /home/[home]/app.rs:10");

    teardown(&dir);
}

#[tokio::test]
async fn tracker_level_attributes_end_up_in_submitted_context() {
    let _guard = ENV_LOCK.lock().await;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/error"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let dir = set_fresh_state_dir();
    point_all_services_at(&server.uri());

    let client =
        faststats_rs::ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("valid client")
            .error_tracking_factory(|f| f.attributes(attrs_with("environment", "staging")))
            .build()
            .expect("client builds");

    let error_tracking = client
        .error_tracking()
        .expect("error tracking enabled by default");
    error_tracking.track_error("E", None, &[], None);
    assert!(error_tracking.submit().await);

    let requests = server.received_requests().await.expect("requests recorded");
    let body = decode_gzip_json(&requests[0].body);
    assert_eq!(body["context"]["environment"], "staging");

    teardown(&dir);
}

#[tokio::test]
async fn failure_path_non_2xx_response_returns_false_without_panicking() {
    let _guard = ENV_LOCK.lock().await;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/error"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;

    let (client, dir) = build_client_against(&server.uri(), |b| b);

    let error_tracking = client
        .error_tracking()
        .expect("error tracking enabled by default");
    error_tracking.track_error("E", Some("boom"), &[], None);

    let submitted = error_tracking.submit().await;
    assert!(!submitted);

    teardown(&dir);
}

#[tokio::test]
async fn panic_hook_reports_unhandled_panic_end_to_end() {
    let _guard = ENV_LOCK.lock().await;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/error"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let (mut client, dir) = build_client_against(&server.uri(), |b| b.metrics(false));
    client.start();

    let error_tracking = std::sync::Arc::clone(
        client
            .error_tracking()
            .expect("error tracking enabled by default"),
    );

    // Run the panicking closure with panic=unwind semantics captured
    // via catch_unwind, so this tests process itself doesn't abort.
    let result = catch_unwind(|| {
        panic!("boom from e2e test");
    });
    assert!(result.is_err());

    tokio::task::yield_now().await;

    assert!(!error_tracking.is_empty());
    assert!(error_tracking.submit().await);

    let requests = server.received_requests().await.expect("requests recorded");
    let body = decode_gzip_json(&requests[0].body);
    let errors = body["errors"].as_array().unwrap();
    assert_eq!(errors[0]["error"], "panic");
    assert_eq!(errors[0]["handled"], false);
    assert_eq!(errors[0]["message"], "boom from e2e test");
    assert!(errors[0]["context"]["thread_name"].is_string());

    client.shutdown().await;
    teardown(&dir);
}
