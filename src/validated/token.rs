//! A validated FastStats project token.

use std::fmt;

use crate::error::{Error, Result};

/// The token pattern: exactly 32 lowercase alphanumeric characters.
pub const PATTERN: &str = "[a-z0-9]{32}";

/// Returns whether `value` matches `{PATTERN}`.
fn matches_pattern(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// A validated FastStats project token, matching `{PATTERN}`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Token(String);

impl Token {
    /// Validates and wraps a token string.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if matches_pattern(&value) {
            Ok(Token(value))
        } else {
            Err(Error::validation(
                "token",
                format!("must match pattern {PATTERN}"),
            ))
        }
    }

    /// Borrows the token as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Tokens shouldn't be freely available; never print the value itself.
        write!(f, "Token(***)")
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Token(***)")
    }
}

impl TryFrom<String> for Token {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Token::new(value)
    }
}

impl TryFrom<&str> for Token {
    type Error = Error;

    fn try_from(value: &str) -> Result<Self> {
        Token::new(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_token() {
        let value = "a".repeat(32);
        assert!(Token::new(value).is_ok());
    }

    #[test]
    fn accepts_mixed_alphanumeric_token() {
        let value = "abcd1234abcd1234abcd1234abcd1234";
        assert_eq!(value.len(), 32);
        assert!(Token::new(value).is_ok());
    }

    #[test]
    fn rejects_too_short() {
        let value = "a".repeat(31);
        assert!(Token::new(value).is_err());
    }

    #[test]
    fn rejects_too_long() {
        let value = "a".repeat(33);
        assert!(Token::new(value).is_err());
    }

    #[test]
    fn rejects_uppercase() {
        let value = "A".repeat(32);
        assert!(Token::new(value).is_err());
    }

    #[test]
    fn rejects_symbols() {
        let mut value = "a".repeat(31);
        value.push('-');
        assert!(Token::new(value).is_err());
    }

    #[test]
    fn debug_and_display_never_leak_token_value() {
        let value = "b".repeat(32);
        let token = Token::new(value).expect("valid token");
        assert_eq!(format!("{token}"), "Token(***)");
        assert_eq!(format!("{token:?}"), "Token(***)");
    }
}
