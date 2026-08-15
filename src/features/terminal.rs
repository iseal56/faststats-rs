//! TUI-specific internal metrics, enabled via the `terminal` cargo
//! feature: terminal size (rows x columns) and a best-effort detected
//! terminal emulator name. Zero `crossterm` dependency is pulled in
//! unless this feature is enabled.

use crossterm::terminal;
use serde_json::{Map, Value, json};
use std::env;

/// Appends `terminal_columns`/`terminal_rows`/`terminal_emulator` into
/// `metrics`. Fields are omitted, not placeholder-filled, when their
/// value can't be determined (e.g. stdout isn't a TTY).
pub fn append_terminal_data(metrics: &mut Map<String, Value>) {
    if let Some((columns, rows)) = terminal_size() {
        metrics.insert("terminal_columns".to_string(), json!(columns));
        metrics.insert("terminal_rows".to_string(), json!(rows));
    }
    if let Some(emulator) = detected_terminal_emulator() {
        metrics.insert("terminal_emulator".to_string(), json!(emulator));
    }
}

/// Queries the current terminal size. Returns `None`
/// if the query fails (e.g. stdout isn't a TTY).
fn terminal_size() -> Option<(u16, u16)> {
    terminal::size().ok()
}

/// Best-effort terminal emulator detection: `TERM_PROGRAM`, falling
/// back to `COLORTERM`, then `TERM`. `None` if none are set.
fn detected_terminal_emulator() -> Option<String> {
    env::var("TERM_PROGRAM")
        .ok()
        .or_else(|| env::var("COLORTERM").ok())
        .or_else(|| env::var("TERM").ok())
        .filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::{remove_var, set_var};
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn detected_terminal_emulator_prefers_term_program() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: `_guard` serializes all env-mutating tests in this module.
        unsafe {
            set_var("TERM_PROGRAM", "iTerm.app");
            set_var("COLORTERM", "truecolor");
            set_var("TERM", "xterm-256color");
        }

        assert_eq!(detected_terminal_emulator(), Some("iTerm.app".to_string()));

        unsafe {
            remove_var("TERM_PROGRAM");
            remove_var("COLORTERM");
            remove_var("TERM");
        }
    }

    #[test]
    fn detected_terminal_emulator_falls_back_to_colorterm() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: see `detected_terminal_emulator_prefers_term_program`.
        unsafe {
            remove_var("TERM_PROGRAM");
            set_var("COLORTERM", "truecolor");
            remove_var("TERM");
        }

        assert_eq!(detected_terminal_emulator(), Some("truecolor".to_string()));

        unsafe { remove_var("COLORTERM") };
    }

    #[test]
    fn detected_terminal_emulator_falls_back_to_term() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: see `detected_terminal_emulator_prefers_term_program`.
        unsafe {
            remove_var("TERM_PROGRAM");
            remove_var("COLORTERM");
            set_var("TERM", "xterm-256color");
        }

        assert_eq!(
            detected_terminal_emulator(),
            Some("xterm-256color".to_string())
        );

        unsafe { remove_var("TERM") };
    }

    #[test]
    fn detected_terminal_emulator_none_when_all_unset() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: see `detected_terminal_emulator_prefers_term_program`.
        unsafe {
            remove_var("TERM_PROGRAM");
            remove_var("COLORTERM");
            remove_var("TERM");
        }

        assert_eq!(detected_terminal_emulator(), None);
    }

    #[test]
    fn detected_terminal_emulator_none_when_blank() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: see `detected_terminal_emulator_prefers_term_program`.
        unsafe {
            set_var("TERM_PROGRAM", "   ");
            remove_var("COLORTERM");
            remove_var("TERM");
        }

        assert_eq!(detected_terminal_emulator(), None);

        unsafe { remove_var("TERM_PROGRAM") };
    }
}
