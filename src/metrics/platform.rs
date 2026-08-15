//! Built-in internal metrics reporting facts about the Rust/native
//! platform the binary is running on.

use os_info::Bitness;
use serde_json::{Map, Value, json};
use std::env::consts;
use std::thread::available_parallelism;

/// Appends the built-in platform metrics into `metrics`. Always called,
/// regardless of `Config::additional_metrics`.
pub fn append_internal_data(metrics: &mut Map<String, Value>, program_version: impl Into<String>) {
    let info = os_info::get();
    metrics.insert("os_name".to_string(), json!(info.os_type().to_string()));
    metrics.insert(
        "os_arch".to_string(),
        json!(info.architecture().unwrap_or(consts::ARCH).to_string()),
    );
    metrics.insert("os_version".to_string(), json!(info.version().to_string()));
    metrics.insert(
        "pointer_width".to_string(),
        json!(if info.bitness() == Bitness::X32 {
            32
        } else {
            64
        }),
    );
    metrics.insert("core_count".to_string(), json!(cpu_count()));
    metrics.insert(
        "debug_assertions".to_string(),
        json!(cfg!(debug_assertions)),
    );
    metrics.insert("program_version".to_string(), json!(program_version.into()));
}

/// The number of logical CPUs available to this process. Falls back to `1`.
fn cpu_count() -> usize {
    available_parallelism().map(|n| n.get()).unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use os_info::Type::Arch;

    #[test]
    fn appends_all_expected_keys() {
        let mut metrics = Map::new();
        append_internal_data(&mut metrics, "1.0.0");
        assert!(metrics.contains_key("os_name"));
        assert!(metrics.contains_key("os_arch"));
        assert!(metrics.contains_key("os_version"));
        assert!(metrics.contains_key("pointer_width"));
        assert!(metrics.contains_key("core_count"));
        assert!(metrics.contains_key("debug_assertions"));
        assert!(metrics.contains_key("program_version"));
        assert_eq!(metrics.len(), 7);
    }

    #[test]
    fn os_and_arch_match_env_consts() {
        let mut metrics = Map::new();
        append_internal_data(&mut metrics, "1.0.0");
        assert_eq!(metrics["os_name"], os_info::get().os_type().to_string());
        assert_eq!(metrics["os_arch"], os_info::get().architecture().unwrap());
    }

    #[test]
    fn cpu_count_is_at_least_one() {
        assert!(cpu_count() >= 1);
    }
}
