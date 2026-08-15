//! Mutable key-value attributes for feature flag targeting and error
//! context.

use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;

use crate::error::{Error, Result};

/// A mutable bag of key-value attributes, accepting any [`Serialize`]
/// value (including nested structs, maps, and vectors).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Attributes {
    entries: HashMap<String, Value>,
}

impl Attributes {
    /// Creates new, empty attributes.
    pub fn empty() -> Self {
        Attributes {
            entries: HashMap::new(),
        }
    }

    /// Creates new attributes by copying entries from `other`.
    pub fn copy_of(other: &Attributes) -> Self {
        Attributes {
            entries: other.entries.clone(),
        }
    }

    /// Sets a value for `key`, converting it to JSON.
    /// Rejects `NaN`/ `Infinity` floats anywhere in the value with an error
    /// A value that serializes to JSON `null` is treated as absent and simply
    /// not stored (removing any existing entry for `key`).
    pub fn put<T: Serialize>(&mut self, key: impl Into<String>, value: T) -> Result<&mut Self> {
        value
            .serialize(NonFiniteFloatCheck)
            .map_err(|e| Error::validation("attribute value", e.0))?;
        let json = serde_json::to_value(value)?;
        let key = key.into();
        if json.is_null() {
            self.entries.remove(&key);
        } else {
            self.entries.insert(key, json);
        }
        Ok(self)
    }

    /// Removes the value for `key`, if present.
    pub fn remove(&mut self, key: &str) -> &mut Self {
        self.entries.remove(key);
        self
    }

    /// Returns whether a value is set for `key`.
    pub fn contains_key(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// Gets the JSON value stored for `key`, if any.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries.get(key)
    }

    /// Visits each stored attribute as its underlying JSON value.
    pub fn for_each(&self, mut action: impl FnMut(&str, &Value)) {
        for (key, value) in &self.entries {
            action(key, value);
        }
    }

    /// The number of stored attributes.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are no stored attributes.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Merges two attribute sets; on conflict, `second` wins.
    pub fn join(first: Option<&Attributes>, second: Option<&Attributes>) -> Attributes {
        let mut entries = HashMap::new();
        if let Some(attrs) = first {
            entries.extend(attrs.entries.clone());
        }
        if let Some(attrs) = second {
            entries.extend(attrs.entries.clone());
        }
        Attributes { entries }
    }

    /// Consumes these attributes, returning the underlying JSON object.
    pub fn into_json_map(self) -> serde_json::Map<String, Value> {
        self.entries.into_iter().collect()
    }

    /// Borrows the underlying entries as a JSON object.
    pub fn to_json_map(&self) -> serde_json::Map<String, Value> {
        self.entries
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

impl Serialize for Attributes {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.entries.serialize(serializer)
    }
}

/// A [`serde::Serializer`] that visits every field of a value purely to
/// check for `NaN`/infinite floats, without building any output. Used as
/// a pre-check before `serde_json::to_value`, which would otherwise
/// silently turn such floats into `null` instead of erroring.
pub(crate) struct NonFiniteFloatCheck;

/// The error used by [`NonFiniteFloatCheck`], carrying just a message.
#[derive(Debug)]
pub(crate) struct NonFiniteFloatError(pub(crate) String);

impl std::fmt::Display for NonFiniteFloatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NonFiniteFloatError {}

impl serde::ser::Error for NonFiniteFloatError {
    fn custom<T: std::fmt::Display>(msg: T) -> Self {
        NonFiniteFloatError(msg.to_string())
    }
}

impl serde::Serializer for NonFiniteFloatCheck {
    type Ok = ();
    type Error = NonFiniteFloatError;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    fn serialize_bool(self, _v: bool) -> std::result::Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_i8(self, _v: i8) -> std::result::Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_i16(self, _v: i16) -> std::result::Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_i32(self, _v: i32) -> std::result::Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_i64(self, _v: i64) -> std::result::Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_u8(self, _v: u8) -> std::result::Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_u16(self, _v: u16) -> std::result::Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_u32(self, _v: u32) -> std::result::Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_u64(self, _v: u64) -> std::result::Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_f32(self, v: f32) -> std::result::Result<Self::Ok, Self::Error> {
        if v.is_finite() {
            Ok(())
        } else {
            Err(NonFiniteFloatError("float is NaN or infinite".to_string()))
        }
    }

    fn serialize_f64(self, v: f64) -> std::result::Result<Self::Ok, Self::Error> {
        if v.is_finite() {
            Ok(())
        } else {
            Err(NonFiniteFloatError("float is NaN or infinite".to_string()))
        }
    }

