//! `TrackedError` and the in-memory dedup/ignore-rule tracking table.
//! Errors are deduplicated by `(type, message, stack, handled,
//! attributes, causes)` identity, incrementing a count instead of being
//! recorded twice, and can be filtered out via developer-registered
//! ignore rules.

use std::collections::HashMap;

use regex::Regex;
use serde_json::{Map, Value};

use crate::domain::Attributes;

/// One entry in an error's cause chain (i.e. one `source()` in the
/// chain beyond the top-level error itself), already anonymized and
/// stack-collapsed the same way as the top-level error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CauseFrame {
    pub(crate) error_type: String,
    pub(crate) message: Option<String>,
    pub(crate) stack: Vec<String>,
}

/// A single tracked error occurrence, ready for submission. `handled`
/// is `false` for panics captured automatically via the panic hook.
#[derive(Debug, Clone)]
pub struct TrackedError {
    pub(crate) error_type: String,
    pub(crate) message: Option<String>,
    pub(crate) stack: Vec<String>,
    pub(crate) handled: bool,
    pub(crate) context: Option<Attributes>,
    pub(crate) causes: Vec<CauseFrame>,
    pub(crate) count: u32,
}

impl TrackedError {
    /// The dedup identity key: errors with the same type, message,
    /// (post-collapse) stack, `handled` flag, attributes, and cause
    /// chain are the same tracked error.
    /// Built as a stable JSON string rather than a `HashMap`-native key
    /// tuple, since `Attributes`/`serde_json::Value` aren't `Hash`.
    fn identity(&self) -> String {
        let context_json = self
            .context
            .as_ref()
            .map(|c| Value::Object(c.to_json_map()))
            .unwrap_or(Value::Null);
        let causes_json: Vec<Value> = self
            .causes
            .iter()
            .map(|c| {
                serde_json::json!({
                    "error_type": c.error_type,
                    "message": c.message,
                    "stack": c.stack,
                })
            })
            .collect();
        serde_json::json!({
            "error_type": self.error_type,
            "message": self.message,
            "stack": self.stack,
            "handled": self.handled,
            "context": context_json,
            "causes": causes_json,
        })
        .to_string()
    }

    /// Serializes this tracked error into its wire representation. The
    /// cause chain is appended into the same `stack` array as
    /// `"Caused by: <Type>: <message>"` headers followed by their own
    /// frames, flattening the whole chain into one stacktrace JSON array.
    pub(crate) fn to_json(&self) -> Value {
        let mut object = Map::new();
        object.insert("error".to_string(), Value::from(self.error_type.clone()));
        if let Some(message) = &self.message {
            object.insert("message".to_string(), Value::from(message.clone()));
        }

        let mut stack: Vec<Value> = self.stack.iter().cloned().map(Value::from).collect();
        for cause in &self.causes {
            let header = match &cause.message {
                Some(message) => format!("Caused by: {}: {message}", cause.error_type),
                None => format!("Caused by: {}", cause.error_type),
            };
            stack.push(Value::from(header));
            stack.extend(cause.stack.iter().cloned().map(Value::from));
        }
        object.insert("stack".to_string(), Value::from(stack));

        object.insert("handled".to_string(), Value::from(self.handled));
        if let Some(context) = &self.context
            && !context.is_empty()
        {
            object.insert("context".to_string(), Value::Object(context.to_json_map()));
        }
        if self.count > 1 {
            object.insert("count".to_string(), Value::from(self.count));
        }
        Value::Object(object)
    }
}

/// A rule that suppresses matching errors from ever being tracked.
pub enum IgnoreRule {
    /// Ignore every error whose type exactly matches.
    Type(String),
    /// Ignore every error whose message matches the pattern (an error
    /// with no message never matches a message-based rule).
    MessagePattern(Regex),
    /// Ignore only errors that match both the type and the pattern.
    TypeAndPattern(String, Regex),
}

impl IgnoreRule {
    fn matches(&self, error_type: &str, message: Option<&str>) -> bool {
        match self {
            IgnoreRule::Type(t) => t == error_type,
            IgnoreRule::MessagePattern(pattern) => message.is_some_and(|m| pattern.is_match(m)),
            IgnoreRule::TypeAndPattern(t, pattern) => {
                t == error_type && message.is_some_and(|m| pattern.is_match(m))
            }
        }
    }
}

/// The in-memory tracking table: pending deduplicated errors plus the
/// ignore rules that filter incoming reports before they're stored.
#[derive(Default)]
pub struct Tracker {
    ignore_rules: Vec<IgnoreRule>,
    pending: HashMap<String, TrackedError>,
    /// Insertion order of dedup keys, for deterministic submission order.
    order: Vec<String>,
}

impl Tracker {
    pub fn new() -> Self {
        Tracker::default()
    }

    /// Registers an ignore rule. Rules are checked in registration
    /// order; the first match suppresses the error.
    pub fn add_ignore_rule(&mut self, rule: IgnoreRule) {
        self.ignore_rules.push(rule);
    }

    /// Whether an error of this identity should be suppressed.
    fn is_ignored(&self, error_type: &str, message: Option<&str>) -> bool {
        self.ignore_rules
            .iter()
            .any(|rule| rule.matches(error_type, message))
    }

