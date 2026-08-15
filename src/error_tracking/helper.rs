//! Anonymization and stack-trace normalization for error tracking:
//! default regex-based anonymization patterns, OS-username scrubbing,
//! and stack-trace collapsing/length limits.

use std::env;
use std::sync::OnceLock;

use regex::Regex;

/// Maximum number of stack frames kept per error (extra frames are
/// dropped from the middle).
pub const MAX_STACK_SIZE: usize = 30;
/// Maximum characters kept per stack frame (truncated with `...`).
pub const MAX_FRAME_SIZE: usize = 300;
/// Maximum characters kept for an error message (truncated with `...`).
pub const MAX_MESSAGE_LENGTH: usize = 1000;

/// OS usernames that are generic enough not to need anonymizing
const ALLOWED_USERNAMES: &[&str] = &["root", "ubuntu", "server", "ec2-user", "admin", "user"];

/// One anonymization pattern: a compiled regex plus its replacement.
struct Pattern {
    regex: Regex,
    replacement: &'static str,
}

fn ipv4_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\b(?:(?:25[0-5]|2[0-4]\d|1?\d?\d)\.){3}(?:25[0-5]|2[0-4]\d|1?\d?\d)\b")
            .expect("valid ipv4 pattern")
    })
}

fn ipv6_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(^|[^0-9a-fA-F:])(?:[0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}($|[^0-9a-fA-F:])")
            .expect("valid ipv6 pattern")
    })
}

/// Redacts IPv6 addresses in `text`, replacing each match with
/// `[ipv6]` while preserving the single character of surrounding
/// context that `ipv6_pattern` had to capture in order to anchor the
/// match without `\b` (see the comment on `ipv6_pattern`).
fn redact_ipv6(text: &str) -> String {
    ipv6_pattern()
        .replace_all(text, "${1}[ipv6]${2}")
        .into_owned()
}

fn home_path_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Capture the OS-specific prefix separately from the username so
        // the prefix can be kept in the replacement.
        Regex::new(r"(/home/|/Users/|C:\\Users\\)[^/\\\s]+").expect("valid home-path pattern")
    })
}

fn discord_webhook_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"https://discord(?:app)?\.com/api/webhooks/\d+/[\w-]+")
            .expect("valid discord webhook pattern")
    })
}

/// Matches a `password=` query-param/attribute anywhere in the text
fn password_attribute_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)(password=)[^&;\s]+").expect("valid password-attribute pattern")
    })
}

/// Redacts a `password=` query-param/attribute value anywhere in the
/// text, keeping the `password=` key intact.
fn redact_password_attribute(text: &str) -> String {
    password_attribute_pattern()
        .replace_all(text, "${1}[password]")
        .into_owned()
}

/// Returns the default anonymization patterns, always applied in
/// addition to any user-supplied ones.
fn default_patterns() -> Vec<Pattern> {
    vec![
        // ipv6 is handled separately by `redact_ipv6` (see `anonymize`
        // below): its pattern has to capture a character of surrounding
        // context to work around the `regex` crate's lack of lookaround,
        // which doesn't fit the flat literal-replacement `Pattern` shape
        // used here.
        Pattern {
            regex: ipv4_pattern().clone(),
            replacement: "[ipv4]",
        },
        Pattern {
            regex: home_path_pattern().clone(),
            replacement: "${1}[home]",
        },
        Pattern {
            regex: discord_webhook_pattern().clone(),
            replacement: "[discord_webhook]",
        },
    ]
}

/// Applies default anonymization patterns first, then any user-supplied
/// `extra_patterns`, then username scrubbing, to `text`.
pub fn anonymize(text: &str, extra_patterns: &[(Regex, &str)]) -> String {
    let mut result = redact_ipv6(text);

    for pattern in default_patterns() {
        result = pattern.regex.replace_all(&result, pattern.replacement).into_owned();
    }
    result = redact_password_attribute(&result);

    for (regex, replacement) in extra_patterns {
        result = regex.replace_all(&result, *replacement).into_owned();
    }

    result = anonymize_username(&result);
    result
}

/// Replaces occurrences of the current OS username with `[user]`,
/// unless the username is on the generic allow-list. Case-insensitive,
/// so a username appearing in a different letter-casing is still
/// redacted. Best-effort no-op if the username can't be determined.
fn anonymize_username(text: &str) -> String {
    let username = match current_username() {
        Some(u) if !u.is_empty() => u,
        _ => return text.to_string(),
    };
    if ALLOWED_USERNAMES
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(&username))
    {
        return text.to_string();
    }

    let escaped = regex::escape(&username);
    let pattern = Regex::new(&format!("(?i){escaped}")).expect("valid escaped username pattern");
    pattern.replace_all(text, "[user]").into_owned()
}

