//! [`FlagValue`]: the three flag value types (`String`, `Number`,
//! `Boolean`). Round-trips through `serde_json::Value` for wire
//! (de)serialization.

use serde_json::Value;

/// A feature flag's value: one of `String`, `Number` (`f64`), or
/// `Boolean`.
#[derive(Debug, Clone, PartialEq)]
pub enum FlagValue {
    String(String),
    Number(f64),
    Boolean(bool),
}

impl FlagValue {
    /// Converts to the JSON representation sent/received on the wire.
    pub fn to_json(&self) -> Value {
        match self {
            FlagValue::String(s) => Value::from(s.clone()),
            FlagValue::Number(n) => serde_json::Number::from_f64(*n)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            FlagValue::Boolean(b) => Value::Bool(*b),
        }
    }

    /// Parses a fetched JSON value into a [`FlagValue`] matching
    /// `self`'s own variant/type. The response `"value"` is read as a
    /// JSON primitive, and for `Number`/`Boolean` flags a string
    /// primitive is also accepted as a fallback if it parses as one.
    /// `None` on any mismatch.
    pub fn parse_matching(&self, json: &Value) -> Option<FlagValue> {
        match self {
            FlagValue::String(_) => json.as_str().map(|s| FlagValue::String(s.to_string())),
            FlagValue::Number(_) => {
                if let Some(n) = json.as_f64() {
                    return Some(FlagValue::Number(n));
                }
                json.as_str()?
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .map(FlagValue::Number)
            }
            FlagValue::Boolean(_) => {
                if let Some(b) = json.as_bool() {
                    return Some(FlagValue::Boolean(b));
                }
                match json.as_str()? {
                    "true" => Some(FlagValue::Boolean(true)),
                    "false" => Some(FlagValue::Boolean(false)),
                    _ => None,
                }
            }
        }
    }

    /// Borrows the string value, if this is a [`FlagValue::String`].
    pub fn as_str(&self) -> Option<&str> {
        match self {
            FlagValue::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// The numeric value, if this is a [`FlagValue::Number`].
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            FlagValue::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// The boolean value, if this is a [`FlagValue::Boolean`].
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            FlagValue::Boolean(b) => Some(*b),
            _ => None,
        }
    }
}

impl From<String> for FlagValue {
    fn from(value: String) -> Self {
        FlagValue::String(value)
    }
}

impl From<&str> for FlagValue {
    fn from(value: &str) -> Self {
        FlagValue::String(value.to_string())
    }
}

impl From<f64> for FlagValue {
    fn from(value: f64) -> Self {
        FlagValue::Number(value)
    }
}

impl From<bool> for FlagValue {
    fn from(value: bool) -> Self {
        FlagValue::Boolean(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_round_trips_through_json() {
        let value = FlagValue::from("hello");
        let json = value.to_json();
        assert_eq!(json, Value::String("hello".to_string()));
        assert_eq!(
            value.parse_matching(&json),
            Some(FlagValue::String("hello".to_string()))
        );
    }

    #[test]
    fn number_round_trips_through_json() {
        let value = FlagValue::from(3.5_f64);
        let json = value.to_json();
        assert_eq!(value.parse_matching(&json), Some(FlagValue::Number(3.5)));
    }

    #[test]
    fn boolean_round_trips_through_json() {
        let value = FlagValue::from(true);
        let json = value.to_json();
        assert_eq!(json, Value::Bool(true));
        assert_eq!(value.parse_matching(&json), Some(FlagValue::Boolean(true)));
    }

    #[test]
    fn parse_matching_rejects_mismatched_type() {
        let string_flag = FlagValue::from("default");
        let number_json = Value::from(42);
        assert_eq!(string_flag.parse_matching(&number_json), None);

        let number_flag = FlagValue::from(1.0_f64);
        let string_json = Value::from("not a number");
        assert_eq!(number_flag.parse_matching(&string_json), None);

        let bool_flag = FlagValue::from(false);
        let string_json = Value::from("not a bool");
        assert_eq!(bool_flag.parse_matching(&string_json), None);
    }

    #[test]
    fn number_parse_matching_accepts_numeric_string() {
        let number_flag = FlagValue::from(1.0_f64);
        assert_eq!(
            number_flag.parse_matching(&Value::from("12.5")),
            Some(FlagValue::Number(12.5))
        );
    }

    #[test]
    fn boolean_parse_matching_accepts_true_false_strings() {
        let bool_flag = FlagValue::from(false);
        assert_eq!(
            bool_flag.parse_matching(&Value::from("true")),
            Some(FlagValue::Boolean(true))
        );
        assert_eq!(
            bool_flag.parse_matching(&Value::from("false")),
            Some(FlagValue::Boolean(false))
        );
        assert_eq!(bool_flag.parse_matching(&Value::from("yes")), None);
    }

    #[test]
    fn accessors_return_none_for_wrong_variant() {
        let value = FlagValue::from("s");
        assert_eq!(value.as_f64(), None);
        assert_eq!(value.as_bool(), None);
        assert_eq!(value.as_str(), Some("s"));
    }
}
