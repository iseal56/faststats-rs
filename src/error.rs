//! Crate-level error type. Every fallible operation returns [`Result`].

use std::{io, result};

/// The crate-level result alias.
pub type Result<T> = result::Result<T, Error>;

/// Errors that can occur anywhere in the `faststats-rs` crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("error in initialization: {0}")]
    Initialization(String),
    
    #[error("invalid {kind}: {reason}")]
    Validation { kind: &'static str, reason: String },

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("compression error: {0}")]
    Compression(#[from] io::Error),

    #[error("invalid server url in {env_var}: {reason}")]
    InvalidServerUrl {
        env_var: &'static str,
        reason: String,
    },
}

impl Error {
    /// Builds a [`Error::Validation`] error.
    pub(crate) fn validation(kind: &'static str, reason: impl Into<String>) -> Self {
        Error::Validation {
            kind,
            reason: reason.into(),
        }
    }
}