    fn serialize_char(self, _v: char) -> std::result::Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_str(self, _v: &str) -> std::result::Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_bytes(self, _v: &[u8]) -> std::result::Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_none(self) -> std::result::Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_some<T: ?Sized + Serialize>(
        self,
        value: &T,
    ) -> std::result::Result<Self::Ok, Self::Error> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> std::result::Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_unit_struct(
        self,
        _name: &'static str,
    ) -> std::result::Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
    ) -> std::result::Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> std::result::Result<Self::Ok, Self::Error> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        value: &T,
    ) -> std::result::Result<Self::Ok, Self::Error> {
        value.serialize(self)
    }

    fn serialize_seq(
        self,
        _len: Option<usize>,
    ) -> std::result::Result<Self::SerializeSeq, Self::Error> {
        Ok(self)
    }

    fn serialize_tuple(
        self,
        _len: usize,
    ) -> std::result::Result<Self::SerializeTuple, Self::Error> {
        Ok(self)
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> std::result::Result<Self::SerializeTupleStruct, Self::Error> {
        Ok(self)
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> std::result::Result<Self::SerializeTupleVariant, Self::Error> {
        Ok(self)
    }

    fn serialize_map(
        self,
        _len: Option<usize>,
    ) -> std::result::Result<Self::SerializeMap, Self::Error> {
        Ok(self)
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> std::result::Result<Self::SerializeStruct, Self::Error> {
        Ok(self)
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> std::result::Result<Self::SerializeStructVariant, Self::Error> {
        Ok(self)
    }
}

impl serde::ser::SerializeSeq for NonFiniteFloatCheck {
    type Ok = ();
    type Error = NonFiniteFloatError;

    fn serialize_element<T: ?Sized + Serialize>(
        &mut self,
        value: &T,
    ) -> std::result::Result<(), Self::Error> {
        value.serialize(NonFiniteFloatCheck)
    }

    fn end(self) -> std::result::Result<Self::Ok, Self::Error> {
        Ok(())
    }
}

impl serde::ser::SerializeTuple for NonFiniteFloatCheck {
    type Ok = ();
    type Error = NonFiniteFloatError;

    fn serialize_element<T: ?Sized + Serialize>(
        &mut self,
        value: &T,
    ) -> std::result::Result<(), Self::Error> {
        value.serialize(NonFiniteFloatCheck)
    }

    fn end(self) -> std::result::Result<Self::Ok, Self::Error> {
        Ok(())
    }
}

impl serde::ser::SerializeTupleStruct for NonFiniteFloatCheck {
    type Ok = ();
    type Error = NonFiniteFloatError;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        value: &T,
    ) -> std::result::Result<(), Self::Error> {
        value.serialize(NonFiniteFloatCheck)
    }

    fn end(self) -> std::result::Result<Self::Ok, Self::Error> {
        Ok(())
    }
}

impl serde::ser::SerializeTupleVariant for NonFiniteFloatCheck {
    type Ok = ();
    type Error = NonFiniteFloatError;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        value: &T,
    ) -> std::result::Result<(), Self::Error> {
        value.serialize(NonFiniteFloatCheck)
    }

    fn end(self) -> std::result::Result<Self::Ok, Self::Error> {
        Ok(())
    }
}

impl serde::ser::SerializeMap for NonFiniteFloatCheck {
    type Ok = ();
    type Error = NonFiniteFloatError;

    fn serialize_key<T: ?Sized + Serialize>(
        &mut self,
        key: &T,
    ) -> std::result::Result<(), Self::Error> {
        key.serialize(NonFiniteFloatCheck)
    }

    fn serialize_value<T: ?Sized + Serialize>(
        &mut self,
        value: &T,
    ) -> std::result::Result<(), Self::Error> {
        value.serialize(NonFiniteFloatCheck)
    }

    fn end(self) -> std::result::Result<Self::Ok, Self::Error> {
        Ok(())
    }
}

impl serde::ser::SerializeStruct for NonFiniteFloatCheck {
    type Ok = ();
    type Error = NonFiniteFloatError;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        _key: &'static str,
        value: &T,
    ) -> std::result::Result<(), Self::Error> {
        value.serialize(NonFiniteFloatCheck)
    }

    fn end(self) -> std::result::Result<Self::Ok, Self::Error> {
        Ok(())
    }
}

