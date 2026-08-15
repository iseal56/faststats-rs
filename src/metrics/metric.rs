//! The `Metric` abstraction: a developer-registered, lazily-computed
//! piece of custom metrics data. Accepts any `Serialize` value.

use crate::domain::attributes::NonFiniteFloatCheck;
use crate::error::{Error, Result};
use crate::validated::Id;
use serde::Serialize;
use serde_json::Value;
use std::fmt::Debug;

/// A developer-registered custom metric: a validated id plus a
/// closure that computes its value on demand.
pub struct Metric {
    id: Id,
    compute: Box<dyn Fn() -> Result<Option<Value>> + Send + Sync>,
}

impl Metric {
    /// Creates a metric from an infallible closure returning `T`.
    /// `Metric::new("core_count", || 4)`.
    pub fn new<T, F>(id: impl TryInto<Id, Error = Error>, value: F) -> Result<Self>
    where
        T: Serialize,
        F: Fn() -> T + Send + Sync + 'static,
    {
        Self::try_new(id, move || Ok(Some(value())))
    }

    /// Creates a metric from a fallible closure returning
    /// `Result<Option<T>>`, for full control over absence/failure.
    pub fn try_new<T, F>(id: impl TryInto<Id, Error = Error>, compute: F) -> Result<Self>
    where
        T: Serialize,
        F: Fn() -> Result<Option<T>> + Send + Sync + 'static,
    {
        let id = id.try_into()?;
        let compute: Box<dyn Fn() -> Result<Option<Value>> + Send + Sync> =
            Box::new(move || match compute()? {
                None => Ok(None),
                Some(value) => {
                    value
                        .serialize(NonFiniteFloatCheck)
                        .map_err(|e| Error::validation("metric value", e.0))?;
                    let json = serde_json::to_value(value)?;
                    if json.is_null() {
                        Ok(None)
                    } else {
                        Ok(Some(json))
                    }
                }
            });
        Ok(Metric { id, compute })
    }

    /// The metric's validated id.
    pub fn id(&self) -> &Id {
        &self.id
    }

    /// Computes the metric's current value, or `Ok(None)` if there's
    /// nothing to report this round.
    pub fn compute(&self) -> Result<Option<Value>> {
        (self.compute)()
    }
}

impl Debug for Metric {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Metric")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn computes_scalar_value() {
        let metric = Metric::new("core_count", || 4).expect("valid metric");
        assert_eq!(metric.compute().unwrap(), Some(Value::from(4)));
    }

    #[test]
    fn computes_nested_struct_value() {
        #[derive(Serialize)]
        struct ServerInfo {
            region: String,
            shard_count: u32,
            beta: bool,
        }

        let metric = Metric::new("server_info", || ServerInfo {
            region: "eu-west".to_string(),
            shard_count: 3,
            beta: false,
        })
        .expect("valid metric");

        let value = metric.compute().unwrap().expect("present");
        assert_eq!(value["region"], "eu-west");
        assert_eq!(value["shard_count"], 3);
        assert_eq!(value["beta"], false);
    }

    #[test]
    fn computes_arbitrarily_nested_map_value() {
        let mut inner = HashMap::new();
        inner.insert("sub-key1".to_string(), "value2".to_string());
        let mut outer: HashMap<String, Value> = HashMap::new();
        outer.insert("key1".to_string(), Value::from("value1"));
        outer.insert("key2".to_string(), serde_json::to_value(inner).unwrap());

        let metric = Metric::new("nested", move || outer.clone()).expect("valid metric");
        let value = metric.compute().unwrap().expect("present");
        assert_eq!(value["key1"], "value1");
        assert_eq!(value["key2"]["sub-key1"], "value2");
    }

    #[test]
    fn rejects_invalid_id() {
        assert!(Metric::new("Invalid-Id", || 1).is_err());
    }

    #[test]
    fn try_new_absent_value_yields_none() {
        let metric: Metric = Metric::try_new("maybe", || Ok(None::<i32>)).expect("valid metric");
        assert_eq!(metric.compute().unwrap(), None);
    }

    #[test]
    fn try_new_propagates_compute_error() {
        let metric: Metric = Metric::try_new("failing", || {
            Err::<Option<i32>, _>(Error::validation("tests", "boom"))
        })
        .expect("valid metric");
        assert!(metric.compute().is_err());
    }

    #[test]
    fn rejects_non_finite_float() {
        let metric = Metric::new("bad_float", || f64::NAN).expect("valid metric");
        assert!(metric.compute().is_err());
    }

    #[test]
    fn none_returning_serialize_value_yields_none() {
        let metric: Metric =
            Metric::new("maybe_null", || Option::<i32>::None).expect("valid metric");
        assert_eq!(metric.compute().unwrap(), None);
    }
}
