//! [`FeatureFlag`]: a single flag's identity, default, and per-flag
//! attributes.
//!
//! TTL is a service-level setting, not per-flag; see
//! [`super::service`] for where it lives.

use crate::domain::Attributes;
use crate::validated::Id;

use super::value::FlagValue;

/// A single feature flag: its id, default value (used until/if a fetch
/// succeeds), and per-flag attributes for server-side targeting.
/// Cache state (value + last-fetch time) is owned by the
/// [`super::service::FeatureFlags`] service, not this type.
#[derive(Debug, Clone)]
pub struct FeatureFlag {
    id: Id,
    default: FlagValue,
    attributes: Option<Attributes>,
}

impl FeatureFlag {
    /// Creates a new flag with the given id and default value.
    pub fn new(id: Id, default: impl Into<FlagValue>) -> Self {
        FeatureFlag {
            id,
            default: default.into(),
            attributes: None,
        }
    }

    /// Builder-style setter for per-flag attributes, merged with
    /// service-level attributes on every request (per-flag wins on
    /// conflict).
    #[must_use]
    pub fn attributes(mut self, attributes: Attributes) -> Self {
        self.attributes = Some(attributes);
        self
    }

    /// The flag's id.
    pub fn id(&self) -> &Id {
        &self.id
    }

    /// The flag's default value.
    pub fn default(&self) -> &FlagValue {
        &self.default
    }

    /// The flag's per-flag attributes, if any.
    pub fn attributes_ref(&self) -> Option<&Attributes> {
        self.attributes.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_id() -> Id {
        Id::new("test_flag").expect("valid id")
    }

    #[test]
    fn new_flag_stores_id_and_default() {
        let flag = FeatureFlag::new(test_id(), "default-value");
        assert_eq!(flag.id(), &test_id());
        assert_eq!(flag.default(), &FlagValue::from("default-value"));
    }

    #[test]
    fn attributes_builder_sets_per_flag_attributes() {
        let mut attrs = Attributes::empty();
        attrs.put("cohort", "beta").expect("valid attribute");
        let flag = FeatureFlag::new(test_id(), 1.0_f64).attributes(attrs);
        assert!(flag.attributes_ref().is_some());
        assert_eq!(
            flag.attributes_ref().unwrap().get("cohort"),
            Some(&serde_json::Value::String("beta".to_string()))
        );
    }

    #[test]
    fn flag_with_no_attributes_returns_none() {
        let flag = FeatureFlag::new(test_id(), true);
        assert!(flag.attributes_ref().is_none());
    }
}
