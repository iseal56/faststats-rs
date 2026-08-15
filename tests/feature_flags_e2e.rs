//! End-to-end tests for the feature flags service, driving
//! [`ClientBuilder`]/[`Client`]/[`faststats_rs::FeatureFlags`] against a
//! mock HTTP server. Feature flag requests are plain uncompressed
//! JSON (no gzip), so bodies are read directly rather than
//! gzip-decoded.

mod common;

use std::time::Duration;

use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use common::*;
use faststats_rs::{Config, FeatureFlag};

fn parse_request_body(request: &Request) -> Value {
    serde_json::from_slice(&request.body).expect("valid JSON body")
}

#[tokio::test]
async fn happy_path_fetch_caches_value_and_matches_request_shape() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/check"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"value": "on"})))
        .expect(1)
        .mount(&server)
        .await;

    let dir = set_fresh_state_dir();
    point_all_services_at(&server.uri());

    let flag = FeatureFlag::new(test_id("checkout_variant"), "off");
    let client =
        faststats_rs::ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("client gets created")
            .feature_flags_factory(|f| f.add_flag(flag))
            .expect("factory configured")
            .build()
            .expect("client builds");

    let flags = client
        .feature_flags()
        .expect("feature flags enabled by default");
    let value = flags
        .when_ready(&test_id("checkout_variant"))
        .await
        .expect("fetch succeeds");
    assert_eq!(value, faststats_rs::FlagValue::from("on"));

    let requests = server.received_requests().await.expect("requests recorded");
    assert_eq!(requests.len(), 1);
    let body = parse_request_body(&requests[0]);
    assert_eq!(body["key"], "checkout_variant");
    assert!(body["identifier"].is_string());

    // second call within the TTL should be served from cache: no new request.
    let cached = flags.cached(&test_id("checkout_variant"));
    assert_eq!(cached, Some(faststats_rs::FlagValue::from("on")));
    let value_again = flags
        .when_ready(&test_id("checkout_variant"))
        .await
        .expect("cached hit");
    assert_eq!(value_again, faststats_rs::FlagValue::from("on"));
    let requests_after = server.received_requests().await.expect("requests recorded");
    assert_eq!(requests_after.len(), 1);

    teardown(&dir);
}

#[tokio::test]
async fn ttl_expiry_triggers_a_fresh_fetch() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/check"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"value": "on"})))
        .expect(2)
        .mount(&server)
        .await;

    let dir = set_fresh_state_dir();
    point_all_services_at(&server.uri());

    let flag = FeatureFlag::new(test_id("short_ttl_flag"), "off");
    let client =
        faststats_rs::ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("client gets created")
            .feature_flags_factory(|f| f.add_flag(flag).map(|f| f.ttl(Duration::from_millis(10))))
            .expect("factory configured")
            .build()
            .expect("client builds");

    let flags = client
        .feature_flags()
        .expect("feature flags enabled by default");
    flags
        .when_ready(&test_id("short_ttl_flag"))
        .await
        .expect("first fetch");

    tokio::time::sleep(Duration::from_millis(50)).await;

    // cached() must report expiry, and when_ready must re-fetch.
    assert_eq!(flags.cached(&test_id("short_ttl_flag")), None);
    flags
        .when_ready(&test_id("short_ttl_flag"))
        .await
        .expect("second fetch after expiry");

    let requests = server.received_requests().await.expect("requests recorded");
    assert_eq!(requests.len(), 2);

    teardown(&dir);
}

#[tokio::test]
async fn opt_in_posts_then_fetches_and_updates_cache() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/opt-in"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/check"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"value": true})))
        .expect(1)
        .mount(&server)
        .await;

    let dir = set_fresh_state_dir();
    point_all_services_at(&server.uri());

    let flag = FeatureFlag::new(test_id("beta_feature"), false);
    let client =
        faststats_rs::ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("client gets created")
            .feature_flags_factory(|f| f.add_flag(flag))
            .expect("factory configured")
            .build()
            .expect("client builds");

    let flags = client
        .feature_flags()
        .expect("feature flags enabled by default");
    let value = flags
        .opt_in(&test_id("beta_feature"))
        .await
        .expect("opt-in succeeds");
    assert_eq!(value, faststats_rs::FlagValue::from(true));
    assert_eq!(
        flags.cached(&test_id("beta_feature")),
        Some(faststats_rs::FlagValue::from(true))
    );

    let opt_in_requests = server
        .received_requests()
        .await
        .expect("requests recorded")
        .into_iter()
        .filter(|r| r.url.path() == "/v1/opt-in")
        .count();
    assert_eq!(opt_in_requests, 1);

    teardown(&dir);
}

