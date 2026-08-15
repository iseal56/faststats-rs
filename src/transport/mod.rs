//! Shared low-level submission transport used by every FastStats
//! service (metrics, error tracking, feature flags).

use std::env;
use std::io::Write;
use std::time::Duration;

use flate2::Compression;
use flate2::write::GzEncoder;
use reqwest::{Client, StatusCode, Url};
use serde_json::Value;

use crate::domain::sdk_info::SdkInfo;
use crate::error::{Error, Result};
use crate::validated::token::Token;

/// The fixed request timeout used for every FastStats HTTP request.
pub const TIMEOUT: Duration = Duration::from_secs(3);

/// The outcome of a submission attempt that successfully reached the
/// server (vs. a transport-level failure captured as an [`Error`]).
#[derive(Debug, Clone)]
pub struct SubmissionOutcome {
    /// The HTTP status code returned by the server.
    pub status: StatusCode,
    /// The raw response body, if it could be read as UTF-8.
    pub body: Option<String>,
}

impl SubmissionOutcome {
    /// Whether the response indicates the submission was accepted.
    pub fn is_successful(&self) -> bool {
        self.status.is_success()
    }

    /// Whether the response body looks like it contains a top-level
    /// `"warnings"` field.
    pub fn has_warnings(&self) -> bool {
        let Some(body) = &self.body else {
            return false;
        };
        match serde_json::from_str::<Value>(body) {
            Ok(Value::Object(map)) => map.contains_key("warnings"),
            _ => false,
        }
    }
}

/// The shared, low-level submission client used by every FastStats
/// service.
#[derive(Debug, Clone)]
pub struct Transport {
    client: Client,
    token: Token,
    sdk_info: SdkInfo,
}

impl Transport {
    /// Constructs a new transport for the given token and SDK info.
    pub fn new(token: Token, sdk_info: SdkInfo) -> Result<Self> {
        let client = Client::builder()
            .connect_timeout(TIMEOUT)
            .timeout(TIMEOUT)
            .build()?;
        Ok(Transport {
            client,
            token,
            sdk_info,
        })
    }

    /// Submits `data` to `url`, gzip-compressing the body.
    pub async fn submit(
        &self,
        url: &Url,
        data: &Value,
        submission_name: &str,
    ) -> Result<SubmissionOutcome> {
        let serialized = serde_json::to_string(data)?;
        let compressed = compress_gzip(serialized.as_bytes())?;

        let request = self
            .client
            .post(url.clone())
            .timeout(TIMEOUT)
            .header("Authorization", format!("Bearer {}", self.token.as_str()))
            .header("User-Agent", self.sdk_info.user_agent())
            .header("Content-Encoding", "gzip")
            .header("Content-Type", "application/octet-stream")
            .body(compressed);

        log::debug!("Sending {submission_name} to: {url}");

        let response = request.send().await?;
        let status = response.status();
        let body = response.text().await.ok();

        let outcome = SubmissionOutcome { status, body };
        log_outcome(&outcome, submission_name);
        Ok(outcome)
    }

    /// Submits `data` to `url` as uncompressed JSON
    pub async fn submit_json(
        &self,
        url: &Url,
        data: &Value,
        submission_name: &str,
    ) -> Result<SubmissionOutcome> {
        let serialized = serde_json::to_string(data)?;

        let request = self
            .client
            .post(url.clone())
            .timeout(TIMEOUT)
            .header("Authorization", format!("Bearer {}", self.token.as_str()))
            .header("Content-Type", "application/json")
            .header("User-Agent", self.sdk_info.user_agent())
            .body(serialized);

        log::debug!("Sending {submission_name} to: {url}");

        let response = request.send().await?;
        let status = response.status();
        let body = response.text().await.ok();

        let outcome = SubmissionOutcome { status, body };
        log_outcome(&outcome, submission_name);
        Ok(outcome)
    }
}

/// Gzip-compresses `bytes`.
fn compress_gzip(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes)?;
    let compressed = encoder.finish()?;
    Ok(compressed)
}

/// Logs the outcome of a submission at the appropriate level.
fn log_outcome(outcome: &SubmissionOutcome, submission_name: &str) {
    let status = outcome.status.as_u16();
    let body = outcome.body.as_deref().unwrap_or_default();

    if outcome.is_successful() {
        if outcome.has_warnings() {
            log::warn!(
                "{} submitted successfully with status code: {status} ({body})",
                capitalize(submission_name)
            );
        } else {
            log::debug!(
                "{} submitted successfully with status code: {status} ({body})",
                capitalize(submission_name)
            );
        }
        return;
    }

    if (300..400).contains(&status) {
        log::warn!("Received redirect response from {submission_name} server: {status} ({body})");
    } else if (400..500).contains(&status) {
        log::error!("Submitted invalid request to {submission_name} server: {status} ({body})");
    } else if (500..600).contains(&status) {
        log::error!(
            "Received server error response from {submission_name} server: {status} ({body})"
        );
    } else {
        log::warn!("Received unexpected response from {submission_name} server: {status} ({body})");
    }
}