/// Best-effort current OS username, via the platform-conventional
/// environment variables. Returns `None` if undetermined.
fn current_username() -> Option<String> {
    env::var("USER")
        .ok()
        .or_else(|| env::var("USERNAME").ok())
        .filter(|v| !v.trim().is_empty())
}

/// Truncates `message` to [`MAX_MESSAGE_LENGTH`] characters
pub fn truncate_message(message: &str) -> String {
    truncate_chars(message, MAX_MESSAGE_LENGTH)
}

/// Truncates a single stack frame to [`MAX_FRAME_SIZE`] characters.
fn truncate_frame(frame: &str) -> String {
    truncate_chars(frame, MAX_FRAME_SIZE)
}

fn truncate_chars(value: &str, max_len: usize) -> String {
    if value.chars().count() <= max_len {
        return value.to_string();
    }
    let truncated: String = value.chars().take(max_len.saturating_sub(3)).collect();
    format!("{truncated}...")
}

/// Normalizes a raw stack trace: collapses consecutive duplicate
/// frames, then detects and collapses repeating cycles, then truncates
/// each frame and caps the total frame count.
pub fn collapse_stack(frames: &[String]) -> Vec<String> {
    let deduped = collapse_consecutive_duplicates(frames);
    let collapsed = collapse_repeating_pattern(&deduped);
    let truncated: Vec<String> = collapsed.iter().map(|f| truncate_frame(f)).collect();

    if truncated.len() <= MAX_STACK_SIZE {
        truncated
    } else {
        let mut kept: Vec<String> = truncated.into_iter().take(MAX_STACK_SIZE).collect();
        kept.push(format!("... ({} more)", frames.len() - MAX_STACK_SIZE));
        kept
    }
}

/// Collapses immediately-repeated identical frames into a single
/// annotated entry, e.g. `["a", "a", "a", "b"]` ->
/// `["a (x3)", "b"]`.
fn collapse_consecutive_duplicates(frames: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < frames.len() {
        let current = &frames[i];
        let mut count = 1;
        while i + count < frames.len() && &frames[i + count] == current {
            count += 1;
        }
        if count > 1 {
            result.push(format!("{current} (x{count})"));
        } else {
            result.push(current.clone());
        }
        i += count;
    }
    result
}

