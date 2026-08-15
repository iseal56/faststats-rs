//! Ecosystem-specific metrics sets, gated behind cargo features,
//! expressed as feature-gated code within a single crate rather than
//! separate compilation units.

#[cfg(feature = "terminal")]
pub mod terminal;