    /// Records an already-normalized error, incrementing the count if
    /// its identity is already pending. Returns `false` if an ignore
    /// rule matches instead of storing it.
    pub fn track(&mut self, error: TrackedError) -> bool {
        if self.is_ignored(&error.error_type, error.message.as_deref()) {
            return false;
        }

        let key = error.identity();
        match self.pending.get_mut(&key) {
            Some(existing) => {
                existing.count += 1;
            }
            None => {
                self.order.push(key.clone());
                self.pending.insert(key, error);
            }
        }
        true
    }

    /// Whether there are zero pending error reports.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Drains all pending tracked errors, in the order they were first
    /// seen, resetting the table for the next submission window.
    pub fn drain(&mut self) -> Vec<TrackedError> {
        let order = std::mem::take(&mut self.order);
        let mut pending = std::mem::take(&mut self.pending);
        order
            .into_iter()
            .filter_map(|key| pending.remove(&key))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(error_type: &str, message: Option<&str>, stack: &[&str]) -> TrackedError {
        TrackedError {
            error_type: error_type.to_string(),
            message: message.map(|m| m.to_string()),
            stack: stack.iter().map(|s| s.to_string()).collect(),
            handled: true,
            context: None,
            causes: Vec::new(),
            count: 1,
        }
    }

    #[test]
    fn tracks_new_error() {
        let mut tracker = Tracker::new();
        assert!(tracker.track(error("PanicError", Some("boom"), &["a", "b"])));
        assert!(!tracker.is_empty());
    }

    #[test]
    fn duplicate_identity_increments_count_instead_of_duplicating() {
        let mut tracker = Tracker::new();
        tracker.track(error("PanicError", Some("boom"), &["a", "b"]));
        tracker.track(error("PanicError", Some("boom"), &["a", "b"]));
        tracker.track(error("PanicError", Some("boom"), &["a", "b"]));

        let drained = tracker.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].count, 3);
    }

    #[test]
    fn different_message_is_a_different_identity() {
        let mut tracker = Tracker::new();
        tracker.track(error("PanicError", Some("boom"), &["a"]));
        tracker.track(error("PanicError", Some("bang"), &["a"]));

        let drained = tracker.drain();
        assert_eq!(drained.len(), 2);
    }

    #[test]
    fn different_stack_is_a_different_identity() {
        let mut tracker = Tracker::new();
        tracker.track(error("PanicError", Some("boom"), &["a"]));
        tracker.track(error("PanicError", Some("boom"), &["b"]));

        let drained = tracker.drain();
        assert_eq!(drained.len(), 2);
    }

    #[test]
    fn drain_empties_the_tracker() {
        let mut tracker = Tracker::new();
        tracker.track(error("PanicError", None, &[]));
        tracker.drain();
        assert!(tracker.is_empty());
    }

    #[test]
    fn drain_preserves_first_seen_order() {
        let mut tracker = Tracker::new();
        tracker.track(error("First", None, &[]));
        tracker.track(error("Second", None, &[]));
        tracker.track(error("Third", None, &[]));

        let drained = tracker.drain();
        let types: Vec<&str> = drained.iter().map(|e| e.error_type.as_str()).collect();
        assert_eq!(types, vec!["First", "Second", "Third"]);
    }

    #[test]
    fn ignore_rule_by_type_suppresses_matching_errors() {
        let mut tracker = Tracker::new();
        tracker.add_ignore_rule(IgnoreRule::Type("NoisyError".to_string()));

        assert!(!tracker.track(error("NoisyError", Some("anything"), &[])));
        assert!(tracker.is_empty());
    }

    #[test]
    fn ignore_rule_by_message_pattern_suppresses_matching_errors() {
        let mut tracker = Tracker::new();
        tracker.add_ignore_rule(IgnoreRule::MessagePattern(
            Regex::new(r"connection reset").unwrap(),
        ));

        assert!(!tracker.track(error("IoError", Some("connection reset by peer"), &[])));
        assert!(tracker.track(error("IoError", Some("disk full"), &[])));
    }

    #[test]
    fn ignore_rule_by_type_and_pattern_requires_both() {
        let mut tracker = Tracker::new();
        tracker.add_ignore_rule(IgnoreRule::TypeAndPattern(
            "IoError".to_string(),
            Regex::new(r"timeout").unwrap(),
        ));

        assert!(tracker.track(error("IoError", Some("disk full"), &[])));
        assert!(tracker.track(error("NetError", Some("timeout occurred"), &[])));
        assert!(!tracker.track(error("IoError", Some("timeout occurred"), &[])));
    }

    #[test]
    fn message_pattern_rule_never_matches_messageless_error() {
        let mut tracker = Tracker::new();
        tracker.add_ignore_rule(IgnoreRule::MessagePattern(Regex::new(r".*").unwrap()));
        assert!(tracker.track(error("PanicError", None, &[])));
    }

    #[test]
    fn to_json_includes_count_only_when_greater_than_one() {
        let mut single = error("E", Some("m"), &["f"]);
        single.count = 1;
        assert!(single.to_json().get("count").is_none());

        let mut repeated = error("E", Some("m"), &["f"]);
        repeated.count = 5;
        assert_eq!(repeated.to_json()["count"], 5);
    }

    #[test]
    fn to_json_omits_message_when_absent() {
        let error = error("E", None, &["f"]);
        assert!(error.to_json().get("message").is_none());
    }
}