impl serde::ser::SerializeStructVariant for NonFiniteFloatCheck {
    type Ok = ();
    type Error = NonFiniteFloatError;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        _key: &'static str,
        value: &T,
    ) -> std::result::Result<(), Self::Error> {
        value.serialize(NonFiniteFloatCheck)
    }

    fn end(self) -> std::result::Result<Self::Ok, Self::Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[derive(Serialize)]
    struct ServerInfo {
        region: String,
        shard_count: u32,
        beta: bool,
    }

    #[test]
    fn empty_has_no_entries() {
        let attrs = Attributes::empty();
        assert_eq!(attrs.len(), 0);
        assert!(attrs.is_empty());
    }

    #[test]
    fn put_scalar_values() {
        let mut attrs = Attributes::empty();
        attrs.put("name", "faststats").expect("string value");
        attrs.put("count", 42).expect("number value");
        attrs.put("active", true).expect("bool value");

        assert_eq!(attrs.get("name"), Some(&Value::String("faststats".into())));
        assert_eq!(attrs.get("count"), Some(&Value::from(42)));
        assert_eq!(attrs.get("active"), Some(&Value::Bool(true)));
    }

    #[test]
    fn put_nested_struct_value() {
        let mut attrs = Attributes::empty();
        attrs
            .put(
                "server_info",
                ServerInfo {
                    region: "eu-west".into(),
                    shard_count: 3,
                    beta: false,
                },
            )
            .expect("struct value");

        let expected = serde_json::json!({
            "region": "eu-west",
            "shard_count": 3,
            "beta": false
        });
        assert_eq!(attrs.get("server_info"), Some(&expected));
    }

    #[test]
    fn put_deeply_nested_map_value() {
        let mut attrs = Attributes::empty();
        let value = serde_json::json!({
            "key1": "value1",
            "key2": { "sub-key1": "value2" }
        });
        attrs.put("nested", value.clone()).expect("nested value");
        assert_eq!(attrs.get("nested"), Some(&value));
    }

    #[test]
    fn remove_deletes_entry() {
        let mut attrs = Attributes::empty();
        attrs.put("temp", 1).expect("number value");
        assert!(attrs.contains_key("temp"));
        attrs.remove("temp");
        assert!(!attrs.contains_key("temp"));
    }

    #[test]
    fn copy_of_is_independent() {
        let mut original = Attributes::empty();
        original.put("a", 1).expect("number value");
        let mut copy = Attributes::copy_of(&original);
        copy.put("b", 2).expect("number value");

        assert!(original.contains_key("a"));
        assert!(!original.contains_key("b"));
        assert!(copy.contains_key("a"));
        assert!(copy.contains_key("b"));
    }

    #[test]
    fn join_prefers_second_on_conflict() {
        let mut first = Attributes::empty();
        first.put("shared", "first").expect("string value");
        first.put("only_first", 1).expect("number value");

        let mut second = Attributes::empty();
        second.put("shared", "second").expect("string value");
        second.put("only_second", 2).expect("number value");

        let joined = Attributes::join(Some(&first), Some(&second));
        assert_eq!(joined.get("shared"), Some(&Value::String("second".into())));
        assert_eq!(joined.get("only_first"), Some(&Value::from(1)));
        assert_eq!(joined.get("only_second"), Some(&Value::from(2)));
    }

    #[test]
    fn join_handles_none_arguments() {
        let mut only = Attributes::empty();
        only.put("k", "v").expect("string value");

        let joined_first_none = Attributes::join(None, Some(&only));
        assert!(joined_first_none.contains_key("k"));

        let joined_second_none = Attributes::join(Some(&only), None);
        assert!(joined_second_none.contains_key("k"));

        let joined_both_none = Attributes::join(None, None);
        assert!(joined_both_none.is_empty());
    }

    #[test]
    fn put_rejects_nan_float() {
        let mut attrs = Attributes::empty();
        assert!(attrs.put("bad", f64::NAN).is_err());
        assert!(!attrs.contains_key("bad"));
    }

    #[test]
    fn put_rejects_infinite_float() {
        let mut attrs = Attributes::empty();
        assert!(attrs.put("bad", f64::INFINITY).is_err());
        assert!(attrs.put("bad", f64::NEG_INFINITY).is_err());
    }

    #[test]
    fn put_rejects_non_finite_float_nested_in_struct() {
        #[derive(Serialize)]
        struct Reading {
            value: f64,
        }

        let mut attrs = Attributes::empty();
        let result = attrs.put("reading", Reading { value: f64::NAN });
        assert!(result.is_err());
        assert!(!attrs.contains_key("reading"));
    }

    #[test]
    fn put_null_value_is_not_stored() {
        let mut attrs = Attributes::empty();
        attrs
            .put("absent", Option::<i32>::None)
            .expect("null value");
        assert!(!attrs.contains_key("absent"));
    }

    #[test]
    fn put_null_value_removes_existing_entry() {
        let mut attrs = Attributes::empty();
        attrs.put("k", 1).expect("number value");
        assert!(attrs.contains_key("k"));

        attrs.put("k", Option::<i32>::None).expect("null value");
        assert!(!attrs.contains_key("k"));
    }
}
