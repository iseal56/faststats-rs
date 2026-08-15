//! A validated source identifier, shared by metrics ids and feature flag
//! keys.

use std::fmt;
use std::sync::LazyLock;
use regex::Regex;
use crate::error::{Error, Result};

pub const PATTERN_STR: &str = "^[a-z0-9_]+$";
pub static PATTERN: LazyLock<Regex> = LazyLock::new(|| Regex::new(PATTERN_STR).expect("valid regex"));

/// Returns whether `value` matches the pattern.
fn matches_pattern(value: &str) -> bool {
    PATTERN.is_match(value)
}

/// A validated source identifier (metrics id or feature flag key).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Id(String);

impl Id {
    /// Validates and wraps an id string.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if matches_pattern(&value) {
            Ok(Id(value))
        } else {
            Err(Error::validation("id", "must match pattern [a-z0-9_]+"))
        }
    }

    /// Borrows the id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Id({:?})", self.0)
    }
}

impl TryFrom<String> for Id {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Id::new(value)
    }
}

impl TryFrom<&str> for Id {
    type Error = Error;

    fn try_from(value: &str) -> Result<Self> {
        Id::new(value)
    }
}

impl serde::Serialize for Id {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_lowercase_letters() {
        assert!(Id::new("core_count").is_ok());
    }

    #[test]
    fn accepts_single_letter() {
        assert!(Id::new("a").is_ok());
    }

    #[test]
    fn rejects_empty() {
        assert!(Id::new("").is_err());
    }

    #[test]
    fn accepts_digits() {
        assert!(Id::new("metric1").is_ok());
    }

    #[test]
    fn rejects_uppercase() {
        assert!(Id::new("Metric").is_err());
    }

    #[test]
    fn rejects_hyphen() {
        assert!(Id::new("core-count").is_err());
    }

    #[test]
    fn display_shows_raw_value() {
        let id = Id::new("cpu_count").expect("valid id");
        assert_eq!(format!("{id}"), "cpu_count");
    }
}