#[tokio::test]
async fn opt_out_posts_then_fetches_and_updates_cache() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/opt-out"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/check"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"value": false})))
        .expect(1)
        .mount(&server)
        .await;

    let dir = set_fresh_state_dir();
    point_all_services_at(&server.uri());

    let flag = FeatureFlag::new(test_id("beta_feature_two"), true);
    let client =
        faststats_rs::ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("client gets created")
            .feature_flags_factory(|f| f.add_flag(flag))
            .expect("factory configured")
            .build()
            .expect("client builds");

    let flags = client
        .feature_flags()
        .expect("feature flags enabled by default");
    let value = flags
        .opt_out(&test_id("beta_feature_two"))
        .await
        .expect("opt-out succeeds");
    assert_eq!(value, faststats_rs::FlagValue::from(false));

    teardown(&dir);
}

#[tokio::test]
async fn concurrent_fetches_for_same_id_share_exactly_one_network_request() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/check"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"value": "leader-result"}))
                .set_delay(Duration::from_millis(50)),
        )
        .expect(1)
        .mount(&server)
        .await;

    let dir = set_fresh_state_dir();
    point_all_services_at(&server.uri());

    let flag = FeatureFlag::new(test_id("contended_flag"), "off");
    let client =
        faststats_rs::ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("client gets created")
            .feature_flags_factory(|f| f.add_flag(flag))
            .expect("factory configured")
            .build()
            .expect("client builds");

    let flags = std::sync::Arc::clone(
        client
            .feature_flags()
            .expect("feature flags enabled by default"),
    );
    let flags_a = std::sync::Arc::clone(&flags);
    let flags_b = std::sync::Arc::clone(&flags);

    let (result_a, result_b) = tokio::join!(
        tokio::spawn(async move { flags_a.fetch(&test_id("contended_flag")).await }),
        tokio::spawn(async move { flags_b.fetch(&test_id("contended_flag")).await }),
    );

    assert_eq!(
        result_a.expect("task a completes").expect("fetch ok"),
        faststats_rs::FlagValue::from("leader-result")
    );
    assert_eq!(
        result_b.expect("task b completes").expect("fetch ok"),
        faststats_rs::FlagValue::from("leader-result")
    );

    let requests = server.received_requests().await.expect("requests recorded");
    assert_eq!(requests.len(), 1);

    teardown(&dir);
}

#[tokio::test]
async fn failure_path_non_2xx_check_response_returns_err_without_panicking() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/check"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;

    let dir = set_fresh_state_dir();
    point_all_services_at(&server.uri());

    let flag = FeatureFlag::new(test_id("failing_flag"), "off");
    let client =
        faststats_rs::ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("client gets created")
            .feature_flags_factory(|f| f.add_flag(flag))
            .expect("factory configured")
            .build()
            .expect("client builds");

    let flags = client
        .feature_flags()
        .expect("feature flags enabled by default");
    let result = flags.fetch(&test_id("failing_flag")).await;
    assert!(result.is_err());
    // failed fetch never populates the cache.
    assert_eq!(flags.cached(&test_id("failing_flag")), None);

    teardown(&dir);
}

#[tokio::test]
async fn developer_disabled_feature_flags_service_is_never_constructed() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let server = MockServer::start().await;
    // No mock registered: any request would fail the tests.

    let (client, dir) = build_client_against(&server.uri(), |b| b.feature_flags(false));

    assert!(client.feature_flags().is_none());

    teardown(&dir);
}

#[tokio::test]
async fn config_disabled_makes_feature_flags_inactive_but_still_constructed() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let server = MockServer::start().await;

    let dir = set_fresh_state_dir();
    point_all_services_at(&server.uri());

    let config = Config::new(uuid::Uuid::nil()).set_enabled(false);
    let client =
        faststats_rs::ClientBuilder::new(test_project_name(), test_version(), test_token())
            .expect("client gets created")
            .config(config)
            .build()
            .expect("client builds");

    assert!(client.feature_flags().is_some());
    assert!(!client.feature_flags_active());

    teardown(&dir);
}
