//! [`CachedValue`]: a fetched flag value plus the instant it was
//! fetched. Validity is checked against the service-level TTL, not any
//! state stored here.

use std::time::{Duration, Instant};

use super::value::FlagValue;

#[derive(Debug, Clone)]
pub struct CachedValue {
    pub value: FlagValue,
    fetched_at: Instant,
}

impl CachedValue {
    pub fn new(value: FlagValue) -> Self {
        CachedValue {
            value,
            fetched_at: Instant::now(),
        }
    }

    pub fn is_valid(&self, ttl: Duration) -> bool {
        self.fetched_at.elapsed() < ttl
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_value_is_valid() {
        let cached = CachedValue::new(FlagValue::from("v"));
        assert!(cached.is_valid(Duration::from_secs(60)));
    }

    #[test]
    fn zero_ttl_is_immediately_invalid() {
        let cached = CachedValue::new(FlagValue::from("v"));
        assert!(!cached.is_valid(Duration::from_secs(0)));
    }
}