/// Detects a repeating cycle of frames (e.g. recursive calls) and
/// collapses each full repetition after the first into a single
/// annotated marker. Requires at least two full repetitions to avoid
/// misidentifying a coincidental short repeat.
fn collapse_repeating_pattern(frames: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < frames.len() {
        let mut collapsed = false;
        let max_cycle_len = (frames.len() - i) / 2;
        for cycle_len in 1..=max_cycle_len {
            let candidate = &frames[i..i + cycle_len];
            let mut repeats = 1;
            while i + (repeats + 1) * cycle_len <= frames.len()
                && &frames[i + repeats * cycle_len..i + (repeats + 1) * cycle_len] == candidate
            {
                repeats += 1;
            }
            if repeats >= 2 {
                result.extend_from_slice(candidate);
                result.push(format!("... (repeated {repeats} times)"));
                i += repeats * cycle_len;
                collapsed = true;
                break;
            }
        }
        if !collapsed {
            result.push(frames[i].clone());
            i += 1;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anonymizes_ipv4() {
        let result = anonymize("connect to 192.168.1.10 now", &[]);
        assert_eq!(result, "connect to [ipv4] now");
    }

    #[test]
    fn anonymizes_ipv6() {
        let result = anonymize("host 2001:db8:85a3:0:0:8a2e:370:7334 unreachable", &[]);
        assert!(result.contains("[ipv6]"));
        assert!(!result.contains("2001:db8"));
    }

    #[test]
    fn does_not_anonymize_elided_ipv6() {
        let result = anonymize("host 2001:db8::1 unreachable", &[]);
        assert!(!result.contains("[ipv6]"));
        assert!(result.contains("2001:db8::1"));

        let loopback = anonymize("bind ::1 failed", &[]);
        assert!(!loopback.contains("[ipv6]"));
        assert!(loopback.contains("::1"));
    }

    #[test]
    fn anonymizes_unix_home_path() {
        let result = anonymize("failed to read /home/alice/config.toml", &[]);
        assert_eq!(result, "failed to read /home/[home]/config.toml");
    }

    #[test]
    fn anonymizes_macos_home_path() {
        let result = anonymize("failed to read /Users/alice/config.toml", &[]);
        assert_eq!(result, "failed to read /Users/[home]/config.toml");
    }

    #[test]
    fn anonymizes_windows_home_path() {
        let result = anonymize(r"failed to read C:\Users\alice\config.toml", &[]);
        assert_eq!(result, r"failed to read C:\Users\[home]\config.toml");
    }

    #[test]
    fn anonymizes_discord_webhook() {
        let text = "webhook: https://discord.com/api/webhooks/123456789/abcDEF-123_xyz";
        let result = anonymize(text, &[]);
        assert_eq!(result, "webhook: [discord_webhook]");
    }

    #[test]
    fn anonymizes_password_query_param() {
        let text = "jdbc:mysql://host/db?user=admin&password=hunter2&ssl=true";
        let result = anonymize(text, &[]);
        assert!(result.contains("[password]"));
        assert!(!result.contains("hunter2"));
        assert!(result.contains("user=admin"));
    }

    #[test]
    fn anonymizes_username_case_insensitively() {
        // SAFETY: test-only env mutation; no other thread in this test
        // binary reads/writes `USER` concurrently with this test.
        unsafe {
            env::set_var("USER", "alice");
        }
        let result = anonymize("path was /srv/ALICE/data", &[]);
        // SAFETY: see above.
        unsafe {
            env::remove_var("USER");
        }
        assert!(result.contains("[user]"));
        assert!(!result.to_lowercase().contains("alice"));
    }

    #[test]
    fn applies_user_supplied_patterns() {
        let custom = vec![(Regex::new(r"secret-\d+").unwrap(), "[secret]")];
        let result = anonymize("token secret-42 leaked", &custom);
        assert_eq!(result, "token [secret] leaked");
    }

    #[test]
    fn default_patterns_apply_before_custom_ones() {
        let custom = vec![(Regex::new(r"192\.168\.1\.10").unwrap(), "[should-not-match]")];
        let result = anonymize("ip 192.168.1.10 seen", &custom);
        assert_eq!(result, "ip [ipv4] seen");
    }

    #[test]
    fn truncate_message_leaves_short_message_untouched() {
        assert_eq!(truncate_message("short"), "short");
    }

    #[test]
    fn truncate_message_truncates_long_message() {
        let long = "a".repeat(MAX_MESSAGE_LENGTH + 50);
        let truncated = truncate_message(&long);
        assert_eq!(truncated.chars().count(), MAX_MESSAGE_LENGTH);
        assert!(truncated.ends_with("..."));
    }

    #[test]
    fn collapse_stack_deduplicates_consecutive_frames() {
        let frames = vec!["a".to_string(), "a".to_string(), "a".to_string(), "b".to_string()];
        let collapsed = collapse_stack(&frames);
        assert_eq!(collapsed, vec!["a (x3)".to_string(), "b".to_string()]);
    }

    #[test]
    fn collapse_stack_detects_repeating_cycle() {
        let frames = vec![
            "a".to_string(),
            "b".to_string(),
            "a".to_string(),
            "b".to_string(),
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
        ];
        let collapsed = collapse_stack(&frames);
        assert_eq!(
            collapsed,
            vec![
                "a".to_string(),
                "b".to_string(),
                "... (repeated 3 times)".to_string(),
                "c".to_string(),
            ]
        );
    }

    #[test]
    fn collapse_stack_truncates_long_frames() {
        let long_frame = "x".repeat(MAX_FRAME_SIZE + 20);
        let frames = vec![long_frame];
        let collapsed = collapse_stack(&frames);
        assert_eq!(collapsed[0].chars().count(), MAX_FRAME_SIZE);
        assert!(collapsed[0].ends_with("..."));
    }

    #[test]
    fn collapse_stack_caps_total_frame_count() {
        let frames: Vec<String> = (0..(MAX_STACK_SIZE + 10)).map(|i| format!("frame_{i}")).collect();
        let collapsed = collapse_stack(&frames);
        assert_eq!(collapsed.len(), MAX_STACK_SIZE + 1);
        assert!(collapsed.last().unwrap().contains("more"));
    }

    #[test]
    fn collapse_stack_leaves_short_unique_stack_untouched() {
        let frames = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let collapsed = collapse_stack(&frames);
        assert_eq!(collapsed, frames);
    }
}