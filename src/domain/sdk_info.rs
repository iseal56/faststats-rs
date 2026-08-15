//! Information that identifies the SDK implementation using FastStats.

use crate::error::{Error, Result};

/// Information that identifies the SDK implementation using FastStats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdkInfo {
    name: String,
    version: String,
    user_agent: String,
}

impl SdkInfo {
    /// Constructs [`SdkInfo`] from this SDK implementation's own
    /// `name`/`version` and an already-built `user_agent` string.
    /// Building the user agent is the caller's responsibility, since it
    /// typically needs context (like the host application's own name/
    /// version) that `SdkInfo` itself has no business knowing about.
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        user_agent: impl Into<String>,
    ) -> Result<Self> {
        let name = name.into();
        let version = version.into();
        let user_agent = user_agent.into();
        if name.trim().is_empty() {
            return Err(Error::validation("sdk_info.name", "must not be blank"));
        }
        if version.trim().is_empty() {
            return Err(Error::validation("sdk_info.version", "must not be blank"));
        }
        if user_agent.trim().is_empty() {
            return Err(Error::validation("sdk_info.user_agent", "must not be blank"));
        }
        Ok(SdkInfo {
            name,
            version,
            user_agent,
        })
    }

    /// Gets the SDK implementation name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Gets the SDK implementation version.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Gets the HTTP user agent sent with FastStats HTTP requests.
    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_the_supplied_user_agent_verbatim() {
        let info = SdkInfo::new("faststats-rs", "0.1.0", "FastStats Rust SDK v0.1.0 (my-app:1.4.2)")
            .expect("valid sdk info");
        assert_eq!(info.user_agent(), "FastStats Rust SDK v0.1.0 (my-app:1.4.2)");
    }

    #[test]
    fn rejects_blank_name() {
        assert!(SdkInfo::new("", "0.1.0", "ua").is_err());
        assert!(SdkInfo::new("   ", "0.1.0", "ua").is_err());
    }

    #[test]
    fn rejects_blank_version() {
        assert!(SdkInfo::new("faststats-rs", "", "ua").is_err());
        assert!(SdkInfo::new("faststats-rs", "   ", "ua").is_err());
    }

    #[test]
    fn rejects_blank_user_agent() {
        assert!(SdkInfo::new("faststats-rs", "0.1.0", "").is_err());
        assert!(SdkInfo::new("faststats-rs", "0.1.0", "   ").is_err());
    }
}