/// Resolves a server URL, allowing an environment-variable override.
pub fn resolve_server_url(env_var: &'static str, default_url: &str) -> Result<Url> {
    if let Ok(value) = env::var(env_var) {
        match Url::parse(&value) {
            Ok(url) => return Ok(url),
            Err(e) => {
                log::error!("Failed to parse server url from {env_var}: {value} ({e})");
            }
        }
    }
    Url::parse(default_url).map_err(|e| Error::InvalidServerUrl {
        env_var,
        reason: e.to_string(),
    })
}

/// Capitalizes the first character of `value`.
fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::GzDecoder;
    use std::env;
    use std::io::Read;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn resolve_server_url_uses_default_when_unset() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: `ENV_LOCK` serializes every test in this module that
        // touches process env vars.
        unsafe {
            env::remove_var("FASTSTATS_TEST_SERVER");
        }
        let url = resolve_server_url(
            "FASTSTATS_TEST_SERVER",
            "https://metrics.faststats.dev/v1/collect",
        )
        .expect("default url parses");
        assert_eq!(url.as_str(), "https://metrics.faststats.dev/v1/collect");
    }

    #[test]
    fn resolve_server_url_uses_valid_override() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: see the lock note above.
        unsafe {
            env::set_var("FASTSTATS_TEST_SERVER", "https://example.com/custom");
        }
        let url = resolve_server_url(
            "FASTSTATS_TEST_SERVER",
            "https://metrics.faststats.dev/v1/collect",
        )
        .expect("override url parses");
        assert_eq!(url.as_str(), "https://example.com/custom");
        // SAFETY: see the lock note above.
        unsafe {
            env::remove_var("FASTSTATS_TEST_SERVER");
        }
    }

    #[test]
    fn resolve_server_url_falls_back_on_invalid_override() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: see the lock note above.
        unsafe {
            env::set_var("FASTSTATS_TEST_SERVER", "not a url");
        }
        let url = resolve_server_url(
            "FASTSTATS_TEST_SERVER",
            "https://metrics.faststats.dev/v1/collect",
        )
        .expect("falls back to default");
        assert_eq!(url.as_str(), "https://metrics.faststats.dev/v1/collect");
        // SAFETY: see the lock note above.
        unsafe {
            env::remove_var("FASTSTATS_TEST_SERVER");
        }
    }

    #[test]
    fn capitalize_uppercases_first_char_only() {
        assert_eq!(capitalize("metrics"), "Metrics");
        assert_eq!(capitalize("errors"), "Errors");
    }

    #[test]
    fn capitalize_handles_empty_string() {
        assert_eq!(capitalize(""), "");
    }

    #[test]
    fn capitalize_handles_already_capitalized() {
        assert_eq!(capitalize("Metrics"), "Metrics");
    }

    #[test]
    fn gzip_round_trips_via_flate2_decoder() {
        let original = b"{\"hello\":\"world\"}";
        let compressed = compress_gzip(original).expect("compression succeeds");

        assert_eq!(&compressed[0..2], &[0x1f, 0x8b]);

        let mut decoder = GzDecoder::new(&compressed[..]);
        let mut decompressed = Vec::new();
        decoder
            .read_to_end(&mut decompressed)
            .expect("decompression succeeds");
        assert_eq!(decompressed, original);
    }

    #[test]
    fn submission_outcome_is_successful_for_2xx() {
        let outcome = SubmissionOutcome {
            status: StatusCode::OK,
            body: None,
        };
        assert!(outcome.is_successful());
    }

    #[test]
    fn submission_outcome_is_not_successful_for_4xx() {
        let outcome = SubmissionOutcome {
            status: StatusCode::BAD_REQUEST,
            body: None,
        };
        assert!(!outcome.is_successful());
    }

    #[test]
    fn has_warnings_detects_top_level_field() {
        let outcome = SubmissionOutcome {
            status: StatusCode::OK,
            body: Some(r#"{"warnings":["slow down"]}"#.to_string()),
        };
        assert!(outcome.has_warnings());
    }

    #[test]
    fn has_warnings_false_when_absent() {
        let outcome = SubmissionOutcome {
            status: StatusCode::OK,
            body: Some(r#"{"ok":true}"#.to_string()),
        };
        assert!(!outcome.has_warnings());
    }

    #[test]
    fn has_warnings_false_for_non_json_body() {
        let outcome = SubmissionOutcome {
            status: StatusCode::OK,
            body: Some("not json".to_string()),
        };
        assert!(!outcome.has_warnings());
    }

    #[test]
    fn has_warnings_false_for_missing_body() {
        let outcome = SubmissionOutcome {
            status: StatusCode::OK,
            body: None,
        };
        assert!(!outcome.has_warnings());
    }
}
