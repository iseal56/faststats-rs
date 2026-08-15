//! `faststats-rs` - Rust SDK for FastStats: metrics, error tracking, and
//! feature flags. No function reachable from the public API panics;
//! every fallible operation returns [`error::Result`].

pub mod client;
pub mod domain;
pub mod error;
pub mod error_tracking;
pub mod features;
pub mod metrics;
pub mod transport;
pub mod validated;
pub mod feature_flags;

pub use client::{Client, ClientBuilder};
pub use domain::{Attributes, Config, SdkInfo};
pub use error::{Error, Result};
pub use error_tracking::{ErrorTracker, ErrorTrackerFactory, IgnoreRule, TrackedError};
pub use feature_flags::flag::FeatureFlag;
pub use feature_flags::service::{Factory as FeatureFlagsFactory, FeatureFlags};
pub use feature_flags::value::FlagValue;
pub use metrics::{Factory as MetricsFactory, Metric, Metrics, MetricsEvent};
pub use transport::{SubmissionOutcome, Transport};
pub use validated::{Id, Token};