//! The metrics submission service: the `Metric` abstraction, built-in
//! platform metrics, and the `Factory`/`Metrics` submission pipeline.

pub mod metric;
pub mod platform;
pub mod service;

pub use metric::Metric;
pub use service::{Factory, Metrics, MetricsEvent};