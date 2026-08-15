//! Error tracking: `ErrorTracker`, panic hook integration, default and
//! custom anonymization, dedup-by-identity with count, ignore rules,
//! and the submission pipeline.

pub mod helper;
pub mod service;
pub mod tracker;

pub use service::{ErrorTracker, Factory as ErrorTrackerFactory};
pub use tracker::{IgnoreRule, TrackedError};